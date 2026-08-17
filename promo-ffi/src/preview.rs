//! Preview-engine FFI (Phase 3, macOS): the host registers a frame-provider
//! callback and asks the engine to render the composition at arbitrary times
//! into IOSurfaces; the engine owns timing math, caching (under a byte
//! budget), and GPU composition.

#![cfg(any(target_os = "macos", target_os = "ios"))]

use promo_engine::{FrameProviderFn, PreviewEngine};
use promo_model::ProjectMetadata;
use std::ffi::{c_char, c_double, c_int, c_void, CStr};

/// Opaque to C.
pub struct PreviewHandle {
    engine: PreviewEngine,
}

/// Test-only injection: forces a panic inside the guarded body of
/// `promo_preview_render_with_overlay`, so a test can prove a panic comes
/// back to the C caller as an error code instead of aborting the process.
#[cfg(test)]
static FORCE_PANIC: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
fn panic_if_forced_by_test() {
    if FORCE_PANIC.load(std::sync::atomic::Ordering::Relaxed) {
        panic!("forced panic injected by preview::tests");
    }
}

/// Creates a preview engine for a project (`metadata.json` payload,
/// NUL-terminated). `provider` + `user` supply layer frames (see
/// `promo_core.h`); `budget_bytes` caps the frame cache. Null on parse or
/// GPU failure. Free with `promo_preview_free`.
///
/// Safety contract (C ABI): `project_json` is a valid NUL-terminated string;
/// `provider`/`user` stay valid for the handle's lifetime.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_preview_new(
    project_json: *const c_char,
    provider: FrameProviderFn,
    user: *mut c_void,
    budget_bytes: u64,
) -> *mut PreviewHandle {
    crate::ffi_guard(std::ptr::null_mut(), move || {
        if project_json.is_null() {
            return std::ptr::null_mut();
        }
        let Ok(text) = unsafe { CStr::from_ptr(project_json) }.to_str() else {
            return std::ptr::null_mut();
        };
        let Ok(meta) = ProjectMetadata::from_json(text) else {
            return std::ptr::null_mut();
        };
        // Panic fence: GPU bring-up must fail as NULL, never abort the host (see
        // promo_preview_render_with_overlay).
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            match PreviewEngine::new(meta, provider, user, budget_bytes as usize) {
                Ok(engine) => Box::into_raw(Box::new(PreviewHandle { engine })),
                Err(_) => std::ptr::null_mut(),
            }
        }));
        result.unwrap_or(std::ptr::null_mut())
    })
}

/// Frees a preview engine (releases every cached frame). Null is a no-op.
///
/// Safety contract (C ABI): `handle` must be a pointer this library
/// returned, freed at most once.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_preview_free(handle: *mut PreviewHandle) {
    crate::ffi_guard((), move || {
        if !handle.is_null() {
            drop(unsafe { Box::from_raw(handle) });
        }
    })
}

/// Renders the composition at `time` into `output_surface` (BGRA IOSurface,
/// `width`×`height`; the canvas is aspect-fit inside). 0 ok, -1 bad input,
/// -4 render failed.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_preview_render(
    handle: *mut PreviewHandle,
    time: c_double,
    output_surface: *mut c_void,
    width: c_int,
    height: c_int,
) -> c_int {
    promo_preview_render_with_overlay(
        handle,
        time,
        output_surface,
        width,
        height,
        std::ptr::null_mut(),
        0,
        0,
    )
}

/// Replaces the engine's project with an edited one, keeping the GPU
/// pipeline and every cached frame whose layer did not change. The editor
/// calls this on each edit (a drag: every frame) instead of recreating the
/// engine. 0 ok, -1 bad handle/JSON, -2 parse failed.
///
/// Safety contract (C ABI): `project_json` is NUL-terminated UTF-8.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_preview_set_project(
    handle: *mut PreviewHandle,
    project_json: *const c_char,
) -> c_int {
    crate::ffi_guard(-2, move || {
        let Some(handle) = (unsafe { handle.as_mut() }) else {
            return -1;
        };
        if project_json.is_null() {
            return -1;
        }
        let Ok(text) = unsafe { CStr::from_ptr(project_json) }.to_str() else {
            return -1;
        };
        let Ok(meta) = promo_model::ProjectMetadata::from_json(text) else {
            return -2;
        };
        handle.engine.set_project(meta);
        0
    })
}

