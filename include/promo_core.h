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

/* Layer visibility at a composition time: 1 visible, 0 hidden. */
int32_t promo_layer_is_visible(const PromoProject *project, int32_t layer_index,
                               double time);

/* Layout in canvas space, out[4] = x, y, width, height. Pure geometry — no
 * project handle. 0 ok, -1 bad out. */
int32_t promo_media_rect(double source_width, double source_height,
                         double canvas_width, double canvas_height, double zoom,
                         double horizontal_shift, double vertical_shift,
                         double *out);
int32_t promo_drawing_rect(double natural_width, double natural_height,
                           double canvas_width, double canvas_height,
                           double zoom, double horizontal_shift,
                           double vertical_shift, double *out);

/* Clockwise rotation in degrees at a composition time. 0 on bad input. */
double promo_layer_rotation(const PromoProject *project, int32_t layer_index,
                            double time);

/* Device-frame 2.5D tilt: out[2] = tiltX, tiltY (degrees). 0 when the layer
 * has tilt keyframes, -1 otherwise (use the frame's static tilt). */
int32_t promo_layer_tilt(const PromoProject *project, int32_t layer_index,
                         double time, double *out);

/* Background layer's resolved color at a time as straight RGBA 0..1:
 * out[4]. Falls back to the composition settings' background color.
 * 0 ok, -1 bad input. */
int32_t promo_layer_background_rgba(const PromoProject *project,
                                    int32_t layer_index, double time,
                                    double *out);

/* Index of the resource a layer references, or -1. */
int32_t promo_layer_resource_index(const PromoProject *project,
                                   int32_t layer_index);

/* ---- Phase 2: GPU compositor (macOS) ------------------------------------ */

/* Opaque compositor (GPU context + pipeline, reused across frames). */
typedef struct PromoCompositor PromoCompositor;

/* NULL when no GPU. Free with promo_compositor_free. */
PromoCompositor *promo_compositor_new(void);
void promo_compositor_free(PromoCompositor *compositor);

/* Renders one frame. scene_json: {canvasWidth, canvasHeight,
 * backgroundRGBA[4], outputWidth, outputHeight, barsRGBA[4], quads:[{texture
 * (index|absent=solid), rect[4], rotation, cornerRadius, borderWidth,
 * borderRGBA[4], solidRGBA[4], opacity}]}. surfaces are BGRA IOSurfaceRefs;
 * output_surface is a BGRA IOSurface of outputWidth x outputHeight.
 * 0 ok, -1 bad input, -2 scene parse, -3 surface import, -4 render. */
int32_t promo_compose_frame(PromoCompositor *compositor, const char *scene_json,
                            const void *const *surfaces,
                            const int32_t *surface_widths,
                            const int32_t *surface_heights,
                            size_t surface_count, void *output_surface);

/* An in-flight GPU submission (opaque); see promo_compositor_set_defer. */
typedef struct PromoSubmissionToken PromoSubmissionToken;

/* Binary-scene variant of promo_compose_frame (the 30-calls-per-output-second
 * export path; skips JSON). header: 12 doubles — canvasW, canvasH,
 * backgroundRGBA[4], outputW, outputH, barsRGBA[4]. quads: quad_count x 18
 * doubles — textureIndex (-1 = solid), rect[4], rotation, cornerRadius,
 * borderWidth, borderRGBA[4], solidRGBA[4], opacity, color709 (non-zero:
 * texture is BT.709-encoded video; the shader converts to sRGB after
 * sampling). out_token (optional, may be NULL): with deferred completion on,
 * receives the GPU fence — pass it to promo_submission_wait before reading
 * output_surface. Same surface contract and return codes as
 * promo_compose_frame. */
int32_t promo_compose_frame_raw(PromoCompositor *compositor,
                                const double *header, const double *quads,
                                size_t quad_count, const void *const *surfaces,
                                const int32_t *surface_widths,
                                const int32_t *surface_heights,
                                size_t surface_count, void *output_surface,
                                PromoSubmissionToken **out_token);

/* Deferred completion: compose submits and returns a token instead of
 * blocking on the GPU, so the caller can do other work (decode the next
 * frame) while it renders. Only for pipelines that render and read on
 * different threads. 0 ok, -1 bad handle. */
int32_t promo_compositor_set_defer(PromoCompositor *compositor, int32_t defer);

/* Blocks until the submission finishes and frees the token. NULL is a
 * no-op. 0 ok, -1 no GPU. */
int32_t promo_submission_wait(PromoSubmissionToken *token);

/* ---- Vector drawings (macOS): GPU tessellation --------------------------- */

