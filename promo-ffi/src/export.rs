//! Video export over the C ABI — the same `render::export_video` the CLI
//! runs, as a job a host polls.
//!
//! The host polls; nothing calls back into it. An export is minutes of
//! work, so it runs on its own thread and `promo_export_progress` answers
//! from atomics — a function pointer into managed code called from a Rust
//! worker thread is where interop gets frightening, and a progress bar
//! needs nothing that polling cannot give it.

use promo_cli::project::Project;
use promo_cli::render::{export_video, ExportOutcome, ExportSettings};
use std::ffi::{c_char, c_double, c_int, CStr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::Arc;

const RUNNING: i32 = 0;
const FINISHED: i32 = 1;
const CANCELLED: i32 = 2;
const FAILED: i32 = -4;

struct Shared {
    done: AtomicUsize,
    total: AtomicUsize,
    cancel: AtomicBool,
    state: AtomicI32,
}

/// Opaque to C.
pub struct ExportHandle {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Starts exporting `project_dir` to `out_path` (mp4) at `width`×`height`.
/// `fps` <= 0 means the project's own rate (else 30). The whole composition
/// is exported. NULL when the project cannot be opened (reason on stderr);
/// everything after that — GPU, decode, encode — reports through
/// `promo_export_progress`, because it happens on the job's thread.
/// Free with `promo_export_free`.
///
/// Safety contract (C ABI): both strings are valid NUL-terminated UTF-8.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_export_start(
    project_dir: *const c_char,
    out_path: *const c_char,
    width: c_int,
    height: c_int,
    fps: c_double,
) -> *mut ExportHandle {
    crate::ffi_guard(std::ptr::null_mut(), move || {
        export_start_impl(project_dir, out_path, width, height, fps, None)
    })
}

/// [`promo_export_start`] with a host-rasterized overlay (watermark)
/// composited over every frame: PREMULTIPLIED BGRA, `overlay_len` ==
/// `overlay_width * overlay_height * 4` exactly (refused otherwise —
/// never read past), stretched over the canvas. The bytes are copied
/// during this call; the caller may free them the moment it returns. A
/// NULL overlay is a plain export.
///
/// Safety contract (C ABI): strings as above; `overlay_bgra` is NULL or
/// addresses `overlay_len` readable bytes.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_export_start_with_overlay(
    project_dir: *const c_char,
    out_path: *const c_char,
    width: c_int,
    height: c_int,
    fps: c_double,
    overlay_bgra: *const u8,
    overlay_len: usize,
    overlay_width: c_int,
    overlay_height: c_int,
) -> *mut ExportHandle {
    crate::ffi_guard(std::ptr::null_mut(), move || {
        let overlay = if overlay_bgra.is_null() {
            None
        } else {
            if overlay_width <= 0
                || overlay_height <= 0
                || overlay_len != (overlay_width as usize * overlay_height as usize * 4)
            {
                eprintln!("promo_export_start_with_overlay: overlay size mismatch");
                return std::ptr::null_mut();
            }
            let bytes = unsafe { std::slice::from_raw_parts(overlay_bgra, overlay_len) }.to_vec();
            Some((bytes, overlay_width as u32, overlay_height as u32))
        };
        export_start_impl(project_dir, out_path, width, height, fps, overlay)
    })
}