/// `promo_preview_render` plus a host-rasterized caption/watermark overlay
/// (BGRA IOSurface of overlay_width x overlay_height, canvas-space) drawn
/// last — the same final quad the export path composites, so an in-app
/// preview matches the exported frame. Pass NULL for no overlay.
///
/// Safety contract (C ABI): surfaces must be BGRA IOSurfaceRefs of the
/// stated sizes.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_preview_render_with_overlay(
    handle: *mut PreviewHandle,
    time: c_double,
    output_surface: *mut c_void,
    width: c_int,
    height: c_int,
    overlay_surface: *mut c_void,
    overlay_width: c_int,
    overlay_height: c_int,
) -> c_int {
    crate::ffi_guard(-4, move || {
        #[cfg(test)]
        panic_if_forced_by_test();
        let Some(handle) = (unsafe { handle.as_mut() }) else {
            return -1;
        };
        if output_surface.is_null() || width <= 0 || height <= 0 {
            return -1;
        }
        let overlay = if overlay_surface.is_null() || overlay_width <= 0 || overlay_height <= 0 {
            None
        } else {
            Some((overlay_surface, overlay_width as u32, overlay_height as u32))
        };
        // Panic fence: a panic that unwinds out of an extern "C" fn ABORTS the
        // host process (this actually happened — cosmic-text's shaper panicked on
        // an empty font db on iOS and took the app with it). A frame that cannot
        // render is a skipped frame, not a crashed editor. -5 = engine panicked.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            match handle.engine.render_with_overlay(
                time,
                output_surface,
                width as u32,
                height as u32,
                overlay,
            ) {
                Ok(()) => 0,
                Err(_) => -4,
            }
        }));
        result.unwrap_or(-5)
    })
}

/// Export mode: the export clock is monotonic, so per-time frames (video,
/// animated-tilt bakes) bypass the LRU cache instead of poisoning it; static
/// content stays cached. 0 ok, -1 bad handle.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_preview_set_export_mode(
    handle: *mut PreviewHandle,
    enabled: c_int,
) -> c_int {
    crate::ffi_guard(-1, move || {
        let Some(handle) = (unsafe { handle.as_mut() }) else {
            return -1;
        };
        handle.engine.set_export_mode(enabled != 0);
        0
    })
}

/// Density multiplier for engine-made canvas-space rasters (captions): pass
/// the export's letterbox scale so text is rasterized at output resolution.
/// 1.0 for preview. 0 ok, -1 bad handle.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_preview_set_raster_scale(
    handle: *mut PreviewHandle,
    scale: c_double,
) -> c_int {
    crate::ffi_guard(-1, move || {
        let Some(handle) = (unsafe { handle.as_mut() }) else {
            return -1;
        };
        handle.engine.set_raster_scale(scale);
        0
    })
}

/// Deferred completion for the engine's compose — the preview-engine twin of
/// `promo_compositor_set_defer`. With it on, `promo_preview_render_export`
/// returns a token to wait on instead of blocking. 0 ok, -1 bad handle.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_preview_set_defer(handle: *mut PreviewHandle, enabled: c_int) -> c_int {
    crate::ffi_guard(-1, move || {
        let Some(handle) = (unsafe { handle.as_mut() }) else {
            return -1;
        };
        handle.engine.set_defer_completion(enabled != 0);
        0
    })
}

/// `promo_preview_render_with_overlay` for the export pipeline: when deferred
/// completion is on (`promo_preview_set_defer`), `out_token` receives a
/// `SubmissionToken*` the caller MUST pass to `promo_submission_wait` before
/// reading `output_surface` (NULL when the compose already blocked).
/// Return codes as in `promo_preview_render_with_overlay`.
///
/// Safety contract (C ABI): surfaces as in
/// `promo_preview_render_with_overlay`; `out_token` may be NULL (the render
/// then blocks regardless of the defer flag — an unclaimed fence would leave
/// the encoder reading half-rendered pixels).
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn promo_preview_render_export(
    handle: *mut PreviewHandle,
    time: c_double,
    output_surface: *mut c_void,
    width: c_int,
    height: c_int,
    overlay_surface: *mut c_void,
    overlay_width: c_int,
    overlay_height: c_int,
    out_token: *mut *mut crate::compose::SubmissionToken,
) -> c_int {
    crate::ffi_guard(-4, move || {
        let rc = promo_preview_render_with_overlay(
            handle,
            time,
            output_surface,
            width,
            height,
            overlay_surface,
            overlay_width,
            overlay_height,
        );
        if rc != 0 {
            return rc;
        }
        // Safe: rc == 0 proved the handle valid above.
        let handle = unsafe { &mut *handle };
        let fence = handle.engine.take_fence();
        if out_token.is_null() {
            // Nobody to hand the fence to — block here so the caller can read.
            if let (Some(fence), Some(ctx)) = (fence, promo_gpu::GpuContext::shared()) {
                fence.wait(ctx);
            }
            return 0;
        }
        unsafe {
            *out_token = match fence {
                Some(fence) => Box::into_raw(Box::new(crate::compose::SubmissionToken::new(fence))),
                None => std::ptr::null_mut(),
            };
        }
        0
    })
}

