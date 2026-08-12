//! promo-ffi: the C ABI the host apps link. Handle-based, additive-only
//! until Phase 5 (see RUST-CORE-PLAN.md §3/§7 in the app repo).

pub mod compose;
pub mod project;
#[cfg(target_os = "macos")]
pub use compose::*;
pub use project::*;

use std::ffi::{c_char, c_double, c_int};

/// Version string for the host gate test. Static storage — never freed.
#[no_mangle]
pub extern "C" fn promo_core_version() -> *const c_char {
    // Build the NUL-terminated string once.
    static VERSION: std::sync::OnceLock<std::ffi::CString> = std::sync::OnceLock::new();
    VERSION
        .get_or_init(|| std::ffi::CString::new(promo_model::core_version()).unwrap())
        .as_ptr()
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
    #[cfg(target_os = "macos")]
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
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (width, height, out_import_us, out_render_us, out_readback_us);
        -1
    }
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

    #[cfg(target_os = "macos")]
    #[test]
    fn spike_runs_through_ffi() {
        let (mut a, mut b, mut c) = (0.0, 0.0, 0.0);
        let rc = promo_gpu_spike_run(64, 64, &mut a, &mut b, &mut c);
        assert_eq!(rc, 0, "spike rc");
        assert!(a > 0.0 && b > 0.0 && c > 0.0);
    }
}
