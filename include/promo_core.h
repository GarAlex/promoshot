/* promo-core C ABI — P0 surface. Hand-maintained until cbindgen automation
 * lands (Phase 1); additive-only until Phase 5. */
#ifndef PROMO_CORE_H
#define PROMO_CORE_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Version string, static storage (never free). "promo-core X.Y.Z". */
const char *promo_core_version(void);

/* No-op round trip (liveness probe / FFI-overhead bench): returns x + 1. */
uint64_t promo_ffi_noop(uint64_t x);

/* Runs the IOSurface<->wgpu interop spike (macOS). 0 = ok; fills timings in
 * microseconds. -1 unsupported platform, -2 no GPU, -3 spike failed. */
int32_t promo_gpu_spike_run(int32_t width, int32_t height,
                            double *out_import_us, double *out_render_us,
                            double *out_readback_us);

#ifdef __cplusplus
}
#endif

#endif /* PROMO_CORE_H */