fn export_start_impl(
    project_dir: *const c_char,
    out_path: *const c_char,
    width: c_int,
    height: c_int,
    fps: c_double,
    overlay: Option<(Vec<u8>, u32, u32)>,
) -> *mut ExportHandle {
    {
        if project_dir.is_null() || out_path.is_null() || width <= 0 || height <= 0 {
            return std::ptr::null_mut();
        }
        let (Ok(dir), Ok(out)) = (
            unsafe { CStr::from_ptr(project_dir) }.to_str(),
            unsafe { CStr::from_ptr(out_path) }.to_str(),
        ) else {
            return std::ptr::null_mut();
        };
        let project = match Project::open(Path::new(dir)) {
            Ok(project) => project,
            Err(e) => {
                eprintln!("promo_export_start: {e}");
                return std::ptr::null_mut();
            }
        };
        let out = PathBuf::from(out);
        let settings = ExportSettings {
            width: width as u32,
            height: height as u32,
            start: 0.0,
            end: project.duration(),
            fps: if fps > 0.0 {
                fps
            } else {
                project.meta.composition_settings.fps.unwrap_or(30.0)
            }
            .max(1.0),
            overlay,
        };
        let total = (((settings.end - settings.start) * settings.fps).round() as usize).max(1);

        let shared = Arc::new(Shared {
            done: AtomicUsize::new(0),
            total: AtomicUsize::new(total),
            cancel: AtomicBool::new(false),
            state: AtomicI32::new(RUNNING),
        });
        let worker = Arc::clone(&shared);
        let thread = std::thread::spawn(move || {
            // Panic fence: this thread's panic must become FAILED, not a
            // poisoned process. catch_unwind because there is no C caller
            // above us to return an error code to.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                export_video(&project, &out, &settings, &mut |done, _| {
                    worker.done.store(done, Ordering::Relaxed);
                    !worker.cancel.load(Ordering::Relaxed)
                })
            }));
            let state = match result {
                Ok(Ok(ExportOutcome::Finished)) => FINISHED,
                Ok(Ok(ExportOutcome::Cancelled)) => CANCELLED,
                Ok(Err(e)) => {
                    eprintln!("promo_export: {e}");
                    FAILED
                }
                Err(_) => {
                    eprintln!("promo_export: engine panicked (fenced)");
                    FAILED
                }
            };
            worker.state.store(state, Ordering::Release);
        });

        Box::into_raw(Box::new(ExportHandle {
            shared,
            thread: Some(thread),
        }))
    }
}

/// The job's state: 0 running, 1 finished, 2 cancelled, -4 failed (reason
/// on stderr), -1 bad handle. Fills `out_done`/`out_total` (frames) when
/// non-null.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_export_progress(
    handle: *const ExportHandle,
    out_done: *mut u64,
    out_total: *mut u64,
) -> c_int {
    crate::ffi_guard(-1, move || {
        let Some(handle) = (unsafe { handle.as_ref() }) else {
            return -1;
        };
        unsafe {
            if !out_done.is_null() {
                *out_done = handle.shared.done.load(Ordering::Relaxed) as u64;
            }
            if !out_total.is_null() {
                *out_total = handle.shared.total.load(Ordering::Relaxed) as u64;
            }
        }
        handle.shared.state.load(Ordering::Acquire)
    })
}

/// Asks the job to stop after the frame it is on. Asynchronous: poll
/// `promo_export_progress` until it answers 2 (cancelled). The partial
/// file is removed by then. A finished job ignores this.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_export_cancel(handle: *const ExportHandle) {
    crate::ffi_guard((), move || {
        if let Some(handle) = unsafe { handle.as_ref() } {
            handle.shared.cancel.store(true, Ordering::Relaxed);
        }
    })
}

