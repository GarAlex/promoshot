/* promo-core C ABI — P0 surface. Hand-maintained until cbindgen automation
 * lands (Phase 1); additive-only until Phase 5. */
#ifndef PROMO_CORE_H
#define PROMO_CORE_H

#include <stdint.h>
#include <stddef.h>

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

/* ---- Phase 1: project handle + timeline queries ------------------------- */

/* Opaque parsed project. */
typedef struct PromoProject PromoProject;

/* Parses a metadata.json payload (NUL-terminated UTF-8). NULL on parse
 * failure. Free with promo_project_free. */
PromoProject *promo_project_parse(const char *json);
void promo_project_free(PromoProject *project);

/* Re-encodes the project as JSON. Free with promo_string_free. */
char *promo_project_to_json(const PromoProject *project);
void promo_string_free(char *s);

/* Evaluates every timeline query at each time (seconds) and returns one JSON
 * document (parity-harness surface). Free with promo_string_free. */
char *promo_timeline_eval(const PromoProject *project, const double *times,
                          size_t times_len);

/* Hot-path queries (no serialization). Layer/resource indices follow the
 * project's stored array order. */

/* out[3] = zoom, verticalShift, horizontalShift. 0 ok, -1 bad input. */
int32_t promo_layer_transform(const PromoProject *project, int32_t layer_index,
                              double time, double *out);

/* Source-media time for a resource-local output time. -1.0 on bad input. */
double promo_resource_source_time(const PromoProject *project,
                                  int32_t resource_index, double local_time);

/* out[4] = sourceStart, localStart, localEnd, rate. 0 ok, -1 bad input. */
int32_t promo_resource_video_segment(const PromoProject *project,
                                     int32_t resource_index, double local_time,
                                     double *out);

/* Resolved layer volume at a layer-local time (baseline default_gain). */
float promo_layer_gain(const PromoProject *project, int32_t layer_index,
                       double local_time, float default_gain);

/* Index of the resource a layer references, or -1. */
int32_t promo_layer_resource_index(const PromoProject *project,
                                   int32_t layer_index);

#ifdef __cplusplus
}
#endif

#endif /* PROMO_CORE_H */
