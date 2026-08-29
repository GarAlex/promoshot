//! promo-ffi: the C ABI the host apps link. Handle-based, additive-only
//! until Phase 5 (see RUST-CORE-PLAN.md §3/§7 in the app repo).

pub mod compose;
pub mod editor;
pub mod preview;
pub mod project;
pub mod renderer;
pub mod vector;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use compose::*;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use preview::*;
pub use project::*;
pub use renderer::*;

use std::ffi::{c_char, c_double, c_int};

/// Runs an FFI body, turning any Rust panic into `fallback` instead of
/// letting the unwind reach the C boundary. `extern "C"` is nounwind: a
/// panic escaping an entry point is promoted to an abort
/// (`panic_cannot_unwind`) that kills the host app — a core bug should cost
/// the caller one failed call (a skipped frame, a null result), not the
/// process. The default panic hook has already printed the message and
/// location to stderr by the time the fallback is returned.
///
/// `AssertUnwindSafe` is deliberate: a panic mid-call can leave a handle's
/// engine state stale (e.g. a half-updated cache), but never memory-unsafe —
/// the accepted degradation for handles that live across calls.
pub(crate) fn ffi_guard<T>(fallback: T, body: impl FnOnce() -> T) -> T {
    ffi_guard_else(move || fallback, body)
}

/// [`ffi_guard`] with a lazily built fallback, for entry points whose
/// failure value must be allocated (e.g. an error-message string).
pub(crate) fn ffi_guard_else<T>(fallback: impl FnOnce() -> T, body: impl FnOnce() -> T) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(value) => value,
        Err(_) => fallback(),
    }
}

/// Version string for the host gate test. Static storage — never freed.
#[no_mangle]
pub extern "C" fn promo_core_version() -> *const c_char {
    crate::ffi_guard(std::ptr::null(), move || {
        // Build the NUL-terminated string once.
        static VERSION: std::sync::OnceLock<std::ffi::CString> = std::sync::OnceLock::new();
        VERSION
            .get_or_init(|| std::ffi::CString::new(promo_model::core_version()).unwrap())
            .as_ptr()
    })
}

/// No-op round trip used by the FFI-overhead microbench (and as a liveness
/// probe): returns `x + 1`.
#[no_mangle]
pub extern "C" fn promo_ffi_noop(x: u64) -> u64 {
    x.wrapping_add(1)
}

/// Runs the IOSurface↔wgpu interop spike at the given size (macOS only).
/// Returns 0 on success and fills the three timing out-params (µs).
/// Non-macOS builds return -1.
///
/// Safety contract (C ABI): out-params must be null or valid writable
/// doubles — the standard out-parameter convention, checked for null.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_gpu_spike_run(
    width: c_int,
    height: c_int,
    out_import_us: *mut c_double,
    out_render_us: *mut c_double,
    out_readback_us: *mut c_double,
) -> c_int {
    crate::ffi_guard(-3, move || {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            let ctx = match promo_gpu::GpuContext::new() {
                Ok(ctx) => ctx,
                Err(_) => return -2,
            };
            match promo_gpu::spike::run(&ctx, width.max(1) as usize, height.max(1) as usize) {
                Ok(t) => {
                    unsafe {
                        if !out_import_us.is_null() {
                            *out_import_us = t.import_us;
                        }
                        if !out_render_us.is_null() {
                            *out_render_us = t.render_us;
                        }
                        if !out_readback_us.is_null() {
                            *out_readback_us = t.readback_us;
                        }
                    }
                    0
                }
                Err(_) => -3,
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        {
            let _ = (width, height, out_import_us, out_render_us, out_readback_us);
            -1
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_round_trips_through_c() {
        let ptr = promo_core_version();
        let s = unsafe { std::ffi::CStr::from_ptr(ptr) }.to_str().unwrap();
        assert!(s.starts_with("promo-core "));
    }

    #[test]
    fn noop_increments() {
        assert_eq!(promo_ffi_noop(41), 42);
    }

    /// A panic inside a guarded `extern "C"` body must come back as the
    /// fallback code; unguarded it would unwind into the nounwind boundary
    /// and abort this test process.
    #[test]
    fn ffi_guard_stops_panics_at_the_boundary() {
        extern "C" fn entry() -> c_int {
            crate::ffi_guard(-4, || panic!("forced panic: ffi_guard test"))
        }
        assert_eq!(entry(), -4);
        assert_eq!(crate::ffi_guard(-4, || 0), 0);
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    #[test]
    fn spike_runs_through_ffi() {
        let (mut a, mut b, mut c) = (0.0, 0.0, 0.0);
        let rc = promo_gpu_spike_run(64, 64, &mut a, &mut b, &mut c);
        assert_eq!(rc, 0, "spike rc");
        assert!(a > 0.0 && b > 0.0 && c > 0.0);
    }
}
