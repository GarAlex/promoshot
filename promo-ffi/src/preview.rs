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
    if project_json.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(text) = unsafe { CStr::from_ptr(project_json) }.to_str() else {
        return std::ptr::null_mut();
    };
    let Ok(meta) = ProjectMetadata::from_json(text) else {
        return std::ptr::null_mut();
    };
    match PreviewEngine::new(meta, provider, user, budget_bytes as usize) {
        Ok(engine) => Box::into_raw(Box::new(PreviewHandle { engine })),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Frees a preview engine (releases every cached frame). Null is a no-op.
///
/// Safety contract (C ABI): `handle` must be a pointer this library
/// returned, freed at most once.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_preview_free(handle: *mut PreviewHandle) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle) });
    }
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
    let Some(handle) = (unsafe { handle.as_mut() }) else {
        return -1;
    };
    if output_surface.is_null() || width <= 0 || height <= 0 {
        return -1;
    }
    match handle
        .engine
        .render(time, output_surface, width as u32, height as u32)
    {
        Ok(()) => 0,
        Err(_) => -4,
    }
}

/// Decodes-ahead for `time` (fills the frame cache without composing).
/// Returns the number of newly fetched frames, or -1 on bad handle.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_preview_prefetch(handle: *mut PreviewHandle, time: c_double) -> c_int {
    let Some(handle) = (unsafe { handle.as_mut() }) else {
        return -1;
    };
    handle.engine.prefetch(time) as c_int
}

/// Sets the proxy tier for subsequent video-frame requests (0 = full
/// resolution; the host raises it while scrubbing and drops it back for the
/// paused full-res refine). 0 ok, -1 bad handle.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_preview_set_tier(handle: *mut PreviewHandle, tier: c_int) -> c_int {
    let Some(handle) = (unsafe { handle.as_mut() }) else {
        return -1;
    };
    handle.engine.set_preferred_tier(tier);
    0
}

/// Fills `out[0..4]` = cache hits, misses, cached bytes, evictions.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_preview_stats(handle: *const PreviewHandle, out: *mut u64) -> c_int {
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
}