/* Opaque vector renderer (GPU context + mesh pipeline, reused). */
typedef struct PromoVector PromoVector;

/* NULL when no GPU. Free with promo_vector_free. */
PromoVector *promo_vector_new(void);
void promo_vector_free(PromoVector *renderer);

/* Tessellates a DrawingDocument JSON payload and renders it into
 * output_surface (BGRA IOSurface, width x height), content bounds scaled to
 * fill. The surface is cleared to transparent first, edges are 4x MSAA
 * antialiased, and the result composites over the frame as a quad — so a
 * drawing can be re-rendered crisply at any size instead of magnifying a
 * pre-baked bitmap. 0 ok, -1 bad input, -2 parse, -4 render. */
int32_t promo_vector_render(PromoVector *renderer, const char *doc_json,
                            void *output_surface, int32_t width,
                            int32_t height);

/* The drawing's natural bounds: out[4] = x, y, width, height (the
 * 1080x1920 fallback for an empty document, matching the Swift model).
 * 0 ok, -1 bad input, -2 parse. */
int32_t promo_vector_content_bounds(const char *doc_json, double *out);

/* ---- Phase 3: preview engine (macOS) ------------------------------------ */

typedef struct PromoPreview PromoPreview;

/* How a host hands a frame over. Fill `kind` and the fields that kind uses;
 * everything else stays zero. One struct for every platform, so the provider
 * contract does not name one.
 *
 * Ownership: the engine retains an IOSURFACE for as long as it caches the
 * frame, and COPIES cpu pixels during the call — so a host may reuse its
 * pixel buffer as soon as the provider returns. */
typedef struct {
  int32_t kind;           /* PROMO_SURFACE_*; 0 = no frame                  */
  void *handle;           /* IOSURFACE: IOSurfaceRef. D3D_HANDLE: NT handle */
  int32_t fd;             /* DMABUF: file descriptor                        */
  const uint8_t *data;    /* CPU_PIXELS: BGRA rows                          */
  uint32_t width;
  uint32_t height;
  uint32_t bytes_per_row; /* CPU_PIXELS: stride; width * 4 when unpadded    */
} PromoHostSurface;

#define PROMO_SURFACE_NONE 0
#define PROMO_SURFACE_IOSURFACE 1
#define PROMO_SURFACE_D3D_HANDLE 2
#define PROMO_SURFACE_DMABUF 3
#define PROMO_SURFACE_CPU_PIXELS 4

/* Host frame provider. layer_id is NUL-terminated; source_time < 0 means
 * static content (image/drawing layers). Fill *out_surface (see
 * PromoHostSurface) and optional flags (bit 1 = pre-framed: engine skips
 * radius/border). Return 0 on success, non-zero to skip the layer for this
 * render.
 *
 * CHANGED (2026-08-14): out_surface was `void **` taking a bare IOSurfaceRef.
 * Hosts must now fill the struct — writing a pointer through the old
 * signature lands in `kind` and corrupts the descriptor. */
typedef int32_t (*PromoFrameProvider)(void *user, const char *layer_id,
                                      double source_time, int32_t tier,
                                      PromoHostSurface *out_surface,
                                      int32_t *out_flags);

/* Layer opacity (0..1) at a composition time; 1 when unkeyed. */
double promo_layer_opacity(const PromoProject *project, int32_t layer_index,
                           double time);

/* Validates a metadata.json payload: NULL when valid, else a message naming
 * the first problem (free with promo_string_free). promo_project_parse only
 * says yes or no, which is not something an editor can show a person. */
char *promo_project_validate(const char *json);

/* --- editor layer (promo-editor) ---------------------------------------
 * Editor calls are rare and small, so they cross as JSON. Rust front ends
 * depend on promo-editor directly and skip all of this.
 *
 * promo_lanes_pack input:
 *   {"layers": [...], "totalDuration": s, "gutter": s,
 *    "viewport": {"center": s, "span": s|null, "total": s}|null,
 *    "alwaysInclude": ["id", ...]}
 * output: [{"rowID", "kind", "indexWithinKind", "layerIDs"}] — free with
 * promo_string_free. NULL on malformed input. */
char *promo_lanes_pack(const char *params_json);
/* Row holding layer_id after the same packing, or NULL if focus dropped it. */
char *promo_lanes_row_id(const char *params_json, const char *layer_id);
/* Lane policy, so every front end draws the same conclusion from a width. */
int32_t promo_lanes_fit(double timeline_width);
int32_t promo_lanes_compact_labels(double timeline_width);