/// Re-targets the frame-cache budget (bytes). The host sizes this from the
/// machine's RAM and shrinks it under memory pressure. 0 ok, -1 bad handle.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_preview_set_cache_budget(handle: *mut PreviewHandle, bytes: u64) -> c_int {
    crate::ffi_guard(-1, move || {
        let Some(handle) = (unsafe { handle.as_mut() }) else {
            return -1;
        };
        handle.engine.set_cache_budget(bytes as usize);
        0
    })
}

/// Decodes-ahead for `time` (fills the frame cache without composing).
/// Returns the number of newly fetched frames, or -1 on bad handle.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_preview_prefetch(handle: *mut PreviewHandle, time: c_double) -> c_int {
    crate::ffi_guard(-1, move || {
        let Some(handle) = (unsafe { handle.as_mut() }) else {
            return -1;
        };
        // Panic fence: see promo_preview_render_with_overlay. -5 = panicked.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handle.engine.prefetch(time) as c_int
        }));
        result.unwrap_or(-5)
    })
}

/// Sets the proxy tier for subsequent video-frame requests (0 = full
/// resolution; the host raises it while scrubbing and drops it back for the
/// paused full-res refine). 0 ok, -1 bad handle.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_preview_set_tier(handle: *mut PreviewHandle, tier: c_int) -> c_int {
    crate::ffi_guard(-1, move || {
        let Some(handle) = (unsafe { handle.as_mut() }) else {
            return -1;
        };
        handle.engine.set_preferred_tier(tier);
        0
    })
}

/// Fills `out[0..4]` = cache hits, misses, cached bytes, evictions.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_preview_stats(handle: *const PreviewHandle, out: *mut u64) -> c_int {
    crate::ffi_guard(-1, move || {
        let Some(handle) = (unsafe { handle.as_ref() }) else {
            return -1;
        };
        if out.is_null() {
            return -1;
        }
        let stats = handle.engine.stats();
        unsafe {
            *out = stats.hits;
            *out.add(1) = stats.misses;
            *out.add(2) = stats.cached_bytes;
            *out.add(3) = stats.evictions;
        }
        0
    })
}

/// Field offsets of [`HostSurface`], so a host can PROVE its mirrored struct
/// matches instead of trusting that two languages agree on layout.
///
/// Swift structs are not C-representable, so `PromoCore.swift` declares its
/// own `PromoHostSurface` and binds raw memory to it. That is an assumption
/// about layout; `PromoCoreGateTests` turns it into a checked invariant by
/// comparing every value here against `MemoryLayout`.
///
/// `field`: 0 = total size, 1 = kind, 2 = handle, 3 = fd, 4 = data,
/// 5 = width, 6 = height, 7 = bytes_per_row. Returns `u64::MAX` if unknown.
#[no_mangle]
pub extern "C" fn promo_host_surface_layout(field: i32) -> u64 {
    use promo_engine::HostSurface;
    match field {
        0 => std::mem::size_of::<HostSurface>() as u64,
        1 => std::mem::offset_of!(HostSurface, kind) as u64,
        2 => std::mem::offset_of!(HostSurface, handle) as u64,
        3 => std::mem::offset_of!(HostSurface, fd) as u64,
        4 => std::mem::offset_of!(HostSurface, data) as u64,
        5 => std::mem::offset_of!(HostSurface, width) as u64,
        6 => std::mem::offset_of!(HostSurface, height) as u64,
        7 => std::mem::offset_of!(HostSurface, bytes_per_row) as u64,
        _ => u64::MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A panic inside the render entry point must degrade to the render-failed
    /// code. Without the `ffi_guard` fence this test would abort the whole
    /// process (`panic_cannot_unwind`) — which is exactly what the cosmic-text
    /// "no default font found" panic did to the iOS host app.
    #[test]
    fn render_panic_degrades_to_error_code() {
        FORCE_PANIC.store(true, std::sync::atomic::Ordering::Relaxed);
        let rc = promo_preview_render_with_overlay(
            std::ptr::null_mut(),
            0.0,
            std::ptr::null_mut(),
            0,
            0,
            std::ptr::null_mut(),
            0,
            0,
        );
        FORCE_PANIC.store(false, std::sync::atomic::Ordering::Relaxed);
        // -4, not -1: proves the injected panic fired and was caught, since
        // the null-argument path would have returned -1.
        assert_eq!(rc, -4);
    }
}
