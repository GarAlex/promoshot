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
    /// Kept for the soundtrack call, which mixes lazily on first ask.
    project: Project,
    /// None = not asked yet; Some(None) = asked, composition has no audio.
    soundtrack: Option<Option<promo_media::AudioBuffer>>,
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
                        project,
                        soundtrack: None,
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

/// Replaces the renderer's project with an edited document (the same
/// metadata.json payload everything else exchanges), keeping the GPU
/// pipeline and frame cache warm — the editor calls this after every
/// applied command instead of reopening. Timing edits change the mix, so
/// the cached soundtrack is dropped; ask `promo_renderer_soundtrack_info`
/// again. The staged media is NOT rebuilt: valid while commands leave
/// `resources` alone, which the current command set does.
/// 0 ok, -1 bad handle/pointer, -2 the payload does not parse.
///
/// Safety contract (C ABI): `json` is a valid NUL-terminated string.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_renderer_set_project(
    handle: *mut RendererHandle,
    json: *const c_char,
) -> c_int {
    crate::ffi_guard(-2, move || {
        let Some(handle) = (unsafe { handle.as_mut() }) else {
            return -1;
        };
        if json.is_null() {
            return -1;
        }
        let Ok(text) = (unsafe { CStr::from_ptr(json) }).to_str() else {
            return -1;
        };
        let meta = match promo_model::ProjectMetadata::from_json(text) {
            Ok(meta) => meta,
            Err(e) => {
                eprintln!("promo_renderer_set_project: {e}");
                return -2;
            }
        };
        handle.project.meta = meta;
        handle.duration = handle.project.duration();
        handle.soundtrack = None;
        // Re-stages the media too, so a command that grew the resource
        // list previews without a reopen.
        if let Err(e) = handle.renderer.set_project(&handle.project) {
            eprintln!("promo_renderer_set_project: {e}");
            return -4;
        }
        0
    })
}

/// Sets (or, with NULL, clears) a host-rasterized overlay composited over
/// every subsequent frame — the watermark seam, and the same final quad
/// the export's overlay is, so a watermarked preview matches a
/// watermarked export. `bgra` is PREMULTIPLIED BGRA stretched over the
/// canvas; `len` must equal `width * height * 4` exactly (refused with -2
/// rather than read past). Uploaded once here; per-frame cost is one quad.
/// 0 ok, -1 bad handle, -2 size mismatch, -4 upload failed (stderr).
///
/// Safety contract (C ABI): `bgra` is NULL or addresses `len` readable
/// bytes; copied/uploaded during the call.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_renderer_set_overlay(
    handle: *mut RendererHandle,
    bgra: *const u8,
    len: usize,
    width: c_int,
    height: c_int,
) -> c_int {
    crate::ffi_guard(-4, move || {
        let Some(handle) = (unsafe { handle.as_mut() }) else {
            return -1;
        };
        if bgra.is_null() {
            let _ = handle.renderer.set_overlay(None);
            return 0;
        }
        if width <= 0 || height <= 0 || len != (width as usize * height as usize * 4) {
            return -2;
        }
        let bytes = unsafe { std::slice::from_raw_parts(bgra, len) };
        match handle
            .renderer
            .set_overlay(Some((bytes, width as u32, height as u32)))
        {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("promo_renderer_set_overlay: {e}");
                -4
            }
        }
    })
}