/* Selection rules (Stage 1 slice 1.2). Stateless: the front end still holds
 * the selection and passes it in — the core owns the rules, not the state.
 *
 * promo_selection_reveal_anchor: promo_lanes_pack's JSON plus
 *   {"target": "id", "usesLanes": bool}; returns
 *   {"kind": "row"|"layer", "value": "..."} or NULL when focus dropped it.
 * promo_selection_is_pinned_outside_window:
 *   {"layer": {...}, "selectedID": "id"|null, "viewport": {...},
 *    "windowActive": bool} -> 1 when on screen only because it is pinned.
 * promo_selection_reconcile: {"orderedIDs": [...], "selectedID": "id"|null}
 *   -> the selection after a list change, or NULL for an empty project. */
char *promo_selection_reveal_anchor(const char *params_json);
int32_t promo_selection_is_pinned_outside_window(const char *params_json);
char *promo_selection_reconcile(const char *params_json);

/* Transport state machine (Stage 1 slice 1.3). Stateless: the host holds the
 * machine's state and hands it over per event.
 *
 * promo_transport_step input:
 *   {"state": "idle"|"playing"|"scrubbing", "time": s, "duration": s,
 *    "resumeAfterScrub": bool, "seekGeneration": n,
 *    "event": {"kind": "play"|"pause"|"toggle"|"tick"|"beginScrub"|
 *                      "scrubTo"|"endScrub"|"seek"|"setDuration",
 *              "time": s?, "duration": s?}}
 * output: the next state plus ordered effects —
 *   {"state","time","duration","resumeAfterScrub","seekGeneration",
 *    "effects":[{"kind":"seek","time","generation"} |
 *               {"kind":"startPlayback","at"} | {"kind":"stopPlayback"}]}
 * Free with promo_string_free; NULL on malformed input or unknown event.
 *
 * promo_transport_seek_is_current: use INSTEAD of the player's "finished"
 * flag, which is false both for a superseded seek and for one whose item was
 * not ready — branching on it is what strands playback. */
char *promo_transport_step(const char *params_json);
int32_t promo_transport_seek_is_current(uint64_t generation, uint64_t current);

/* Field offsets of PromoHostSurface, so a host that mirrors the struct in
 * another language can assert its layout matches rather than assume it.
 * field: 0 = sizeof, 1 = kind, 2 = handle, 3 = fd, 4 = data, 5 = width,
 * 6 = height, 7 = bytes_per_row. UINT64_MAX if unknown. */
uint64_t promo_host_surface_layout(int32_t field);

/* Creates a preview engine for a metadata.json payload. budget_bytes caps
 * the frame cache (LRU eviction). NULL on parse/GPU failure. */
PromoPreview *promo_preview_new(const char *project_json,
                                PromoFrameProvider provider, void *user,
                                uint64_t budget_bytes);
void promo_preview_free(PromoPreview *preview);

/* Renders the composition at `time` into a BGRA IOSurface (canvas
 * aspect-fit inside width x height). 0 ok, -1 bad input, -4 render failed. */
int32_t promo_preview_render(PromoPreview *preview, double time,
                             void *output_surface, int32_t width,
                             int32_t height);

/* promo_preview_render plus a host-rasterized caption/watermark overlay
 * (BGRA IOSurface in canvas space) composited last — the same final quad the
 * export path adds, so an in-app preview matches the exported frame instead
 * of approximating captions with host-drawn text. Pass NULL for none.
 * 0 ok, -1 bad input, -4 render failed. */
int32_t promo_preview_render_with_overlay(PromoPreview *preview, double time,
                                          void *output_surface, int32_t width,
                                          int32_t height, void *overlay_surface,
                                          int32_t overlay_width,
                                          int32_t overlay_height);

/* Re-targets the frame-cache budget (bytes) — size it from the machine's
 * RAM and shrink it under memory pressure. 0 ok, -1 bad handle. */
int32_t promo_preview_set_cache_budget(PromoPreview *preview, uint64_t bytes);

/* Decodes-ahead for `time` (fills the frame cache without composing).
 * Returns newly fetched frame count, or -1 on bad handle. */
int32_t promo_preview_prefetch(PromoPreview *preview, double time);

/* Proxy tier for subsequent video-frame requests (0 = full res; raise while
 * scrubbing, drop back for the paused full-res refine). Cache entries are
 * keyed per tier. 0 ok, -1 bad handle. */
int32_t promo_preview_set_tier(PromoPreview *preview, int32_t tier);

/* out[4] = cache hits, misses, cached bytes, evictions. */
int32_t promo_preview_stats(const PromoPreview *preview, uint64_t *out);

#ifdef __cplusplus
}
#endif

#endif /* PROMO_CORE_H */