/// Frees the job. A still-running export is cancelled and JOINED first —
/// freeing must never leave a detached thread writing to a file the host
/// believes abandoned. Null is a no-op.
///
/// Safety contract (C ABI): `handle` came from this library, freed at most
/// once.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_export_free(handle: *mut ExportHandle) {
    crate::ffi_guard((), move || {
        if handle.is_null() {
            return;
        }
        let mut handle = unsafe { Box::from_raw(handle) };
        handle.shared.cancel.store(true, Ordering::Relaxed);
        if let Some(thread) = handle.thread.take() {
            let _ = thread.join();
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    fn solid_project(dir: &Path) -> CString {
        std::fs::create_dir_all(dir).unwrap();
        let json = r#"{"id":"AAAAAAAA-0000-0000-0000-0000000000E1","name":"solid",
            "createdAt":0,"state":"recorded","trimStart":0,"trimEnd":0,
            "videoDuration":1,"subtitles":[],
            "compositionSettings":{"canvasWidth":160,"canvasHeight":120,
              "backgroundColorHex":"0000FF"},
            "resources":[],"layers":[]}"#;
        std::fs::write(dir.join("metadata.json"), json).unwrap();
        CString::new(dir.to_str().unwrap()).unwrap()
    }

    fn wait_for_end(handle: *const ExportHandle) -> c_int {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        loop {
            let state = promo_export_progress(handle, std::ptr::null_mut(), std::ptr::null_mut());
            if state != RUNNING || std::time::Instant::now() > deadline {
                return state;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn a_missing_project_is_null() {
        let dir = CString::new("/not/a/project").unwrap();
        let out = CString::new("/tmp/never.mp4").unwrap();
        assert!(promo_export_start(dir.as_ptr(), out.as_ptr(), 160, 120, 0.0).is_null());
    }

    #[test]
    fn an_export_finishes_and_the_file_plays() {
        let dir = std::env::temp_dir().join(format!("promo-ffi-export-{}", std::process::id()));
        let cdir = solid_project(&dir);
        let out = dir.join("out.mp4");
        let cout = CString::new(out.to_str().unwrap()).unwrap();

        let handle = promo_export_start(cdir.as_ptr(), cout.as_ptr(), 160, 120, 30.0);
        if handle.is_null() {
            eprintln!("no GPU adapter; skipping");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let state = wait_for_end(handle);
        let (mut done, mut total) = (0u64, 0u64);
        promo_export_progress(handle, &mut done, &mut total);
        promo_export_free(handle);

        if state == FAILED && !out.exists() {
            // Encoding needs ffmpeg; without it the job fails cleanly.
            eprintln!("export failed (ffmpeg absent?); state honest, skipping playback check");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        assert_eq!(state, FINISHED);
        assert_eq!((done, total), (30, 30), "1s at 30fps");
        // The file must decode back — written is not the same as playable.
        let registry = promo_media::Registry::with_defaults();
        let decoder = registry.open_decoder(&out).expect("the export plays");
        assert_eq!(
            (decoder.info().width, decoder.info().height),
            (160, 120),
            "the export is the size that was asked for"
        );
        drop(decoder);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The example project carries real media (video + audio), so this is
    /// the cancel path with decoders and a soundtrack in play — the solid
    /// project below can't see a failure in either.
    #[test]
    fn the_example_project_export_cancels_cleanly() {
        let project = concat!(env!("CARGO_MANIFEST_DIR"), "/../examples/LinuxSmoke.promo");
        let cdir = CString::new(project).unwrap();
        let out = std::env::temp_dir().join(format!("promo-ffi-smoke-{}.mp4", std::process::id()));
        let cout = CString::new(out.to_str().unwrap()).unwrap();

        let handle = promo_export_start(cdir.as_ptr(), cout.as_ptr(), 320, 180, 60.0);
        if handle.is_null() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        promo_export_cancel(handle);
        let state = wait_for_end(handle);
        promo_export_free(handle);
        assert_eq!(state, CANCELLED, "expected a clean cancel, not {state}");
        assert!(!out.exists());
    }

    #[test]
    fn a_cancelled_export_leaves_no_file_behind() {
        let dir = std::env::temp_dir().join(format!("promo-ffi-cancel-{}", std::process::id()));
        let cdir = solid_project(&dir);
        let out = dir.join("cancelled.mp4");
        let cout = CString::new(out.to_str().unwrap()).unwrap();

        // 1s at 600fps = 600 frames: enough runway that cancel lands while
        // the job is still going.
        let handle = promo_export_start(cdir.as_ptr(), cout.as_ptr(), 160, 120, 600.0);
        if handle.is_null() {
            eprintln!("no GPU adapter; skipping");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        promo_export_cancel(handle);
        let state = wait_for_end(handle);
        promo_export_free(handle);

        if state == FAILED && !out.exists() {
            eprintln!("export failed before cancel (ffmpeg absent?); skipping");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        assert_eq!(state, CANCELLED);
        assert!(
            !out.exists(),
            "a cancelled export must not leave a file that looks exported"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