/// Mixes the composition's soundtrack — the SAME mix the export muxes, via
/// `render::build_soundtrack` — and reports its shape. Mixed once, cached
/// on the handle. Fills the out-params when non-null.
/// 0 = audio present, 1 = the composition has no audio (a real answer, not
/// an error — an empty preview must be silent because there is nothing to
/// play, never because nothing could be asked), -1 bad handle, -4 the mix
/// failed (reason on stderr).
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_renderer_soundtrack_info(
    handle: *mut RendererHandle,
    out_frames: *mut u64,
    out_sample_rate: *mut u32,
    out_channels: *mut u32,
) -> c_int {
    crate::ffi_guard(-4, move || {
        let Some(handle) = (unsafe { handle.as_mut() }) else {
            return -1;
        };
        if handle.soundtrack.is_none() {
            match promo_cli::render::build_soundtrack(&handle.project, handle.duration) {
                Ok(mixed) => handle.soundtrack = Some(mixed),
                Err(e) => {
                    eprintln!("promo_renderer_soundtrack_info: {e}");
                    return -4;
                }
            }
        }
        match handle.soundtrack.as_ref().unwrap() {
            None => 1,
            Some(audio) => {
                unsafe {
                    if !out_frames.is_null() {
                        *out_frames = (audio.samples.len() / audio.channels as usize) as u64;
                    }
                    if !out_sample_rate.is_null() {
                        *out_sample_rate = audio.sample_rate;
                    }
                    if !out_channels.is_null() {
                        *out_channels = audio.channels as u32;
                    }
                }
                0
            }
        }
    })
}

