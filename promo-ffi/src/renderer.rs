//! Project renderer over the C ABI — the portable pixel path, every
//! platform.
//!
//! This re-exposes `promo_cli::render::Renderer`, the host that proved the
//! engine on Linux and drives `promo still`/`promo video`: media decode
//! through `promo-media`, images from disk, composition through
//! `render_to_texture`, pixels back over a wgpu readback. A front end that
//! is not Rust (the WinUI app) drives this same host instead of growing a
//! second one in C# — so "does the app render this right?" stays a diff
//! between two callers of one implementation, and host-side media IO stays
//! a decision for the editing milestones rather than a prerequisite for
//! showing a frame.
//!
//! The IOSurface preview engine (preview.rs) remains the Apple path; this
//! one hands out CPU pixels, which is the seam the D3D12 zero-copy work
//! would later replace — measured first.

use promo_cli::project::Project;
use promo_cli::render::Renderer;
use std::ffi::{c_char, c_double, c_int, CStr};
use std::path::Path;

/// Opaque to C.
pub struct RendererHandle {
    renderer: Renderer,
    width: u32,
    height: u32,
    duration: f64,
}

/// Opens a `.promo` project folder and builds a renderer producing
/// `width`×`height` frames (the canvas aspect-fits inside, like every other
/// front end). NULL on a missing/invalid project or no GPU adapter — the
/// reason is on stderr, and `promo_project_validate` can say why a payload
/// is malformed. Free with `promo_renderer_free`.
///
/// Handles are single-threaded: one call at a time per handle.
///
/// Safety contract (C ABI): `project_dir` is a valid NUL-terminated UTF-8
/// path.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_renderer_new(
    project_dir: *const c_char,
    width: c_int,
    height: c_int,
) -> *mut RendererHandle {
    crate::ffi_guard(std::ptr::null_mut(), move || {
        if project_dir.is_null() || width <= 0 || height <= 0 {
            return std::ptr::null_mut();
        }
        let Ok(dir) = unsafe { CStr::from_ptr(project_dir) }.to_str() else {
            return std::ptr::null_mut();
        };
        let project = match Project::open(Path::new(dir)) {
            Ok(project) => project,
            Err(e) => {
                eprintln!("promo_renderer_new: {e}");
                return std::ptr::null_mut();
            }
        };
        let duration = project.duration();
        // Panic fence: GPU bring-up must fail as NULL, never abort the host
        // (the same rule promo_preview_new follows).
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                match Renderer::new(&project, width as u32, height as u32) {
                    Ok(renderer) => Box::into_raw(Box::new(RendererHandle {
                        renderer,
                        width: width as u32,
                        height: height as u32,
                        duration,
                    })),
                    Err(e) => {
                        eprintln!("promo_renderer_new: {e}");
                        std::ptr::null_mut()
                    }
                }
            }));
        result.unwrap_or(std::ptr::null_mut())
    })
}

/// Frees a renderer (closes its decoders). Null is a no-op.
///
/// Safety contract (C ABI): `handle` came from this library, freed at most
/// once.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_renderer_free(handle: *mut RendererHandle) {
    crate::ffi_guard((), move || {
        if !handle.is_null() {
            drop(unsafe { Box::from_raw(handle) });
        }
    })
}

/// The composition's duration in seconds — what a transport needs before
/// the first frame is rendered.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_renderer_duration(handle: *const RendererHandle) -> c_double {
    crate::ffi_guard(-1.0, move || {
        let Some(handle) = (unsafe { handle.as_ref() }) else {
            return -1.0;
        };
        handle.duration
    })
}

/// Renders the frame at `time` into `out_pixels` as premultiplied BGRA rows,
/// `width * height * 4` bytes with no row padding. `out_len` must equal
/// exactly that — a buffer of any other size is refused (-2) rather than
/// written past. 0 ok, -1 bad handle/pointer, -2 wrong buffer size,
/// -4 render failed (reason on stderr), -5 engine panicked (fenced —
/// a frame that cannot render must never abort the host).
///
/// Safety contract (C ABI): `out_pixels` points at `out_len` writable bytes.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_renderer_frame_bgra(
    handle: *mut RendererHandle,
    time: c_double,
    out_pixels: *mut u8,
    out_len: usize,
) -> c_int {
    crate::ffi_guard(-5, move || {
        let Some(handle) = (unsafe { handle.as_mut() }) else {
            return -1;
        };
        if out_pixels.is_null() {
            return -1;
        }
        let needed = handle.width as usize * handle.height as usize * 4;
        if out_len != needed {
            return -2;
        }
        match handle.renderer.frame_bgra(time) {
            Ok(pixels) => {
                if pixels.len() != needed {
                    // Cannot happen by construction; checked anyway because
                    // writing a mismatched length would be heap corruption in
                    // the host, which is worse than a failed frame.
                    return -4;
                }
                unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), out_pixels, needed) };
                0
            }
            Err(e) => {
                eprintln!("promo_renderer_frame_bgra: {e}");
                -4
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    /// A background-only project: no resources, no ffmpeg — just the
    /// engine's own compositor filling the canvas.
    fn solid_background_project(dir: &Path) -> CString {
        std::fs::create_dir_all(dir).unwrap();
        let json = r#"{"id":"AAAAAAAA-0000-0000-0000-0000000000R1","name":"solid",
            "createdAt":0,"state":"recorded","trimStart":0,"trimEnd":0,
            "videoDuration":2,"subtitles":[],
            "compositionSettings":{"canvasWidth":160,"canvasHeight":120,
              "backgroundColorHex":"FF0000"},
            "resources":[],"layers":[]}"#;
        std::fs::write(dir.join("metadata.json"), json).unwrap();
        CString::new(dir.to_str().unwrap()).unwrap()
    }

    #[test]
    fn a_missing_project_is_null_not_a_crash() {
        let dir = CString::new("/definitely/not/a/project").unwrap();
        assert!(promo_renderer_new(dir.as_ptr(), 160, 120).is_null());
        // And the null handle is safe everywhere.
        assert_eq!(promo_renderer_duration(std::ptr::null()), -1.0);
        promo_renderer_free(std::ptr::null_mut());
    }

    #[test]
    fn a_solid_background_renders_as_its_color() {
        let dir = std::env::temp_dir().join(format!("promo-ffi-solid-{}", std::process::id()));
        let cdir = solid_background_project(&dir);
        let handle = promo_renderer_new(cdir.as_ptr(), 160, 120);
        if handle.is_null() {
            eprintln!("no GPU adapter; skipping");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        assert!((promo_renderer_duration(handle) - 2.0).abs() < 1e-9);

        let mut pixels = vec![0u8; 160 * 120 * 4];
        assert_eq!(
            promo_renderer_frame_bgra(handle, 0.5, pixels.as_mut_ptr(), pixels.len()),
            0
        );
        // Center pixel of a red canvas, BGRA. Tolerances because the color
        // crosses an sRGB pipeline, not because the answer is uncertain.
        let center = (60 * 160 + 80) * 4;
        let (b, g, r, a) = (
            pixels[center],
            pixels[center + 1],
            pixels[center + 2],
            pixels[center + 3],
        );
        assert!(
            r > 200 && g < 40 && b < 40 && a == 255,
            "got bgra ({b},{g},{r},{a})"
        );

        // A wrong-size buffer is refused, not written past.
        assert_eq!(
            promo_renderer_frame_bgra(handle, 0.5, pixels.as_mut_ptr(), pixels.len() - 1),
            -2
        );

        promo_renderer_free(handle);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