/// Copies the mixed soundtrack as interleaved f32 samples. `out_len` is in
/// FLOATS and must equal frames × channels from
/// `promo_renderer_soundtrack_info` exactly — any other size is refused
/// (-2) rather than written past. 0 ok, 1 no audio, -1 bad handle/pointer
/// or info not yet asked.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_renderer_soundtrack_pcm(
    handle: *const RendererHandle,
    out_samples: *mut f32,
    out_len: usize,
) -> c_int {
    crate::ffi_guard(-4, move || {
        let Some(handle) = (unsafe { handle.as_ref() }) else {
            return -1;
        };
        let Some(soundtrack) = handle.soundtrack.as_ref() else {
            // No implicit mixing here: info() owns the (fallible, slow)
            // mix, and this call stays a plain copy.
            return -1;
        };
        match soundtrack {
            None => 1,
            Some(audio) => {
                if out_samples.is_null() {
                    return -1;
                }
                if out_len != audio.samples.len() {
                    return -2;
                }
                unsafe {
                    std::ptr::copy_nonoverlapping(audio.samples.as_ptr(), out_samples, out_len)
                };
                0
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

    /// The overlay is the watermark seam: set, every frame carries it;
    /// cleared, frames come back clean. A solid blue canvas under a solid
    /// red overlay proves both directions of that.
    #[test]
    fn an_overlay_covers_the_frame_and_clears_away() {
        let dir = std::env::temp_dir().join(format!("promo-ffi-overlay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let json = r#"{"id":"AAAAAAAA-0000-0000-0000-0000000000V1","name":"blue",
            "createdAt":0,"state":"recorded","trimStart":0,"trimEnd":0,
            "videoDuration":1,"subtitles":[],
            "compositionSettings":{"canvasWidth":160,"canvasHeight":120,
              "backgroundColorHex":"0000FF"},
            "resources":[],"layers":[]}"#;
        std::fs::write(dir.join("metadata.json"), json).unwrap();
        let cdir = CString::new(dir.to_str().unwrap()).unwrap();
        let handle = promo_renderer_new(cdir.as_ptr(), 160, 120);
        if handle.is_null() {
            eprintln!("no GPU adapter; skipping");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let center = (60 * 160 + 80) * 4;
        let mut pixels = vec![0u8; 160 * 120 * 4];
        let red_at = |px: &[u8]| (px[center], px[center + 2]); // (b, r)

        // Opaque red, premultiplied BGRA, tiny — the quad stretches it.
        let overlay = [0u8, 0, 255, 255].repeat(4 * 4);
        assert_eq!(
            promo_renderer_set_overlay(handle, overlay.as_ptr(), overlay.len() - 1, 4, 4),
            -2,
            "a mismatched overlay buffer is refused, not read past"
        );
        assert_eq!(
            promo_renderer_set_overlay(handle, overlay.as_ptr(), overlay.len(), 4, 4),
            0
        );
        assert_eq!(
            promo_renderer_frame_bgra(handle, 0.5, pixels.as_mut_ptr(), pixels.len()),
            0
        );
        let (b, r) = red_at(&pixels);
        assert!(
            r > 200 && b < 40,
            "overlaid frame should be red, got b={b} r={r}"
        );

        assert_eq!(
            promo_renderer_set_overlay(handle, std::ptr::null(), 0, 0, 0),
            0
        );
        assert_eq!(
            promo_renderer_frame_bgra(handle, 0.5, pixels.as_mut_ptr(), pixels.len()),
            0
        );
        let (b, r) = red_at(&pixels);
        assert!(
            b > 200 && r < 40,
            "cleared frame should be blue again, got b={b} r={r}"
        );

        promo_renderer_free(handle);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The editor's per-command path: replace the project in place and the
    /// very next frame is the edited document's, no reopen.
    #[test]
    fn set_project_changes_the_next_frame_and_the_duration() {
        let dir = std::env::temp_dir().join(format!("promo-ffi-setproj-{}", std::process::id()));
        let cdir = solid_background_project(&dir);
        let handle = promo_renderer_new(cdir.as_ptr(), 160, 120);
        if handle.is_null() {
            eprintln!("no GPU adapter; skipping");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let mut pixels = vec![0u8; 160 * 120 * 4];
        assert_eq!(
            promo_renderer_frame_bgra(handle, 0.5, pixels.as_mut_ptr(), pixels.len()),
            0
        );
        let center = (60 * 160 + 80) * 4;
        assert!(pixels[center + 2] > 200, "starts red");

        let edited = std::fs::read_to_string(dir.join("metadata.json"))
            .unwrap()
            .replace("FF0000", "0000FF")
            .replace("\"videoDuration\":2", "\"videoDuration\":5");
        let cjson = CString::new(edited).unwrap();
        assert_eq!(promo_renderer_set_project(handle, cjson.as_ptr()), 0);
        assert!((promo_renderer_duration(handle) - 5.0).abs() < 1e-9);
        assert_eq!(
            promo_renderer_frame_bgra(handle, 0.5, pixels.as_mut_ptr(), pixels.len()),
            0
        );
        assert!(
            pixels[center] > 200 && pixels[center + 2] < 40,
            "the edited document renders blue, got b={} r={}",
            pixels[center],
            pixels[center + 2]
        );
        promo_renderer_free(handle);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The trim editor's path: set_project introduces a VIDEO (with audio,
    /// trims and a mediaCut) to a renderer opened on a project that had
    /// none — the first frame and the soundtrack must both answer, not
    /// bring the process down. Every earlier set_project exercise re-staged
    /// stills only; this is the drive that crashed the Windows app.
    #[test]
    fn set_project_can_introduce_a_trimmed_video() {
        let dir = std::env::temp_dir().join(format!("promo-ffi-setvid-{}", std::process::id()));
        let cdir = solid_background_project(&dir);
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        let clip = dir.join("Resources").join("clip.mp4");
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x240:rate=30:duration=3",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=3",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ])
            .arg(&clip)
            .status();
        if !status.map(|s| s.success()).unwrap_or(false) {
            eprintln!("no ffmpeg; skipping");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let handle = promo_renderer_new(cdir.as_ptr(), 160, 120);
        if handle.is_null() {
            eprintln!("no GPU adapter; skipping");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let edited = std::fs::read_to_string(dir.join("metadata.json"))
            .unwrap()
            .replace(
                r#""resources":[],"layers":[]"#,
                r#""resources":[{"id":"V","kind":"video","filename":"clip.mp4",
                    "displayName":"Clip","addedAt":0,"trimStart":1.0,"trimEnd":2.5,
                    "mediaCuts":[{"id":"C","name":"Finale","trimStart":0.5,"trimEnd":1.5}]}],
                   "layers":[{"id":"L","name":"Clip","sortIndex":0,"kind":"video",
                    "isEnabled":true,"startTime":0.0,"duration":1.5,"resourceID":"V",
                    "keyframes":[{"id":"K","time":0.0,"transitionDuration":0.0,
                     "placement":{"mode":"fit","anchor":"center"}}]}]"#,
            );
        let cjson = CString::new(edited.clone()).unwrap();
        assert_eq!(promo_renderer_set_project(handle, cjson.as_ptr()), 0);
        let mut pixels = vec![0u8; 160 * 120 * 4];
        assert_eq!(
            promo_renderer_frame_bgra(handle, 0.5, pixels.as_mut_ptr(), pixels.len()),
            0,
            "a frame of the introduced video renders"
        );
        // Retrim while the previous staging holds a LIVE decoder for the
        // same layer — the frame above opened one. The app's crash path.
        let retrimmed = edited.replace("\"trimStart\":1.0,\"trimEnd\":2.5,", "");
        let cjson = CString::new(retrimmed).unwrap();
        assert_eq!(promo_renderer_set_project(handle, cjson.as_ptr()), 0);
        assert_eq!(
            promo_renderer_frame_bgra(handle, 0.5, pixels.as_mut_ptr(), pixels.len()),
            0,
            "a frame after the retrim renders"
        );
        let (mut frames, mut rate, mut channels) = (0u64, 0u32, 0u32);
        let info = promo_renderer_soundtrack_info(handle, &mut frames, &mut rate, &mut channels);
        assert!(
            info == 0 || info == 1,
            "soundtrack info answers (mixed or none), got {info}"
        );
        if info == 0 {
            let mut pcm = vec![0f32; frames as usize * channels as usize];
            assert_eq!(
                promo_renderer_soundtrack_pcm(handle, pcm.as_mut_ptr(), pcm.len()),
                0
            );
        }
        promo_renderer_free(handle);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// "No audio" is a real answer (1), distinct from failure — and pcm
    /// without info first is refused, because the copy call must never
    /// hide a fallible mix inside itself.
    #[test]
    fn a_silent_project_says_no_audio_and_pcm_needs_info_first() {
        let dir = std::env::temp_dir().join(format!("promo-ffi-silent-{}", std::process::id()));
        let cdir = solid_background_project(&dir);
        let handle = promo_renderer_new(cdir.as_ptr(), 160, 120);
        if handle.is_null() {
            eprintln!("no GPU adapter; skipping");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let mut sample = 0.0f32;
        assert_eq!(
            promo_renderer_soundtrack_pcm(handle, &mut sample, 1),
            -1,
            "pcm before info must refuse, not mix"
        );
        assert_eq!(
            promo_renderer_soundtrack_info(
                handle,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut()
            ),
            1
        );
        assert_eq!(promo_renderer_soundtrack_pcm(handle, &mut sample, 1), 1);
        promo_renderer_free(handle);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The example project carries real audio; the mix must have the
    /// advertised shape and actual signal in it.
    #[test]
    fn the_example_projects_soundtrack_has_signal() {
        let project = concat!(env!("CARGO_MANIFEST_DIR"), "/../examples/LinuxSmoke.promo");
        let cdir = CString::new(project).unwrap();
        let handle = promo_renderer_new(cdir.as_ptr(), 320, 180);
        if handle.is_null() {
            eprintln!("no GPU adapter (or ffmpeg); skipping");
            return;
        }
        let (mut frames, mut rate, mut channels) = (0u64, 0u32, 0u32);
        let rc = promo_renderer_soundtrack_info(handle, &mut frames, &mut rate, &mut channels);
        if rc == -4 {
            eprintln!("mix failed (ffmpeg absent?); skipping");
            promo_renderer_free(handle);
            return;
        }
        assert_eq!(rc, 0, "the example project has audio");
        assert_eq!((rate, channels), (48_000, 2));
        assert!(frames > 0);

        let len = (frames * channels as u64) as usize;
        let mut pcm = vec![0.0f32; len];
        assert_eq!(
            promo_renderer_soundtrack_pcm(handle, pcm.as_mut_ptr(), len),
            0
        );
        assert_eq!(
            promo_renderer_soundtrack_pcm(handle, pcm.as_mut_ptr(), len - 1),
            -2,
            "a wrong-size buffer is refused, not written past"
        );
        assert!(
            pcm.iter().any(|s| s.abs() > 1e-4),
            "the mix must carry signal, not silence shaped like an answer"
        );
        promo_renderer_free(handle);
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
