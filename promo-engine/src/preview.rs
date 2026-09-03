//! The preview engine (Phase 3, slice 1): renders the composition at an
//! arbitrary time into an IOSurface, pulling layer frames from a
//! host-provided decoder callback and caching them (as adopted GPU textures)
//! under a MemoryGovernor budget.
//!
//! Division of labor with the host (mirrors the P2 stills split):
//! - The host resolves what bitmap a layer shows (image cuts, orientation,
//!   baked device frames, rasterized drawings, decoded video frames) and
//!   hands it over as a BGRA IOSurface. `PRE_FRAMED` in the provider's
//!   out-flags means "already framed — skip radius/border".
//! - The engine owns timing (trim/pause/loop source-time mapping), keyframe
//!   interpolation, layout, z-order, caching, and GPU composition.

use crate::governor::MemoryGovernor;
use promo_gpu::compositor::{Compositor, InputTexture, Scene, SceneQuad};
use promo_gpu::model_pass::{GpuModel, MaterialInput, MeshInput, ModelPass, ModelView};
use promo_gpu::{GpuSurface, ImportedFrame};
// Only the Apple-typed render entries name this; the provider no longer does.
use crate::vector::vector_shapes;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use promo_gpu::iosurface::IOSurfaceRef;
use promo_gpu::{GpuContext, GpuError};
use promo_model::{ProjectLayer, ProjectLayerKind, ProjectMetadata, ProjectResource, Size};
use promo_timeline as tl;
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CString};

/// Provider out-flag: the bitmap already carries its decorative frame —
/// the engine must not apply corner radius or border over it.
pub const FLAG_PRE_FRAMED: i32 = 1;

/// Provider out-flag: the bitmap holds BT.709-encoded video — the shader
/// converts to sRGB while sampling (the export decoder's zero-copy route).
pub const FLAG_COLOR_709: i32 = 2;

/// How a host hands a frame over: one C-ABI struct covering every surface
/// kind, so the provider contract does not name a platform.
///
/// The host fills `kind` and the fields that kind uses. Ownership: the engine
/// retains an `IOSURFACE` for as long as it caches the frame, and **copies**
/// `CPU_PIXELS` during the call — so a host may reuse its pixel buffer the
/// moment the provider returns.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct HostSurface {
    /// One of the `SURFACE_*` constants. 0 means "no frame".
    pub kind: i32,
    /// `SURFACE_IOSURFACE`: an `IOSurfaceRef`.
    /// `SURFACE_D3D_HANDLE`: an NT shared handle.
    pub handle: *mut c_void,
    /// `SURFACE_DMABUF`: the DMA-BUF file descriptor.
    pub fd: i32,
    /// `SURFACE_CPU_PIXELS`: tightly-packed-or-padded BGRA rows.
    pub data: *const u8,
    pub width: u32,
    pub height: u32,
    /// `SURFACE_CPU_PIXELS`: row stride in bytes; `width * 4` when unpadded.
    pub bytes_per_row: u32,
}

pub const SURFACE_NONE: i32 = 0;
pub const SURFACE_IOSURFACE: i32 = 1;
pub const SURFACE_D3D_HANDLE: i32 = 2;
pub const SURFACE_DMABUF: i32 = 3;
pub const SURFACE_CPU_PIXELS: i32 = 4;

impl Default for HostSurface {
    fn default() -> Self {
        Self {
            kind: SURFACE_NONE,
            handle: std::ptr::null_mut(),
            fd: -1,
            data: std::ptr::null(),
            width: 0,
            height: 0,
            bytes_per_row: 0,
        }
    }
}

impl HostSurface {
    /// Borrowed view of what the host filled in. `None` when the descriptor
    /// is empty or inconsistent — a bad descriptor skips the layer rather
    /// than reading whatever the pointer happens to address.
    ///
    /// # Safety
    /// For `SURFACE_CPU_PIXELS`, `data` must address at least
    /// `bytes_per_row * height` readable bytes for the duration of the call.
    unsafe fn to_gpu_surface(self) -> Option<GpuSurface> {
        match self.kind {
            SURFACE_IOSURFACE if !self.handle.is_null() => {
                Some(GpuSurface::IoSurface { raw: self.handle })
            }
            SURFACE_D3D_HANDLE if !self.handle.is_null() => {
                Some(GpuSurface::D3DSharedHandle { raw: self.handle })
            }
            SURFACE_DMABUF if self.fd >= 0 => Some(GpuSurface::DmaBuf { fd: self.fd }),
            SURFACE_CPU_PIXELS if !self.data.is_null() && self.width > 0 && self.height > 0 => {
                let stride = if self.bytes_per_row == 0 {
                    self.width * 4
                } else {
                    self.bytes_per_row
                };
                let len = stride as usize * self.height as usize;
                Some(GpuSurface::CpuPixels {
                    data: std::slice::from_raw_parts(self.data, len).to_vec(),
                    width: self.width,
                    height: self.height,
                    bytes_per_row: stride,
                })
            }
            _ => None,
        }
    }
}

/// Host frame provider. Called with the layer id (NUL-terminated), the
/// source-media time in seconds (or a negative value for static content),
/// and the proxy tier (0 = full resolution; higher = smaller proxies).
/// On success returns 0 and fills `out_surface` (see [`HostSurface`]) plus
/// optional flags. Non-zero return = no frame (the layer is skipped).
pub type FrameProviderFn = extern "C" fn(
    user: *mut c_void,
    layer_id: *const c_char,
    // resource_id: the resource showing at this time. A layer's keyframes
    // may swap it, so the layer alone no longer says what to hand over;
    // empty when the layer names none.
    resource_id: *const c_char,
    source_time: f64,
    tier: i32,
    out_surface: *mut HostSurface,
    out_flags: *mut i32,
) -> i32;

/// Material slots (by index) and the resource ids bound to them.
type SlotPictures = Vec<(usize, String)>;

/// A model the host provided: decoded, uploaded, and the colours its
/// slots were last painted (so a binding re-paints only when it changes).
struct LoadedModel {
    model: crate::model::Model,
    gpu: GpuModel,
    painted: HashMap<usize, [f32; 4]>,
    /// The frame id each slot was last given a picture from, so a binding
    /// re-binds only when the picture changes.
    bound: HashMap<usize, u64>,
}

struct CachedFrame {
    /// Owns the texture and whatever must outlive it (the IOSurface retain on
    /// Apple, nothing for an upload) — see `promo_gpu::ImportedFrame`.
    frame: ImportedFrame,
    flags: i32,
    /// Captions only: canvas-space top-left, so a cache hit rebuilds the quad
    /// without laying the text out again.
    caption_origin: Option<(f64, f64)>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PreviewStats {
    pub hits: u64,
    pub misses: u64,
    pub cached_bytes: u64,
    pub evictions: u64,
}

pub struct PreviewEngine {
    meta: ProjectMetadata,
    ctx: &'static GpuContext,
    compositor: Compositor,
    provider: FrameProviderFn,
    user: *mut c_void,
    governor: MemoryGovernor,
    cache: HashMap<u64, CachedFrame>,
    /// Where every reveal unit sits, per caption raster key.
    ///
    /// Shaping the text is the expensive half and a typewriter asks for the
    /// same answer every frame, so the geometry is cached beside the raster
    /// it describes rather than recomputed 60 times a second.
    reveal_cache: HashMap<String, Option<promo_text::RevealLayout>>,
    /// Models by resource id, decoded and on the GPU — handed in by the
    /// host through `provide_model` (the file is the host's I/O, as every
    /// pixel source is), drawn by the model pass per frame.
    models: HashMap<String, LoadedModel>,
    /// Built on the first model; None until then, so a project with no
    /// models pays nothing.
    model_pass: Option<ModelPass>,
    /// Quads a layer adds ABOVE its own — click rings — drained into the
    /// scene right after the layer's quad.
    overlays: Vec<SceneQuad>,
    key_of: HashMap<(String, i64, i32), u64>,
    id_of: HashMap<u64, (String, i64, i32)>,
    next_id: u64,
    hits: u64,
    misses: u64,
    /// Proxy tier requested from the provider for video frames: 0 = full
    /// resolution, higher = smaller proxies. The host raises it while
    /// scrubbing/playing and drops it back to 0 for the paused refine
    /// (cache entries are keyed per tier, so both coexist).
    preferred_tier: i32,
    /// Render the project over nothing (an alpha export).
    transparent_plate: bool,
    /// Export mode: the clock is monotonic, so per-time frames (video,
    /// animated-tilt bakes) are never requested twice — caching them only
    /// evicts the static content that IS reused. They go into `scratch`
    /// instead, which lives exactly one render.
    export_mode: bool,
    /// Per-time frames for the render in flight (export mode only). Cleared
    /// at the START of the next build, not the end of this one, so a
    /// deferred-fence compose still has live textures while the GPU works.
    scratch: HashMap<u64, CachedFrame>,
    /// Density multiplier for canvas-space rasters the engine creates
    /// (captions). Export renders a small canvas into a large output; without
    /// this the GPU magnifies the caption raster and text arrives soft.
    raster_scale: f64,
    /// Vector rasterizer for drawing layers, built on first use — a project
    /// without drawings never pays for the pipeline.
    vector: Option<promo_gpu::vector::VectorRenderer>,
    /// The sub-sample time a blurred layer's GEOMETRY resolves at, while a
    /// motion-blur render walks the shutter. None outside that walk. Layers
    /// without `motionBlur` ignore it entirely — that is what pins them
    /// sharp — and source decode ALWAYS uses the frame's own time, so the
    /// walk re-rasterises but never re-decodes.
    blur_sample: Option<f64>,
    /// Set for the second and later sub-builds of one render, so the scratch
    /// (and the frames a just-submitted compose still samples) survives the
    /// whole walk instead of dying at each sub-build.
    retain_scratch: bool,
    /// Scene builds since creation. The early-out's whole contract is
    /// "a still frame pays one build, not a walk" — invisible in pixels
    /// (the accumulator reproduces a still frame exactly), so the tests
    /// read the cost instead.
    builds: u64,
    /// Key → id for scratch entries. Export mode skips the governed cache by
    /// design (a monotonic clock never re-asks) — but a blur walk DOES
    /// re-ask, N times, and without this memo each sub-build would decode
    /// the same frame again.
    scratch_key: HashMap<(String, i64, i32), u64>,
    /// The frame each layer+resource most recently SHOWED, for the live
    /// canvas's miss path: a host decode that fails or arrives late must
    /// hold the last picture, not blank the layer for one render — a long
    /// video's mid-GOP seeks miss often enough that the drop-out reads as
    /// BLINKING during playback. Best-effort: the id may have been evicted
    /// since (checked before use). Export/transient renders never consult
    /// it — a stale frame silently written into a delivered file would
    /// hide the decode failure instead of surfacing it.
    recent_by_subject: HashMap<String, u64>,
}

// The raw `user` pointer is owned by the host and promised valid for the
// engine's lifetime (FFI contract).
unsafe impl Send for PreviewEngine {}

/// Cache-time quantization: 1 ms buckets (finer than any output frame rate).
fn quantize(source_time: f64) -> i64 {
    if source_time < 0.0 {
        -1
    } else {
        (source_time * 1000.0).round() as i64
    }
}

impl PreviewEngine {
    pub fn new(
        meta: ProjectMetadata,
        provider: FrameProviderFn,
        user: *mut c_void,
        budget_bytes: usize,
    ) -> Result<Self, GpuError> {
        let ctx = GpuContext::shared().ok_or(GpuError::NoAdapter)?;
        let compositor = Compositor::new(ctx)?;
        Ok(PreviewEngine {
            meta,
            ctx,
            compositor,
            provider,
            user,
            governor: MemoryGovernor::new(budget_bytes),
            cache: HashMap::new(),
            reveal_cache: HashMap::new(),
            models: HashMap::new(),
            model_pass: None,
            overlays: Vec::new(),
            key_of: HashMap::new(),
            id_of: HashMap::new(),
            next_id: 1,
            hits: 0,
            misses: 0,
            preferred_tier: 0,
            transparent_plate: false,
            export_mode: false,
            scratch: HashMap::new(),
            raster_scale: 1.0,
            vector: None,
            blur_sample: None,
            retain_scratch: false,
            builds: 0,
            scratch_key: HashMap::new(),
            recent_by_subject: HashMap::new(),
        })
    }

    /// Export mode: per-time frames bypass the LRU cache (see `scratch`).
    /// Static content — images, drawings, caption rasters — stays cached and
    /// is what makes frame N+1 cheap.
    pub fn set_export_mode(&mut self, enabled: bool) {
        self.export_mode = enabled;
        if !enabled {
            self.scratch.clear();
        }
    }

    /// Density for engine-made canvas-space rasters (captions): the export's
    /// letterbox scale, so text is rasterized at output resolution. 1.0 for
    /// preview. Values are clamped to a sane positive range.
    pub fn set_raster_scale(&mut self, scale: f64) {
        self.raster_scale = if scale.is_finite() {
            scale.clamp(0.1, 8.0)
        } else {
            1.0
        };
    }

    /// Deferred completion for the compose (see
    /// `Compositor::set_defer_completion`): render returns as soon as the GPU
    /// work is submitted and [`take_fence`](Self::take_fence) hands the caller
    /// what to wait on before reading the output. Export-pipeline only.
    pub fn set_defer_completion(&mut self, defer: bool) {
        self.compositor.set_defer_completion(defer);
    }

    /// The pending fence for the last deferred render, if any.
    pub fn take_fence(&mut self) -> Option<promo_gpu::compositor::Fence> {
        self.compositor.take_fence()
    }

    /// Swaps in an edited project without rebuilding the GPU pipeline.
    ///
    /// The editor re-syncs on every change (a drag does this per frame), and
    /// recreating the engine there would recreate the compositor and drop the
    /// whole frame cache. This keeps both, and evicts cached frames only for
    /// layers that actually changed — the engine holds the old and new
    /// metadata, so it can diff them precisely instead of guessing.
    pub fn set_project(&mut self, meta: ProjectMetadata) {
        let old = self.meta.layers.clone().unwrap_or_default();
        let new = meta.layers.clone().unwrap_or_default();
        let old_resources = self.meta.resources.clone().unwrap_or_default();
        let new_resources = meta.resources.clone().unwrap_or_default();
        // A layer whose RESOURCE changed is as stale as one that changed
        // itself: an image cut's rect, a device frame's material, a caption
        // resource's text all live there, and the layer struct is untouched
        // when they move. Comparing only layers kept the old bake on screen.
        let resource_changed = |layer: &ProjectLayer| -> bool {
            let Some(rid) = layer.resource_id.as_ref() else {
                return false;
            };
            let before = old_resources.iter().find(|r| &r.id == rid);
            let after = new_resources.iter().find(|r| &r.id == rid);
            before != after
        };
        let mut stale: Vec<String> = Vec::new();
        for layer in &old {
            match new.iter().find(|l| l.id == layer.id) {
                Some(updated) if updated == layer && !resource_changed(layer) => {}
                // Changed or removed: its cached frames may no longer apply.
                _ => stale.push(layer.id.clone()),
            }
        }
        // Settings changes (canvas size, defaults) affect every layer's layout.
        let settings_changed = meta.composition_settings != self.meta.composition_settings;
        if settings_changed {
            stale = old.iter().map(|l| l.id.clone()).collect();
        }
        for id in stale {
            self.evict_layer(&id);
        }
        // Mask rasters are keyed `mask:{resource}:…`, per RESOURCE on purpose
        // so two layers wearing the same mask share one texture — which puts
        // them beyond `evict_layer`'s reach, and beyond `resource_changed`
        // above, which only follows `layer.resource_id` and never
        // `mask_resource_id`. The stamp in the key is what makes an edited
        // mask VISIBLE; this only reclaims the superseded raster now instead
        // of leaving it for the governor's LRU to notice under pressure. One
        // is worth reclaiming: a mask raster is capped at twice the canvas'
        // long side per axis.
        for old_res in &old_resources {
            if new_resources.iter().find(|r| r.id == old_res.id) != Some(old_res) {
                self.evict_mask(&old_res.id);
            }
        }
        self.meta = meta;
    }

    /// Drops the cached rasters of a mask DRAWING, keyed `mask:{resource}:…`.
    ///
    /// Separate from `evict_layer` because a mask belongs to its RESOURCE,
    /// not to any one layer wearing it — the key is shared on purpose, and a
    /// layer-scoped sweep cannot express "this drawing changed".
    fn evict_mask(&mut self, resource_id: &str) {
        let prefix = format!("mask:{resource_id}:");
        let victims: Vec<u64> = self
            .id_of
            .iter()
            .filter(|(_, (id, _, _))| id.starts_with(&prefix))
            .map(|(entry, _)| *entry)
            .collect();
        for entry in victims {
            if let Some(key) = self.id_of.remove(&entry) {
                self.key_of.remove(&key);
            }
            self.governor.remove(entry);
            self.cache.remove(&entry);
        }
    }

    /// Drops every cached frame belonging to `layer_id` — media frames keyed
    /// `{id}\u{1f}{resource}` AND caption rasters keyed `caption:{id}:…`.
    ///
    /// Both suffixes have caught this function out. A bare-id match once
    /// skipped the captions, so stale text survived every edit; then the
    /// media key gained a resource suffix (a keyframe can swap what a layer
    /// shows) and the same exact match stopped finding those too. Anything
    /// keyed by a layer has to be matched as a PREFIX.
    fn evict_layer(&mut self, layer_id: &str) {
        let caption_prefix = format!("caption:{layer_id}:");
        let media_prefix = format!("{layer_id}\u{1f}");
        let victims: Vec<u64> = self
            .id_of
            .iter()
            .filter(|(_, (id, _, _))| {
                id == layer_id || id.starts_with(&media_prefix) || id.starts_with(&caption_prefix)
            })
            .map(|(entry, _)| *entry)
            .collect();
        for entry in victims {
            if let Some(key) = self.id_of.remove(&entry) {
                self.key_of.remove(&key);
            }
            self.governor.remove(entry);
            self.cache.remove(&entry);
        }
    }

    /// Sets the proxy tier used for subsequent video-frame requests.
    /// Render the project over nothing: no settings colour, no gradient,
    /// so an alpha export keeps what the layers drew and nothing else. A
    /// background LAYER still paints.
    pub fn set_transparent_plate(&mut self, on: bool) {
        self.transparent_plate = on;
    }

    pub fn set_preferred_tier(&mut self, tier: i32) {
        self.preferred_tier = tier.max(0);
    }

    /// The decode tier a layer needs at `time`. Stills always decode once at
    /// full resolution; video follows the host's preferred tier — except
    /// when a keyframe viewport magnifies the source past what the 540p
    /// scrub proxy can supply. The host sets its tier from canvas scale and
    /// knows nothing about per-layer windows, so at 2× the user would be
    /// aiming a precision rectangle at proxy blur blown up 2×. Past 1.5×
    /// the layer escalates to tier 0 and pays the full decode (~77 ms
    /// uncached on 4K) rather than showing mush.
    fn tier_for(&self, layer: &ProjectLayer, time: f64) -> i32 {
        if layer.kind != ProjectLayerKind::Video {
            return 0;
        }
        if self.preferred_tier > 0 {
            let magnified = tl::layer_viewport(layer, time).is_some_and(|vp| vp[3] < 1.0 / 1.5);
            if magnified {
                return 0;
            }
        }
        self.preferred_tier
    }

    pub fn preferred_tier(&self) -> i32 {
        self.preferred_tier
    }

    /// Re-targets the frame-cache budget, dropping LRU frames if it shrank.
    pub fn set_cache_budget(&mut self, bytes: usize) {
        for victim in self.governor.set_budget(bytes) {
            if let Some(k) = self.id_of.remove(&victim) {
                self.key_of.remove(&k);
            }
            self.cache.remove(&victim);
        }
    }

    pub fn stats(&self) -> PreviewStats {
        PreviewStats {
            hits: self.hits,
            misses: self.misses,
            cached_bytes: self.governor.used() as u64,
            evictions: self.governor.evictions(),
        }
    }

    fn resource_for(&self, layer: &ProjectLayer) -> Option<&ProjectResource> {
        let rid = layer.resource_id.as_ref()?;
        self.meta.resources.as_ref()?.iter().find(|r| &r.id == rid)
    }

    /// True when an image layer's device frame animates its tilt — the baked
    /// bitmap then changes over time, so the layer can't be cached as static
    /// content. Mirrors Swift `effectiveFrame(forCutID:)`: the layer's image
    /// cut's frame wins over the resource-level frame.
    /// The decorative frame a layer's content carries, if any. Mirrors Swift
    /// `effectiveFrame(forCutID:)`: the layer's image cut's frame wins over
    /// the resource-level frame.
    fn effective_frame(&self, layer: &ProjectLayer) -> Option<&promo_model::ResourceFrame> {
        let res = self.resource_for(layer)?;
        layer
            .image_cut_id
            .as_ref()
            .and_then(|cid| res.image_cuts.iter().find(|c| &c.id == cid))
            .and_then(|cut| cut.frame.as_ref())
            .or(res.frame.as_ref())
    }

    fn image_has_animated_tilt(&self, layer: &ProjectLayer) -> bool {
        if layer.kind != ProjectLayerKind::Image || !tl::layer_has_tilt_keyframes(layer) {
            return false;
        }
        matches!(
            self.effective_frame(layer),
            Some(f) if f.kind == promo_model::ResourceFrameKind::Device
        )
    }

    /// A frame the scene refers to, whichever store it lives in.
    fn cached_frame(&self, id: u64) -> &CachedFrame {
        self.cache
            .get(&id)
            .or_else(|| self.scratch.get(&id))
            .expect("scene refers to a frame the engine no longer holds")
    }

    /// Fetches (or serves from cache) the frame for `layer` at `source_time`.
    /// `pinned`: ids the in-flight scene already holds — they must survive
    /// this admit's eviction or the scene would point at freed frames.
    fn frame(
        &mut self,
        layer_id: &str,
        resource_id: &str,
        source_time: f64,
        tier: i32,
        pinned: &[u64],
    ) -> Option<u64> {
        // Export mode: a per-time frame will never be asked for again (the
        // export clock is monotonic), so it skips the cache entirely.
        let transient = self.export_mode && source_time >= 0.0;
        // Keyed by RESOURCE as well as layer: a keyframe can swap what a
        // layer shows, and an image asks at a fixed source time, so a
        // layer-only key would answer every frame after the swap with the
        // bitmap from before it.
        let key = (
            format!("{layer_id}\u{1f}{resource_id}"),
            quantize(source_time),
            tier,
        );
        if transient {
            // A blur walk asks for the same frame once per sub-build; the
            // monotonic-clock argument for skipping the cache does not
            // survive N sub-samples of one instant.
            if let Some(&id) = self.scratch_key.get(&key) {
                self.hits += 1;
                return Some(id);
            }
        } else if let Some(&id) = self.key_of.get(&key) {
            self.governor.touch(id);
            self.hits += 1;
            self.recent_by_subject.insert(key.0.clone(), id);
            return Some(id);
        }

        let c_id = CString::new(layer_id).ok()?;
        let c_resource = CString::new(resource_id).ok()?;
        let mut surface = HostSurface::default();
        let mut flags: i32 = 0;
        let rc = (self.provider)(
            self.user,
            c_id.as_ptr(),
            c_resource.as_ptr(),
            source_time,
            tier,
            &mut surface,
            &mut flags,
        );
        if rc != 0 {
            // Hold the last picture rather than blanking the layer for one
            // render — see `recent_by_subject`. Never for transient
            // (export) frames: a delivered file must not paper over misses.
            return self.recent_frame(&key.0, transient);
        }
        // SAFETY: the provider contract requires a CPU_PIXELS descriptor to
        // address bytes_per_row * height readable bytes for this call; the
        // conversion copies them before returning.
        let Some(gpu_surface) = (unsafe { surface.to_gpu_surface() }) else {
            return self.recent_frame(&key.0, transient);
        };
        // One import entry point: retains and adopts on Apple, uploads
        // elsewhere, and hands back something that owns what it needs.
        let Ok(frame) = Compositor::import(self.ctx, &gpu_surface) else {
            return self.recent_frame(&key.0, transient);
        };
        let (width, height) = (frame.width as usize, frame.height as usize);

        self.misses += 1;
        let id = self.next_id;
        self.next_id += 1;
        let entry = CachedFrame {
            frame,
            flags,
            caption_origin: None,
        };
        if transient {
            self.scratch.insert(id, entry);
            self.scratch_key.insert(key, id);
            return Some(id);
        }
        for victim in self.governor.admit(id, width * height * 4, pinned) {
            if let Some(k) = self.id_of.remove(&victim) {
                self.key_of.remove(&k);
            }
            self.cache.remove(&victim);
        }
        self.cache.insert(id, entry);
        self.key_of.insert(key.clone(), id);
        self.recent_by_subject.insert(key.0.clone(), id);
        self.id_of.insert(id, key);
        Some(id)
    }

    /// The last frame this layer+resource actually showed, if it is still
    /// cached — the live canvas's answer to a provider miss. `None` in
    /// transient (export) mode, for a subject never yet shown, or once the
    /// governor has evicted the frame.
    fn recent_frame(&mut self, subject: &str, transient: bool) -> Option<u64> {
        if transient {
            return None;
        }
        let id = *self.recent_by_subject.get(subject)?;
        if !self.cache.contains_key(&id) {
            return None;
        }
        self.governor.touch(id);
        Some(id)
    }

    /// Decodes-ahead: fetches (and caches) the frames every visible video
    /// layer needs at `time`, without composing. Playback calls this for
    /// upcoming ticks so the render itself is all cache hits. Returns how
    /// many frames were newly fetched.
    pub fn prefetch(&mut self, time: f64) -> usize {
        let layers: Vec<ProjectLayer> = self.meta.layers.clone().unwrap_or_default();
        let mut fetched = 0;
        for layer in &layers {
            if layer.kind != ProjectLayerKind::Video || !tl::layer_is_visible(layer, time) {
                continue;
            }
            let local = tl::layer_local_time(layer, time);
            let source_time = match self.resource_for(layer) {
                Some(res) => {
                    let view = tl::resource_for_cut(res, layer.media_cut_id.as_deref());
                    tl::source_time_for_layer(&view, local, layer.beyond_end).unwrap_or(local)
                }
                None => local,
            };
            let before = self.misses;
            let resource_id =
                tl::layer_resource_id(layer, time, self.meta.resources.as_deref().unwrap_or(&[]))
                    .unwrap_or_default()
                    .to_string();
            let _ = self.frame(
                &layer.id,
                &resource_id,
                source_time,
                self.tier_for(layer, time),
                &[],
            );
            if self.misses > before {
                fetched += 1;
            }
        }
        fetched
    }

    /// Renders the composition at `time` into `output` (BGRA IOSurface of
    /// `output_width` × `output_height`; the canvas is aspect-fit inside).
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn render(
        &mut self,
        time: f64,
        output: IOSurfaceRef,
        output_width: u32,
        output_height: u32,
    ) -> Result<(), GpuError> {
        self.render_with_overlay(time, output, output_width, output_height, None)
    }

    /// The layer's image effects at `time`, onto its quad. Radii stay in
    /// canvas px — the compositor scales them to texels from the rect.
    fn apply_effects(quad: &mut SceneQuad, layer: &promo_model::ProjectLayer, time: f64) {
        if let Some(fx) = tl::layer_effects(layer, time) {
            quad.blur = fx.blur as f32;
            quad.blur_angle = fx.blur_angle.map(|a| a as f32);
            quad.glow = [
                fx.glow as f32,
                fx.glow_radius as f32,
                fx.glow_threshold as f32,
            ];
            quad.vignette = [fx.vignette as f32, fx.vignette_softness as f32];
            // A fresh pattern every frame: the seed walks with time.
            quad.grain = [
                fx.grain as f32,
                ((time * 1000.0).round() as i64 % 100_000) as f32,
            ];
            quad.sharpen = fx.sharpen as f32;
        }
    }

    fn blend_for(layer: &promo_model::ProjectLayer) -> promo_gpu::compositor::QuadBlend {
        use promo_gpu::compositor::QuadBlend;
        match layer.blend_mode {
            Some(promo_model::BlendMode::Multiply) => QuadBlend::Multiply,
            Some(promo_model::BlendMode::Screen) => QuadBlend::Screen,
            Some(promo_model::BlendMode::Add) => QuadBlend::Add,
            _ => QuadBlend::Normal,
        }
    }

    /// The layer's grade at `time` in the shader's units, or None when the
    /// grade is identity and the shader can skip it. Fed the GEOMETRY time
    /// under a blur walk, so a fade-to-grey smears with everything else.
    fn adjust_for(
        &self,
        layer: &promo_model::ProjectLayer,
        time: f64,
    ) -> Option<([f32; 4], [f32; 4])> {
        let resolved = tl::layer_adjustments(layer, time)?;
        let tint = resolved
            .tint_hex
            .as_deref()
            .map(|hex| rgba_from_hex(self.meta.composition_settings.resolve_color(hex)))
            .unwrap_or([1.0, 1.0, 1.0, 1.0]);
        Some((
            [
                resolved.saturation as f32,
                resolved.contrast as f32,
                resolved.brightness as f32,
                if resolved.tint_hex.is_some() {
                    resolved.tint_amount as f32
                } else {
                    0.0
                },
            ],
            tint,
        ))
    }

    /// The widest shutter any visible blurred layer asks for, in seconds.
    /// None when this instant needs no walk at all — the common case, and
    /// the zero-cost one.
    fn blur_shutter(&self, time: f64) -> Option<f64> {
        let layers = self.meta.layers.as_deref()?;
        let fps = self
            .meta
            .composition_settings
            .fps
            .filter(|f| *f > 0.0)
            .unwrap_or(30.0);
        let fraction = layers
            .iter()
            .filter(|l| tl::layer_is_visible(l, time))
            .filter_map(|l| tl::layer_shutter(l, time))
            .fold(0.0f64, f64::max);
        (fraction > 0.0).then_some(fraction / fps)
    }

    /// One sub-build of a blur walk: geometry at `sample`, identity at
    /// `centre`, and the scratch retained so frames a submitted compose
    /// still reads stay alive across the walk.
    fn build_sub(
        &mut self,
        centre: f64,
        sample: f64,
        retain: bool,
        output_width: u32,
        output_height: u32,
    ) -> Result<(Scene, Vec<u64>), GpuError> {
        self.blur_sample = Some(sample);
        self.retain_scratch = retain;
        let out = self.build_scene(centre, output_width, output_height);
        self.blur_sample = None;
        self.retain_scratch = false;
        out
    }

    /// The scenes one output frame needs: one, or a shutter's worth.
    ///
    /// The sample count is DERIVED, not authored: the shutter's two
    /// endpoint scenes are built first, the furthest any blurred quad
    /// travels between them is measured in output pixels, and the walk gets
    /// a sample roughly every two. The author has no way to know that
    /// number — it changes with the canvas, the output size and the move —
    /// which is the placement lesson again. Too few samples is not "less
    /// blur", it is N distinct ghosts.
    fn build_scenes(
        &mut self,
        time: f64,
        output_width: u32,
        output_height: u32,
    ) -> Result<Vec<(Scene, Vec<u64>)>, GpuError> {
        let Some(shutter) = self.blur_shutter(time) else {
            return Ok(vec![self.build_scene(time, output_width, output_height)?]);
        };
        // Centred on the frame, the way a renderer's -0.5 shutter offset
        // is: the sharp position stays where a sharp frame would put it.
        let start = time - shutter / 2.0;
        let end = time + shutter / 2.0;
        let first = self.build_sub(time, start, false, output_width, output_height)?;
        let last = self.build_sub(time, end, true, output_width, output_height)?;

        // A whip-speed push crosses the canvas in a frame; 24 samples of
        // that is not "less blur", it is 24 visible ghosts — the first
        // template to push under a full shutter proved it. Export affords
        // the honest count; preview stays cheap and the proxy-tier stance
        // covers the difference.
        let cap = if self.export_mode { 64 } else { 8 };
        let count = match scene_displacement(&first.0, &last.0) {
            // Nothing moves as far as a pixel: the average IS the sharp
            // frame, so render that and pay nothing. Retain the scratch —
            // the walk's frames are this frame's frames.
            Some(d) if d < 0.75 => {
                return Ok(vec![self.build_sub(
                    time,
                    time,
                    true,
                    output_width,
                    output_height,
                )?]);
            }
            Some(d) => (((d / 2.0).ceil() as usize) + 1).clamp(3, cap),
            // The endpoint scenes disagree about how many quads exist (a
            // reveal unit arrived mid-shutter): no measurement, so take the
            // cap rather than guess low.
            None => cap,
        };

        let mut scenes = Vec::with_capacity(count);
        scenes.push(first);
        for j in 1..count - 1 {
            let tau = start + shutter * j as f64 / (count - 1) as f64;
            scenes.push(self.build_sub(time, tau, true, output_width, output_height)?);
        }
        scenes.push(last);
        Ok(scenes)
    }

    /// Renders with a host-rasterized overlay (captions + watermark) composited
    /// last, over everything — the same final quad the export path adds, so a
    /// preview built this way matches the exported frame instead of
    /// approximating it with separate host-drawn text.
    ///
    /// Apple-only because the overlay arrives as an IOSurface and rides the
    /// compositor's adoption cache (a stable overlay then costs nothing per
    /// frame). The portable entry is [`render_to_texture`](Self::render_to_texture);
    /// overlays reach it when caption rasterization is portable.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn render_with_overlay(
        &mut self,
        time: f64,
        output: IOSurfaceRef,
        output_width: u32,
        output_height: u32,
        overlay: Option<(IOSurfaceRef, u32, u32)>,
    ) -> Result<(), GpuError> {
        let mut scenes = self.build_scenes(time, output_width, output_height)?;
        let canvas = Size::new(
            self.meta.composition_settings.canvas_width,
            self.meta.composition_settings.canvas_height,
        );
        // The overlay rides EVERY sub-sample identically. The average of N
        // identical overlays over N varying scenes is the overlay over the
        // average — exact, so blur needs no special casing here.
        let overlay_texture = match overlay {
            Some((surface, width, height)) => Some(
                self.compositor
                    .import_iosurface_cached(self.ctx, surface, width, height)?,
            ),
            None => None,
        };
        if overlay_texture.is_some() {
            for (scene, used) in &mut scenes {
                scene.quads.push(SceneQuad {
                    texture: Some(used.len()),
                    rect: [0.0, 0.0, canvas.width(), canvas.height()],
                    ..Default::default()
                });
            }
        }

        let count = scenes.len();
        for (index, (scene, used)) in scenes.iter().enumerate() {
            // Field-disjoint lookup (not the `cached_frame` helper): the
            // closure may only borrow the two maps, because
            // `self.compositor` is borrowed mutably for the compose below.
            let mut textures: Vec<&InputTexture> = used
                .iter()
                .map(|id| {
                    let frame = self
                        .cache
                        .get(id)
                        .or_else(|| self.scratch.get(id))
                        .expect("scene refers to a frame the engine no longer holds");
                    &frame.frame.texture
                })
                .collect();
            if let Some(overlay) = overlay_texture.as_ref() {
                textures.push(overlay);
            }
            if count == 1 {
                return self
                    .compositor
                    .compose_to_iosurface_borrowed(self.ctx, scene, &textures, output);
            }
            self.compositor
                .accumulate_scene_to_texture_borrowed(self.ctx, scene, &textures, index, count)?;
        }
        self.compositor.accumulate_resolve_to_iosurface(
            self.ctx,
            output,
            output_width,
            output_height,
        )
    }

    /// Renders into a wgpu texture — the portable path, and the one a Rust
    /// front end wants: egui shares this device, so it samples the texture
    /// directly instead of round-tripping pixels through the host.
    pub fn render_to_texture(
        &mut self,
        time: f64,
        output: &promo_gpu::wgpu::Texture,
        output_width: u32,
        output_height: u32,
    ) -> Result<(), GpuError> {
        self.render_to_texture_with_overlay(time, output, output_width, output_height, None)
    }

    /// [`render_to_texture`](Self::render_to_texture) plus a host-provided
    /// overlay (an already-uploaded texture, premultiplied BGRA) composited
    /// last as a canvas-spanning quad — the portable twin of
    /// [`render_with_overlay`](Self::render_with_overlay), and the same
    /// final quad, so a watermarked preview matches a watermarked export.
    /// The caller uploads once ([`Compositor::upload_texture`]) and hands
    /// the texture back per frame: a watermark is static content, and an
    /// upload per frame at export sizes is real money.
    pub fn render_to_texture_with_overlay(
        &mut self,
        time: f64,
        output: &promo_gpu::wgpu::Texture,
        output_width: u32,
        output_height: u32,
        overlay: Option<&InputTexture>,
    ) -> Result<(), GpuError> {
        let mut scenes = self.build_scenes(time, output_width, output_height)?;
        if overlay.is_some() {
            let canvas = Size::new(
                self.meta.composition_settings.canvas_width,
                self.meta.composition_settings.canvas_height,
            );
            // The overlay rides EVERY sub-sample identically; the average of
            // N identical overlays over N varying scenes is the overlay over
            // the average — exact, so blur needs no special casing.
            for (scene, used) in &mut scenes {
                scene.quads.push(SceneQuad {
                    texture: Some(used.len()),
                    rect: [0.0, 0.0, canvas.width(), canvas.height()],
                    ..Default::default()
                });
            }
        }
        let count = scenes.len();
        for (index, (scene, used)) in scenes.iter().enumerate() {
            // Field-disjoint lookup, as in `render_with_overlay`.
            let mut textures: Vec<&InputTexture> = used
                .iter()
                .map(|id| {
                    let frame = self
                        .cache
                        .get(id)
                        .or_else(|| self.scratch.get(id))
                        .expect("scene refers to a frame the engine no longer holds");
                    &frame.frame.texture
                })
                .collect();
            if let Some(overlay) = overlay {
                textures.push(overlay);
            }
            if count == 1 {
                return self
                    .compositor
                    .compose_to_texture_borrowed(self.ctx, scene, &textures, output);
            }
            self.compositor
                .accumulate_scene_to_texture_borrowed(self.ctx, scene, &textures, index, count)?;
        }
        self.compositor
            .accumulate_resolve_to_texture(self.ctx, output, output_width, output_height)
    }

    /// Rasterizes a caption layer and returns its quad plus the cache id of
    /// the texture.
    ///
    /// Cached under the frame cache like any other texture, keyed by the
    /// layer id with a static source time: caption text does not change with
    /// the playhead, so a 30-second title is shaped once, not 900 times.
    /// One media quad, for ONE resource.
    ///
    /// Split out so a resource swap can call it twice — the outgoing
    /// material and the incoming one — which is what a transition between
    /// two clips needs and what a single-quad layer could never do.
    #[allow(clippy::too_many_arguments)]
    fn media_quad(
        &mut self,
        layer: &promo_model::ProjectLayer,
        showing: &str,
        settings: &promo_model::CompositionSettings,
        canvas: Size,
        time: f64,
        source_time: f64,
        tier: i32,
        is_drawing: bool,
        resources: &[promo_model::ProjectResource],
        used: &[u64],
        prepared: Option<u64>,
    ) -> Option<(SceneQuad, Option<SceneQuad>, u64, Option<u64>)> {
        let mut lut_frame: Option<u64> = None;
        let frame_id = match prepared {
            Some(id) => id,
            None => self.frame(&layer.id, showing, source_time, tier, used)?,
        };
        let frame = self.cached_frame(frame_id);
        let (mut fw, mut fh) = (frame.frame.width as f64, frame.frame.height as f64);
        let pre_framed = frame.flags & FLAG_PRE_FRAMED != 0;
        let color_709 = frame.flags & FLAG_COLOR_709 != 0;

        // A sprite sheet arrives as ONE texture and shows one cell of it.
        // The cell size replaces the sheet size here, before any layout
        // happens, so a walk cycle lays out as its 64×64 frame rather
        // than as the 256×128 image the frames are stored in.
        let resource = resources.iter().find(|r| r.id == showing);
        let mut uv_rect = [0.0f32, 0.0, 1.0, 1.0];
        if let Some(sheet) = resource.and_then(tl::sheet_for) {
            let local = tl::layer_local_time(layer, time);
            let Some(cell) = tl::sprite_frame_at(sheet, layer, local, Size::new(fw, fh)) else {
                // `hide`: the cycle is spent and the layer asked to go.
                return None;
            };
            fw = cell.cell.width();
            fh = cell.cell.height();
            uv_rect = [
                cell.uv_rect[0] as f32,
                cell.uv_rect[1] as f32,
                cell.uv_rect[2] as f32,
                cell.uv_rect[3] as f32,
            ];
        }

        // A keyframe viewport windows the source further — inside the
        // sprite cell when there is one, which is why it composes with
        // the uv rather than assigning it. The window's size then drives
        // layout exactly the way a cell's does: the layer lays out as
        // what it SHOWS, so drawn height stays canvasHeight × zoom and
        // width follows the window's aspect — the fixed frame the
        // feature promises.
        // The follow rule, when the layer has one and the recording a
        // track, is the viewport; a keyframed window otherwise.
        // The track's clock is the recording's own; a still has none, so
        // a followed image runs on the layer's clock.
        let follow_time = match resource {
            Some(r) if r.kind == promo_model::ProjectResourceKind::Video => source_time,
            _ => tl::layer_local_time(layer, time),
        };
        let following = resource.and_then(|r| tl::follow::follow_viewport(layer, r, follow_time));
        if let Some(vp) = following.or_else(|| tl::layer_viewport(layer, time)) {
            let uv = tl::compose_uv(
                [
                    uv_rect[0] as f64,
                    uv_rect[1] as f64,
                    uv_rect[2] as f64,
                    uv_rect[3] as f64,
                ],
                vp,
            );
            uv_rect = [uv[0] as f32, uv[1] as f32, uv[2] as f32, uv[3] as f32];
            fw *= vp[2];
            fh *= vp[3];
        }

        let tr = tl::layer_transform_along_paths(
            layer,
            time,
            settings,
            self.meta.resources.as_deref().unwrap_or(&[]),
        );
        let rect = if is_drawing {
            tl::drawing_rect(
                Size::new(fw, fh),
                canvas,
                tr.zoom,
                tr.horizontal_shift,
                tr.vertical_shift,
            )
        } else {
            tl::media_rect(
                Size::new(fw, fh),
                canvas,
                tr.zoom,
                tr.horizontal_shift,
                tr.vertical_shift,
            )
        };

        let mut quad = SceneQuad {
            texture: Some(0), // patched below
            rect: [rect.x(), rect.y(), rect.width(), rect.height()],
            rotation_deg: tl::layer_rotation(layer, time),
            opacity: tl::layer_opacity(layer, time) as f32,
            color_709,
            uv_rect,
            nearest: tl::is_nearest(resource),
            ..Default::default()
        };
        if !is_drawing && !pre_framed {
            let style = media_border_style(
                self.effective_frame(layer),
                layer,
                settings,
                tr.zoom,
                canvas.width(),
            );
            quad.corner_radius = style.corner_radius;
            quad.border_width = style.border_width;
            quad.border_rgba = style.border_rgba;
        }
        if let Some((adjust, tint)) = self.adjust_for(layer, time) {
            quad.adjust = adjust;
            quad.tint_rgba = tint;
        }
        if let Some((rid, amount)) = layer.adjustments.as_ref().and_then(|a| {
            a.lut_resource_id
                .as_ref()
                .map(|id| (id.clone(), a.lut_amount.unwrap_or(1.0)))
        }) {
            // The strip arrives through the provider like a still: keyed by a
            // synthetic layer so it never collides with the layer's own frame.
            let mut pinned = used.to_vec();
            pinned.push(frame_id);
            if let Some(id) = self.frame(&format!("lut\u{1f}{}", layer.id), &rid, -1.0, 0, &pinned)
            {
                let size = self.cached_frame(id).frame.height as f32;
                quad.lut_params = [1.0, amount.clamp(0.0, 1.0) as f32, size.max(2.0), 0.0];
                lut_frame = Some(id);
            }
        }
        if let Some(key) = layer.chroma_key.as_ref() {
            let rgba = rgba_from_hex(settings.resolve_color(&key.color_hex));
            quad.key_rgba = [rgba[0], rgba[1], rgba[2], 1.0];
            quad.key_params = [
                key.tolerance.unwrap_or(0.3).clamp(0.0, 1.0) as f32,
                key.softness.unwrap_or(0.1).clamp(0.0, 1.0) as f32,
                0.0,
                0.0,
            ];
        }
        Self::apply_effects(&mut quad, layer, time);
        quad.blend = Self::blend_for(layer);

        // Click rings for a followed recording: a circle at each live
        // click, growing and fading over half a second, in canvas px from
        // the click's place inside the window this quad shows.
        if let Some(vp) = following {
            let rgba = layer
                .follow
                .as_ref()
                .and_then(|f| f.click_color_hex.clone())
                .map(|hex| rgba_from_hex(settings.resolve_color(&hex)))
                .unwrap_or_else(|| {
                    let accent = settings.resolve_color("@accent");
                    if accent.eq_ignore_ascii_case("@accent") {
                        [1.0, 1.0, 1.0, 1.0]
                    } else {
                        rgba_from_hex(accent)
                    }
                });
            let clicks = resource
                .map(|r| tl::follow::live_clicks(layer, r, follow_time))
                .unwrap_or_default();
            for (ux, uy, age) in clicks {
                let life = (age / tl::follow::RING_SECONDS).clamp(0.0, 1.0);
                let cx = quad.rect[0] + (ux - vp[0]) / vp[2].max(1e-6) * quad.rect[2];
                let cy = quad.rect[1] + (uy - vp[1]) / vp[3].max(1e-6) * quad.rect[3];
                let size = (18.0 + 42.0 * life) * canvas.height() / 900.0;
                self.overlays.push(SceneQuad {
                    texture: None,
                    rect: [cx - size / 2.0, cy - size / 2.0, size, size],
                    corner_radius: size / 2.0,
                    border_width: (3.0 * canvas.height() / 900.0).max(1.0),
                    border_rgba: [rgba[0], rgba[1], rgba[2], (1.0 - life) as f32],
                    solid_rgba: [0.0, 0.0, 0.0, 0.0],
                    opacity: quad.opacity,
                    ..Default::default()
                });
            }
        }

        // The drop shadow, as its own soft-edged solid quad under this one.
        // Media only; `media_shadow_suppressed` holds the rest of the rule.
        let shadow = if is_drawing {
            None
        } else {
            let frame_ref = self.effective_frame(layer);
            if media_shadow_suppressed(frame_ref, pre_framed, layer.mask_resource_id.is_some()) {
                None
            } else {
                media_shadow_style(frame_ref, settings, tr.zoom, canvas.width()).map(|style| {
                    let radius =
                        media_border_style(frame_ref, layer, settings, tr.zoom, canvas.width())
                            .corner_radius
                            .min(quad.rect[2].min(quad.rect[3]) * 0.5)
                            .max(0.0);
                    // Rect and radius inflated by half the penumbra so the
                    // shader's falloff band straddles the TRUE edge — see
                    // `SceneQuad::edge_soften`.
                    let inflate = style.soften * 0.5;
                    SceneQuad {
                        texture: None,
                        rect: [
                            quad.rect[0] - inflate + style.offset[0],
                            quad.rect[1] - inflate + style.offset[1],
                            quad.rect[2] + inflate * 2.0,
                            quad.rect[3] + inflate * 2.0,
                        ],
                        rotation_deg: quad.rotation_deg,
                        corner_radius: radius + inflate,
                        solid_rgba: style.rgba,
                        opacity: quad.opacity,
                        edge_soften: style.soften,
                        ..Default::default()
                    }
                })
            }
        };
        Some((quad, shadow, frame_id, lut_frame))
    }

    /// Where each reveal unit sits for the caption this layer shows, laid
    /// out for the WHOLE string so revealing part of it cannot move it.
    fn caption_reveal(
        &mut self,
        layer: &ProjectLayer,
        showing: Option<&str>,
        settings: &promo_model::CompositionSettings,
        canvas: Size,
        time: f64,
        by: promo_text::RevealBy,
    ) -> Option<promo_text::RevealLayout> {
        let text_owned = self.meta.caption_text_showing(layer, showing)?;
        let text = text_owned.trim().to_string();
        if text.is_empty() {
            return None;
        }
        let style_source = self.meta.caption_style_showing(layer, showing);
        let mut style = caption_style(style_source.as_ref(), settings);
        if let Some(values) = tl::layer_caption_values(
            layer,
            time,
            tl::CaptionValues {
                font_size: style.font_size,
                vertical_margin: style.vertical_margin,
                left_margin: style.left_margin,
            },
        ) {
            style.font_size = values.font_size;
            style.vertical_margin = values.vertical_margin;
            style.left_margin = values.left_margin;
        }
        let key = format!(
            "{}|{:?}|{}|{}|{}",
            text,
            by,
            (style.font_size * 10.0).round(),
            (style.left_margin * 10.0).round(),
            (style.right_margin * 10.0).round()
        );
        if let Some(cached) = self.reveal_cache.get(&key) {
            return cached.clone();
        }
        let layout = promo_text::reveal_layout(&text, canvas.width(), canvas.height(), &style, by);
        self.reveal_cache.insert(key, layout.clone());
        layout
    }

    fn caption_quad(
        &mut self,
        layer: &ProjectLayer,
        showing: Option<&str>,
        settings: &promo_model::CompositionSettings,
        canvas: Size,
        time: f64,
        pinned: &[u64],
    ) -> Option<(SceneQuad, u64)> {
        self.caption_quad_colored(layer, showing, settings, canvas, time, None, pinned)
    }

    /// The caption raster, optionally in another colour.
    ///
    /// A karaoke highlight is the same words in a second colour, so it is a
    /// second raster from the same layout rather than a shader trick: two
    /// stable cache entries, and no per-frame text work for either.
    #[allow(clippy::too_many_arguments)]
    fn caption_quad_colored(
        &mut self,
        layer: &ProjectLayer,
        showing: Option<&str>,
        settings: &promo_model::CompositionSettings,
        canvas: Size,
        time: f64,
        color_override: Option<[u8; 4]>,
        pinned: &[u64],
    ) -> Option<(SceneQuad, u64)> {
        // Resource first, layer second — the app's rule (`captionText(for:)`).
        // Captions authored in the app keep their words and style on the
        // resource; reading only the layer left those invisible here, which is
        // why the host once composited its own copy on top — and animated
        // captions rendered twice.
        //
        // `showing` is WHICH caption resource, resolved for this instant: a
        // keyframe can swap a caption layer's words the way it swaps an
        // image, and reading `layer.resource_id` here meant every swap after
        // the first rendered the original text.
        let text_owned = self.meta.caption_text_showing(layer, showing)?;
        let text = text_owned.trim();
        if text.is_empty() {
            return None;
        }
        let style_source = self.meta.caption_style_showing(layer, showing);
        let mut style = caption_style(style_source.as_ref(), settings);
        if let Some(rgba) = color_override {
            style.text_rgba = rgba;
            // The plate belongs to the base raster; a tinted copy draws the
            // words alone or the highlight would paint a second plate over
            // the first.
            style.background_rgba = [0, 0, 0, 0];
        }
        // A caption's keyframes change its SIZE and MARGINS (the app's
        // mapping), so unlike a frame the raster does vary with the playhead —
        // and it is re-rasterized per size rather than scaled, which is what
        // keeps animated text crisp. The key carries the style, quantized to a
        // tenth of a point so a slow ramp reuses rasters instead of making one
        // per frame.
        if let Some(values) = tl::layer_caption_values(
            layer,
            time,
            tl::CaptionValues {
                font_size: style.font_size,
                vertical_margin: style.vertical_margin,
                left_margin: style.left_margin,
            },
        ) {
            style.font_size = values.font_size;
            style.vertical_margin = values.vertical_margin;
            style.left_margin = values.left_margin;
        }
        let stamp = |v: f64| (v * 10.0).round() as i64;
        // Raster density (1.0 in preview; the export's letterbox scale).
        // Part of the key: the same caption at a different density is a
        // different bitmap.
        let scale = self.raster_scale;
        // EVERYTHING that shapes the raster is in the key: the text (with
        // resource-held captions the same layer id can mean new words after
        // an edit) and the full resolved style — colors, family, weight,
        // alignment. A key of just the geometric fields kept serving a white
        // raster after the caption was recolored.
        let content_stamp = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            text.hash(&mut hasher);
            style.font_family.hash(&mut hasher);
            style.bold.hash(&mut hasher);
            style.italic.hash(&mut hasher);
            format!("{:?}", style.align).hash(&mut hasher);
            style.text_rgba.hash(&mut hasher);
            style.background_rgba.hash(&mut hasher);
            color_override.hash(&mut hasher);
            stamp(style.padding).hash(&mut hasher);
            stamp(style.corner_radius).hash(&mut hasher);
            stamp(style.right_margin).hash(&mut hasher);
            // The cached entry carries the ORIGIN, and a placement decides
            // it — an offset nudge must miss, or the box stays put.
            format!("{:?}", style.placement).hash(&mut hasher);
            hasher.finish()
        };
        let key = (
            format!(
                "caption:{}:{:x}:{}:{}:{}:{}",
                layer.id,
                content_stamp,
                stamp(style.font_size),
                stamp(style.vertical_margin),
                stamp(style.left_margin),
                stamp(scale)
            ),
            0i64,
            0i32,
        );
        if let Some(&id) = self.key_of.get(&key) {
            self.governor.touch(id);
            self.hits += 1;
            let frame = &self.cache[&id];
            // Dividing by the CURRENT scale is sound: it is part of the key,
            // so a hit means the raster was made at this same density.
            let (w, h) = (
                frame.frame.width as f64 / scale,
                frame.frame.height as f64 / scale,
            );
            let (x, y) = frame.caption_origin?;
            let mut quad = caption_scene_quad(x, y, w, h);
            if let Some((adjust, tint)) = self.adjust_for(layer, time) {
                quad.adjust = adjust;
                quad.tint_rgba = tint;
            }
            Self::apply_effects(&mut quad, layer, time);
            quad.blend = Self::blend_for(layer);
            return Some((quad, id));
        }

        // Rasterize at `scale`× density: everything the layout reads scales
        // together, so the quad below lands at the same canvas-space spot —
        // the texture is just denser.
        let dense = style.scaled_lengths(scale);
        let raster = promo_text::rasterize(
            text,
            canvas.width() * scale,
            canvas.height() * scale,
            &dense,
        )?;
        // promo-text produces straight RGBA; the compositor wants
        // premultiplied BGRA. Without the premultiply, every antialiased
        // glyph edge saturates and the text renders with binary edges.
        let mut bgra = raster.rgba;
        for px in bgra.chunks_exact_mut(4) {
            px.swap(0, 2);
            let a = px[3] as u32;
            for channel in px.iter_mut().take(3) {
                *channel = ((*channel as u32 * a + 127) / 255) as u8;
            }
        }
        let surface = GpuSurface::CpuPixels {
            data: bgra,
            width: raster.width,
            height: raster.height,
            bytes_per_row: raster.width * 4,
        };
        let frame = Compositor::import(self.ctx, &surface).ok()?;
        let bytes = frame.byte_size();

        self.misses += 1;
        let id = self.next_id;
        self.next_id += 1;
        for victim in self.governor.admit(id, bytes, pinned) {
            if let Some(k) = self.id_of.remove(&victim) {
                self.key_of.remove(&k);
            }
            self.cache.remove(&victim);
        }
        self.cache.insert(
            id,
            CachedFrame {
                frame,
                flags: 0,
                // Canvas-space origin (density divided back out), so a cache
                // hit rebuilds the quad without knowing how dense it is.
                caption_origin: Some((raster.x / scale, raster.y / scale)),
            },
        );
        self.key_of.insert(key.clone(), id);
        self.id_of.insert(id, key);
        let mut quad = caption_scene_quad(
            raster.x / scale,
            raster.y / scale,
            raster.width as f64 / scale,
            raster.height as f64 / scale,
        );
        if let Some((adjust, tint)) = self.adjust_for(layer, time) {
            quad.adjust = adjust;
            quad.tint_rgba = tint;
        }
        Self::apply_effects(&mut quad, layer, time);
        quad.blend = Self::blend_for(layer);
        Some((quad, id))
    }

    /// Rasterizes a drawing layer's vector document and returns its quad plus
    /// the cache id of the texture.
    ///
    /// Cached like a caption raster: keyed by layer id and the pixel size it
    /// was drawn at, so a zoom keyframe re-rasterizes (vector content stays
    /// crisp) while a hold costs nothing. The natural size comes from the
    /// document's own content bounds, which is what makes this possible
    /// without a host: nothing outside knows the drawing better than the
    /// shapes do.
    fn drawing_quad(
        &mut self,
        layer: &ProjectLayer,
        showing: Option<&str>,
        settings: &promo_model::CompositionSettings,
        canvas: Size,
        time: f64,
        pinned: &[u64],
    ) -> Option<(SceneQuad, u64)> {
        // Which document, for this instant: a drawing swaps like an image.
        let doc = self
            .meta
            .resources
            .as_ref()?
            .iter()
            .find(|r| Some(r.id.as_str()) == showing)?
            .drawing
            .as_ref()?;
        let shapes = vector_shapes(doc, settings);
        if shapes.is_empty() {
            return None;
        }
        let (_, _, bw, bh) = promo_gpu::vector::content_bounds(&shapes);
        // Path-aware: a keyframe carrying a motionPath bends the route to
        // it, and resolving that needs the drawing resource.
        let tr = tl::layer_transform_along_paths(
            layer,
            time,
            settings,
            self.meta.resources.as_deref().unwrap_or(&[]),
        );
        let rect = tl::drawing_rect(
            Size::new(bw.max(1.0), bh.max(1.0)),
            canvas,
            tr.zoom,
            tr.horizontal_shift,
            tr.vertical_shift,
        );
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return None;
        }
        // Density follows the raster scale (export renders at output size),
        // capped so a deep zoom cannot ask for an absurd texture — the same
        // ceiling the host rasterizers used.
        let cap = canvas.width().max(canvas.height()) * 2.0 * self.raster_scale;
        let pixel_width = (rect.width() * self.raster_scale).min(cap).max(1.0);
        let pixel_height = (rect.height() * self.raster_scale).min(cap).max(1.0);
        let (pw, ph) = (pixel_width.round() as u32, pixel_height.round() as u32);

        // Stamped for the same reason the mask key is: pw×ph comes from the
        // document's content bounds, which come from its points, so every
        // edit that moves no point — colour, stroke width, fill, arrowheads —
        // hit the pre-edit raster. `evict_layer` cannot cover for it either:
        // it matches `caption:{layer}:` and `{layer}\u{1f}` but never
        // `drawing:`, so a stale drawing layer survived its own eviction.
        let stamp = vector_content_stamp(&shapes);
        let key = (
            format!("drawing:{}:{:x}:{}x{}", layer.id, stamp, pw, ph),
            0i64,
            0i32,
        );
        if let Some(&id) = self.key_of.get(&key) {
            self.governor.touch(id);
            self.hits += 1;
            let mut quad = drawing_scene_quad(&rect);
            if let Some((adjust, tint)) = self.adjust_for(layer, time) {
                quad.adjust = adjust;
                quad.tint_rgba = tint;
            }
            Self::apply_effects(&mut quad, layer, time);
            quad.blend = Self::blend_for(layer);
            return Some((quad, id));
        }

        let renderer = match self.vector.as_mut() {
            Some(renderer) => renderer,
            None => {
                self.vector = Some(promo_gpu::vector::VectorRenderer::new(self.ctx).ok()?);
                self.vector.as_mut()?
            }
        };
        let frame = renderer.render_to_frame(self.ctx, &shapes, pw, ph).ok()?;
        let bytes = frame.byte_size();

        self.misses += 1;
        let id = self.next_id;
        self.next_id += 1;
        for victim in self.governor.admit(id, bytes, pinned) {
            if let Some(k) = self.id_of.remove(&victim) {
                self.key_of.remove(&k);
            }
            self.cache.remove(&victim);
        }
        self.cache.insert(
            id,
            CachedFrame {
                frame,
                flags: 0,
                caption_origin: None,
            },
        );
        self.key_of.insert(key.clone(), id);
        self.id_of.insert(id, key);
        let mut quad = drawing_scene_quad(&rect);
        if let Some((adjust, tint)) = self.adjust_for(layer, time) {
            quad.adjust = adjust;
            quad.tint_rgba = tint;
        }
        Self::apply_effects(&mut quad, layer, time);
        quad.blend = Self::blend_for(layer);
        Some((quad, id))
    }

    /// Rasterizes a mask drawing and returns the cache id of its texture.
    ///
    /// Uniform scale at COVERING density: the raster keeps the drawing's own
    /// aspect and the shader stretches it corner-to-corner over the layer's
    /// rect, so choosing the larger of the two covering scales keeps both
    /// axes sampling at or above 1:1. Cached by resource and pixel size —
    /// per RESOURCE, not per layer, so two layers sharing a mask at the same
    /// size share the raster.
    fn mask_texture(
        &mut self,
        resource_id: &str,
        rect: [f64; 4],
        settings: &promo_model::CompositionSettings,
        canvas: Size,
        pinned: &[u64],
    ) -> Option<(u64, f64, f64)> {
        if rect[2] <= 0.0 || rect[3] <= 0.0 {
            return None;
        }
        let doc = self
            .meta
            .resources
            .as_ref()?
            .iter()
            .find(|r| r.id == resource_id)?
            .drawing
            .as_ref()?;
        let shapes = vector_shapes(doc, settings);
        if shapes.is_empty() {
            return None;
        }
        let (_, _, bw, bh) = promo_gpu::vector::content_bounds(&shapes);
        let (bw, bh) = (bw.max(1.0), bh.max(1.0));
        // Same ceiling the drawing rasterizer uses, but applied to the SCALE
        // so the raster's aspect survives being capped.
        let cap = canvas.width().max(canvas.height()) * 2.0 * self.raster_scale;
        let scale = ((rect[2] * self.raster_scale) / bw)
            .max((rect[3] * self.raster_scale) / bh)
            .min(cap / bw)
            .min(cap / bh);
        let pw = (bw * scale).round().max(1.0) as u32;
        let ph = (bh * scale).round().max(1.0) as u32;

        // The stamp is what makes an EDIT visible. `mask:{resource}:{w}x{h}`
        // alone survived every change that left the drawing's points alone —
        // ink opacity, fill toggle, stroke width, shape kind, colour — since
        // the size below is derived from those points and nothing else, so
        // the canvas kept showing the pre-edit window. Nothing evicts it
        // either: this key is per RESOURCE and `evict_layer` is per layer.
        let stamp = vector_content_stamp(&shapes);
        let key = (
            format!("mask:{resource_id}:{stamp:x}:{pw}x{ph}"),
            0i64,
            0i32,
        );
        if let Some(&id) = self.key_of.get(&key) {
            self.governor.touch(id);
            self.hits += 1;
            return Some((id, bw, bh));
        }
        let renderer = match self.vector.as_mut() {
            Some(renderer) => renderer,
            None => {
                self.vector = Some(promo_gpu::vector::VectorRenderer::new(self.ctx).ok()?);
                self.vector.as_mut()?
            }
        };
        let frame = renderer.render_to_frame(self.ctx, &shapes, pw, ph).ok()?;
        let bytes = frame.byte_size();

        self.misses += 1;
        let id = self.next_id;
        self.next_id += 1;
        for victim in self.governor.admit(id, bytes, pinned) {
            if let Some(k) = self.id_of.remove(&victim) {
                self.key_of.remove(&k);
            }
            self.cache.remove(&victim);
        }
        self.cache.insert(
            id,
            CachedFrame {
                frame,
                flags: 0,
                caption_origin: None,
            },
        );
        self.key_of.insert(key.clone(), id);
        self.id_of.insert(id, key);
        Some((id, bw, bh))
    }

    /// Everything both render paths share: resolve the layers live at `time`,
    /// pull their frames through the provider, and describe the result. The
    /// only thing left to the caller is where it lands.
    fn build_scene(
        &mut self,
        time: f64,
        output_width: u32,
        output_height: u32,
    ) -> Result<(Scene, Vec<u64>), GpuError> {
        let mut doc = SceneDoc::project(&self.meta);
        if self.transparent_plate {
            doc.plate = Plate::Transparent;
        }
        self.build_scene_for(&doc, time, output_width, output_height)
    }

    /// The scene for one document level: the project itself, or a nested
    /// composition — the same builder, so every kind of layer, every
    /// effect and every transition works inside a composition exactly as
    /// it does at the top. Only the top level clears the previous render's
    /// transient frames; a nested build is part of its parent's.
    fn build_scene_for(
        &mut self,
        doc: &SceneDoc,
        time: f64,
        output_width: u32,
        output_height: u32,
    ) -> Result<(Scene, Vec<u64>), GpuError> {
        self.overlays.clear();
        // The PREVIOUS render's transient frames die here, not at its end: a
        // deferred-fence compose may still have the GPU sampling them after
        // render returns, and wgpu keeps submitted resources alive only once
        // they are submitted — which the previous render has done by now.
        // A motion-blur walk is the exception: its sub-builds are one
        // render, and the frames its earlier composes reference must live
        // until the walk resolves.
        if doc.depth == 0 {
            self.builds += 1;
            if !self.retain_scratch {
                self.scratch.clear();
                self.scratch_key.clear();
            }
        }
        let settings = self.meta.composition_settings.clone();
        let canvas = doc.canvas;

        let mut layers: Vec<ProjectLayer> = doc.layers.clone();
        layers.sort_by_key(|l| l.sort_index);

        // Background color: the first visible background layer's keyframed
        // color, else the settings color (same rule as the stills path).
        let bg_layer = layers
            .iter()
            .find(|l| l.kind == ProjectLayerKind::Background && tl::layer_is_visible(l, time));
        // The look resolves keyframes-first, then the layer's BACKGROUND
        // RESOURCE (rung 16 — swap-aware, so a keyframe can replace the
        // whole plate), then the composition settings. The resource slots
        // in by overriding a local copy of the settings: the existing
        // keyframe resolution then falls back to it naturally.
        let bg_all = self.meta.resources.clone().unwrap_or_default();
        let bg_resource = bg_layer
            .and_then(|l| tl::layer_resource_id(l, time, &bg_all))
            .and_then(|rid| bg_all.iter().find(|r| r.id == rid))
            .filter(|r| r.kind == promo_model::ProjectResourceKind::Background)
            .cloned();
        let mut bg_settings = settings.clone();
        if let Some(style) = bg_resource.as_ref().and_then(|r| r.background.as_ref()) {
            if let Some(hex) = &style.color_hex {
                bg_settings.background_color_hex = hex.clone();
            }
            if style.gradient.is_some() {
                bg_settings.background_gradient = style.gradient.clone();
            }
        }
        // A composition's plate is its own colour, or nothing at all: a title
        // comp over footage shows the footage through. A nested background
        // LAYER still paints as in any document.
        let background = match (&doc.plate, bg_layer) {
            (Plate::Composition(None), None) | (Plate::Transparent, None) => [0.0, 0.0, 0.0, 0.0],
            (Plate::Composition(Some(hex)), None) => rgba_from_hex(settings.resolve_color(hex)),
            _ => {
                let bg_hex = bg_layer
                    .map(|l| tl::layer_background_color_hex(l, time, &bg_settings))
                    .unwrap_or_else(|| bg_settings.background_color_hex.clone());
                rgba_from_hex(settings.resolve_color(&bg_hex))
            }
        };

        let background_gradient = bg_layer
            .and_then(|layer| tl::layer_background_gradient(layer, time, &bg_settings))
            .or_else(|| match doc.plate {
                Plate::Project => bg_settings.background_gradient.clone(),
                Plate::Composition(_) | Plate::Transparent => None,
            })
            // Absent geometry resolved at READ — the timeline already
            // resolved keyframed gradients against the plate; this covers
            // the plate/settings fallbacks themselves.
            .map(|gradient| gradient.resolved_geometry(None))
            .map(|gradient| {
                let (cw, ch) = (canvas.width() as f32, canvas.height() as f32);
                let point = |p: promo_model::Point| [p.x() as f32 * cw, p.y() as f32 * ch];
                let fallback = promo_model::BackgroundGradient::default_geometry(gradient.kind);
                promo_gpu::compositor::SceneGradient {
                    radial: gradient.kind == promo_model::GradientKind::Radial,
                    repeat: match gradient.effective_repeat() {
                        promo_model::GradientRepeat::Clamp => 0,
                        promo_model::GradientRepeat::Repeat => 1,
                        promo_model::GradientRepeat::Mirror => 2,
                    },
                    start: point(gradient.start.unwrap_or(fallback.0)),
                    end: point(gradient.end.unwrap_or(fallback.1)),
                    stops: gradient
                        .resolved_stops()
                        .iter()
                        .map(|stop| {
                            (
                                rgba_from_hex(settings.resolve_color(&stop.color_hex)),
                                stop.at as f32,
                            )
                        })
                        .collect(),
                }
            });

        let mut quads: Vec<SceneQuad> = Vec::new();
        let mut used: Vec<u64> = Vec::new();
        // Drop shadows, recorded as (owner index, quad) and spliced in only
        // after the texture patch and the mask loop below — both address
        // `quads` by position, and a shadow inserted early would shift
        // every index after it.
        let mut shadow_inserts: Vec<(usize, SceneQuad)> = Vec::new();
        // Click rings ride ABOVE their layer; inserted after the positional
        // patch, like shadows, so `used` stays one frame per quad until then.
        let mut overlay_inserts: Vec<(usize, SceneQuad)> = Vec::new();
        // Masked media quads, patched after the walk: (quad index, mask
        // resource, inverted, placement). Rasterized LAST, so a mask's cache
        // admission can never evict a frame the walk has already borrowed.
        // Placement resolves on the GEOMETRY clock (`time`, the blur
        // sub-sample) — a flying window smears like any other motion.
        let mut mask_requests: Vec<(usize, String, bool, Option<tl::MaskPlacement>)> = Vec::new();
        // LUT strips join the texture list AFTER the content, like masks:
        // `used` stays one frame per quad until the positional patch below.
        let mut lut_requests: Vec<(usize, u64)> = Vec::new();

        for layer in &layers {
            if !tl::layer_is_visible(layer, time) {
                continue;
            }
            // Motion blur: a blurred layer's GEOMETRY resolves at the walk's
            // sub-sample; everything deciding IDENTITY — which resource
            // shows, which source frame decodes, whether the layer is here
            // at all — stays on the frame's own clock. That split is the
            // whole feature: the editor's motion smears, a cut inside the
            // shutter stays a cut, and no frame decodes twice.
            let centre = time;
            let time = match self.blur_sample {
                Some(sample) if tl::layer_shutter(layer, centre).is_some_and(|s| s > 0.0) => sample,
                _ => centre,
            };
            if layer.kind == ProjectLayerKind::Caption {
                // Text is drawn by the core now (promo-text), not by a host
                // overlay — so a headless render keeps its captions.
                let all = self.meta.resources.clone().unwrap_or_default();
                let showing = tl::layer_resource_id(layer, centre, &all).map(str::to_string);
                let swap = tl::transition::active_swap_sampled(layer, centre, time, &all);
                // Words being replaced are still on screen while the new ones
                // arrive — the same two-quad rule the media path uses.
                if let Some(swap) = swap.as_ref() {
                    if let Some((mut quad, id)) = self.caption_quad(
                        layer,
                        swap.previous.as_deref(),
                        &settings,
                        canvas,
                        time,
                        &used,
                    ) {
                        quad.opacity = tl::layer_opacity(layer, time) as f32;
                        if let Some((tilt_x, tilt_y)) = tl::layer_tilt_offset(layer, time) {
                            quad.tilt = [tilt_x, tilt_y];
                        }
                        // A push shoves the outgoing material out the far
                        // side; every other kind leaves it where it is and
                        // arrives over it.
                        apply_effect(&mut quad, swap.departing, canvas, time);
                        apply_transition(&mut quad, layer, time, canvas);
                        quads.push(quad);
                        used.push(id);
                    }
                }
                if let Some((mut quad, id)) =
                    self.caption_quad(layer, showing.as_deref(), &settings, canvas, time, &used)
                {
                    quad.opacity = tl::layer_opacity(layer, time) as f32;
                    // Tilt keyframes lean a caption in perspective — the
                    // device slab's own pinhole camera, on the quad. Set
                    // before the extrusion, so the side leans with the face.
                    if let Some((tilt_x, tilt_y)) = tl::layer_tilt_offset(layer, time) {
                        quad.tilt = [tilt_x, tilt_y];
                        quad.tilt_distance = quad.rect[2].max(quad.rect[3]) * 3.2;
                        quad.tilt_pivot = Some([
                            quad.rect[0] + quad.rect[2] / 2.0,
                            quad.rect[1] + quad.rect[3] / 2.0,
                        ]);
                    }
                    if let Some(swap) = swap.as_ref() {
                        apply_effect(&mut quad, swap.effect, canvas, time);
                    }
                    apply_transition(&mut quad, layer, time, canvas);

                    // A reveal draws the SAME raster as a set of bands —
                    // cropped to what has arrived — rather than a new picture
                    // per frame. Laid out whole, so revealing part of it
                    // cannot move the caption, and a word keeps the place the
                    // layout gave it however it arrives.
                    let depth_rule = self
                        .meta
                        .caption_style_showing(layer, showing.as_deref())
                        .and_then(|style| style.depth)
                        .unwrap_or(promo_model::TextDepth {
                            count: Some(0),
                            ..Default::default()
                        });
                    let rule = self
                        .meta
                        .caption_style_showing(layer, showing.as_deref())
                        .and_then(|style| style.reveal)
                        .or_else(|| settings.subtitle_reveal.clone());
                    let bands = rule.as_ref().and_then(|rule| {
                        let by = tl::reveal::unit_of(rule);
                        let layout = self.caption_reveal(
                            layer,
                            showing.as_deref(),
                            &settings,
                            canvas,
                            time,
                            by,
                        )?;
                        let progress = tl::reveal::progress(rule, layer, time, layout.units.len());
                        Some((tl::reveal::bands(&layout, progress, rule), rule.clone()))
                    });

                    match bands {
                        Some((bands, rule)) if !bands.is_empty() => {
                            let tinted = rule.highlight_color_hex.as_deref().and_then(|hex| {
                                let rgba = rgba_bytes(settings.resolve_color(hex), 1.0);
                                self.caption_quad_colored(
                                    layer,
                                    showing.as_deref(),
                                    &settings,
                                    canvas,
                                    time,
                                    Some(rgba),
                                    &used,
                                )
                            });
                            for band in bands {
                                let source = if band.active { tinted } else { None };
                                let (mut piece, piece_id) = match source {
                                    Some((tinted_quad, tinted_id)) => {
                                        let mut copy = tinted_quad;
                                        copy.opacity = quad.opacity;
                                        (copy, tinted_id)
                                    }
                                    None => (quad, id),
                                };
                                let full_height = piece.rect[3];
                                crop_to_band(&mut piece, band.uv);
                                apply_effect(
                                    &mut piece,
                                    band.effect_for(full_height),
                                    canvas,
                                    time,
                                );
                                for copy in extrusion(&piece, &depth_rule, &settings, canvas) {
                                    quads.push(copy);
                                    used.push(piece_id);
                                }
                                quads.push(piece);
                                used.push(piece_id);
                            }
                        }
                        // A reveal is ACTIVE but nothing has arrived yet —
                        // the staggered modes' honest first frames. Drawing
                        // nothing is what the rule says; falling through to
                        // the whole caption flashed the full text at rest
                        // for a frame before every rise, which read as a
                        // glitch on every caption in a template.
                        Some((_, _)) => {}
                        // No reveal on this caption (or it could not be
                        // measured): the whole quad, as always.
                        None => {
                            for copy in extrusion(&quad, &depth_rule, &settings, canvas) {
                                quads.push(copy);
                                used.push(id);
                            }
                            quads.push(quad);
                            used.push(id);
                        }
                    }
                }
                continue;
            }
            if layer.kind == ProjectLayerKind::Background {
                // An image-backed background plate, drawn BARE: no border,
                // corner, shadow or adjustments — scenery, not media. Its
                // colour/gradient already came in through the scene
                // background above; swap keyframes replace the plate via
                // `layer_resource_id`, and for a TILED plate the layer's
                // shift keyframes scroll the pattern from its anchor.
                let all = self.meta.resources.clone().unwrap_or_default();
                if let Some(rid) = tl::layer_resource_id(layer, centre, &all).map(str::to_string) {
                    if let Some(resource) = all.iter().find(|r| r.id == rid) {
                        if resource.kind == promo_model::ProjectResourceKind::Background
                            && !resource.filename.is_empty()
                        {
                            let style = resource.background.clone().unwrap_or_default();
                            let tier = self.tier_for(layer, centre);
                            if let Some(frame_id) = self.frame(&layer.id, &rid, -1.0, tier, &used) {
                                let frame = self.cached_frame(frame_id);
                                let (fw, fh) =
                                    (frame.frame.width as f64, frame.frame.height as f64);
                                let color_709 = frame.flags & FLAG_COLOR_709 != 0;
                                let (cw, ch) = (canvas.width(), canvas.height());
                                let mut quad = SceneQuad {
                                    texture: Some(0), // patched below
                                    rect: [0.0, 0.0, cw, ch],
                                    opacity: tl::layer_opacity(layer, time) as f32,
                                    color_709,
                                    ..Default::default()
                                };
                                match style.fill {
                                    promo_model::BackgroundFill::Stretch => {}
                                    promo_model::BackgroundFill::Fit => {
                                        let scale = (cw / fw.max(1.0)).min(ch / fh.max(1.0));
                                        let (w, h) = (fw * scale, fh * scale);
                                        quad.rect = [(cw - w) / 2.0, (ch - h) / 2.0, w, h];
                                    }
                                    promo_model::BackgroundFill::Tile => {
                                        let tr = tl::layer_transform_along_paths(
                                            layer, time, &settings, &all,
                                        );
                                        let anchor = style.anchor.unwrap_or([0.0, 0.0]);
                                        // Scale multiplies the tile's size,
                                        // so it DIVIDES how many repeats
                                        // span the canvas. The layer's ZOOM
                                        // keyframes — which mean nothing
                                        // else on a background — multiply
                                        // it, making tile scale animatable
                                        // on the ordinary eased track.
                                        let tile_scale = (style.scale.unwrap_or(1.0)
                                            * tr.zoom.max(0.01))
                                        .max(0.005);
                                        // The image's NATIVE size decides
                                        // the tile, not the provided
                                        // frame's: previews hand in TIERED
                                        // (downsampled) bitmaps, and
                                        // repeats computed from those made
                                        // a 3000px photo tile as if it
                                        // were canvas-sized.
                                        let iw =
                                            resource.pixel_width.filter(|w| *w > 1.0).unwrap_or(fw);
                                        let ih = resource
                                            .pixel_height
                                            .filter(|h| *h > 1.0)
                                            .unwrap_or(fh);
                                        quad.tile_repeats = [
                                            (cw / (iw * tile_scale).max(1.0)) as f32,
                                            (ch / (ih * tile_scale).max(1.0)) as f32,
                                        ];
                                        quad.tile_anchor = [
                                            (anchor[0] + tr.horizontal_shift / cw) as f32,
                                            (anchor[1] + tr.vertical_shift / ch) as f32,
                                        ];
                                    }
                                }
                                quads.push(quad);
                                used.push(frame_id);
                            }
                        }
                    }
                }
                continue;
            }
            let (_is_media, is_drawing) = match layer.kind {
                ProjectLayerKind::Video | ProjectLayerKind::Image | ProjectLayerKind::Model => {
                    (true, false)
                }
                ProjectLayerKind::Drawing => (false, true),
                _ => continue,
            };

            let source_time = if layer.kind == ProjectLayerKind::Video {
                let local = tl::layer_local_time(layer, centre);
                match self.resource_for(layer) {
                    Some(res) => {
                        // A layer naming a cut plays that sub-range; the
                        // mapping is the same code either way.
                        let view = tl::resource_for_cut(res, layer.media_cut_id.as_deref());
                        match tl::source_time_for_layer(&view, local, layer.beyond_end) {
                            Some(time) => time,
                            // `Hide`: the layer has outlived its material and
                            // draws nothing rather than freezing on a still.
                            None => continue,
                        }
                    }
                    None => local,
                }
            } else if self.image_has_animated_tilt(layer) {
                // Animated-tilt device frame: the baked bitmap varies with
                // time, so request per-time (the provider re-bakes with the
                // interpolated tilt; the cache keys per quantized time).
                // The frame's own time even under blur: the bake is source
                // pixels, and source pixels decode once.
                centre
            } else {
                -1.0
            };

            // Drawings are VECTOR content the engine can draw itself, so it
            // does — no provider round trip. Every front end used to have to
            // remember to tessellate and hand a bitmap over; the one that
            // forgot (the CLI) rendered whole compositions with the strokes
            // silently missing, while `inspect` still called the layer
            // renderable. One producer, like captions.
            if is_drawing {
                let all = self.meta.resources.clone().unwrap_or_default();
                let showing = tl::layer_resource_id(layer, centre, &all).map(str::to_string);
                let swap = tl::transition::active_swap_sampled(layer, centre, time, &all);
                if let Some(swap) = swap.as_ref() {
                    if let Some((mut quad, id)) = self.drawing_quad(
                        layer,
                        swap.previous.as_deref(),
                        &settings,
                        canvas,
                        time,
                        &used,
                    ) {
                        quad.rotation_deg = tl::layer_rotation(layer, time);
                        quad.opacity = tl::layer_opacity(layer, time) as f32;
                        apply_effect(&mut quad, swap.departing, canvas, time);
                        apply_transition(&mut quad, layer, time, canvas);
                        quads.push(quad);
                        used.push(id);
                    }
                }
                if let Some((quad, id)) =
                    self.drawing_quad(layer, showing.as_deref(), &settings, canvas, time, &used)
                {
                    let mut quad = quad;
                    quad.rotation_deg = tl::layer_rotation(layer, time);
                    quad.opacity = tl::layer_opacity(layer, time) as f32;
                    if let Some(swap) = swap.as_ref() {
                        apply_effect(&mut quad, swap.effect, canvas, time);
                    }
                    apply_transition(&mut quad, layer, time, canvas);
                    quads.push(quad);
                    used.push(id);
                }
                continue;
            }

            let tier = self.tier_for(layer, centre);
            // What this layer shows right now — its own resource, unless a
            // keyframe has swapped it.
            let resources = self.meta.resources.clone().unwrap_or_default();
            let showing = tl::layer_resource_id(layer, centre, &resources)
                .unwrap_or_default()
                .to_string();
            let swap = tl::transition::active_swap_sampled(layer, centre, time, &resources);
            // A composition draws itself: rendered by recursion into a
            // texture that then stands where a host frame would.
            let composed = match resources.iter().find(|r| {
                r.id == showing
                    && matches!(
                        r.kind,
                        promo_model::ProjectResourceKind::Composition
                            | promo_model::ProjectResourceKind::Model
                    )
            }) {
                Some(resource) if resource.kind == promo_model::ProjectResourceKind::Model => {
                    // A model draws itself through the model pass; the
                    // picture then rides the media path like a still.
                    match self.model_frame(layer, resource, &settings, centre, tier, &used) {
                        Some(id) => Some(id),
                        None => continue,
                    }
                }
                Some(resource) => {
                    match self.composition_frame(
                        layer,
                        resource,
                        source_time,
                        tier,
                        doc.depth,
                        &used,
                    ) {
                        Some(id) => Some(id),
                        None => continue,
                    }
                }
                None => None,
            };
            if let Some(swap) = swap.as_ref() {
                // The outgoing material, whole, underneath — a dissolve or a
                // wipe needs both on screen at once, which is exactly what a
                // swap could not do while a layer drew one quad.
                if let Some(previous) = swap.previous.as_deref() {
                    // The outgoing material's shadow is discarded: one
                    // shadow per layer, and the incoming quad carries it.
                    if let Some((mut quad, _shadow, id, lut_id)) = self.media_quad(
                        layer,
                        previous,
                        &settings,
                        canvas,
                        time,
                        source_time,
                        tier,
                        is_drawing,
                        &resources,
                        &used,
                        None,
                    ) {
                        apply_effect(&mut quad, swap.departing, canvas, time);
                        apply_transition(&mut quad, layer, time, canvas);
                        quads.push(quad);
                        used.push(id);
                        if let Some(lut) = lut_id {
                            lut_requests.push((quads.len() - 1, lut));
                        }
                        // The window frames the OUTGOING material too: a
                        // swap happens inside the porthole, not around it.
                        if let Some(rid) = layer.mask_resource_id.as_deref() {
                            mask_requests.push((
                                quads.len() - 1,
                                rid.to_string(),
                                layer.mask_inverted.unwrap_or(false),
                                tl::layer_mask_placement(layer, time),
                            ));
                        }
                    }
                }
            }
            let Some((mut quad, shadow, frame_id, lut_id)) = self.media_quad(
                layer,
                &showing,
                &settings,
                canvas,
                time,
                source_time,
                tier,
                is_drawing,
                &resources,
                &used,
                composed,
            ) else {
                continue;
            };
            used.push(frame_id);
            if let Some(swap) = swap.as_ref() {
                // The incoming material arrives over it.
                apply_effect(&mut quad, swap.effect, canvas, time);
            }
            // After the border, so a wiped edge cuts the frame too rather
            // than leaving a stroke drawn around nothing.
            let pre_transition = quad.rect;
            apply_transition(&mut quad, layer, time, canvas);
            quads.push(quad);
            for ring in self.overlays.drain(..) {
                overlay_inserts.push((quads.len() - 1, ring));
            }
            if let Some(lut) = lut_id {
                lut_requests.push((quads.len() - 1, lut));
            }
            if let Some(mut sq) = shadow {
                // The transition moved or faded the layer — the shadow
                // travels glued to it. Delta'd rather than recomputed: a
                // wipe clips uv, not rect, and needs nothing here.
                let q = &quads[quads.len() - 1];
                sq.rect[0] += q.rect[0] - pre_transition[0];
                sq.rect[1] += q.rect[1] - pre_transition[1];
                sq.opacity = q.opacity;
                shadow_inserts.push((quads.len() - 1, sq));
            }
            if let Some(rid) = layer.mask_resource_id.as_deref() {
                mask_requests.push((
                    quads.len() - 1,
                    rid.to_string(),
                    layer.mask_inverted.unwrap_or(false),
                    tl::layer_mask_placement(layer, time),
                ));
            }
        }

        // Patch texture indices now that the used-frame list is final; the
        // caller borrows the textures in this same order.
        for (i, quad) in quads.iter_mut().enumerate() {
            quad.texture = Some(i);
        }
        for (quad_index, id) in lut_requests {
            let slot = match used.iter().position(|&u| u == id) {
                Some(slot) => slot,
                None => {
                    used.push(id);
                    used.len() - 1
                }
            };
            quads[quad_index].lut = Some(slot);
        }

        // Masks join the texture list AFTER the content: `used` is complete,
        // so pinning it keeps every borrowed frame safe from the masks'
        // admissions, and nothing allocates after this, so the masks stay
        // put too. A mask shared by several quads lands in one slot.
        for (quad_index, resource_id, inverted, placement) in mask_requests {
            let mut rect = quads[quad_index].rect;
            // A zoomed-up window samples a larger share of the rect from the
            // same raster: raise the raster's density with it so the edge
            // stays crisp. Never below the base density — a shrunken window
            // costs nothing extra.
            let density = placement.map_or(1.0, |p| p.zoom).max(1.0);
            rect[2] *= density;
            rect[3] *= density;
            let Some((id, bw, bh)) =
                self.mask_texture(&resource_id, rect, &settings, canvas, &used)
            else {
                continue;
            };
            // The mask's OWN proportions, aspect-fitted into the layer's rect
            // and centred — so a circle drawn round renders round on a 2:3
            // layer instead of being stretched into an oval by it. Only a
            // deliberate zoom_y makes it an oval now.
            let quad_rect = quads[quad_index].rect;
            let fit = (quad_rect[2] / bw.max(1e-6)).min(quad_rect[3] / bh.max(1e-6));
            let half = [(bw * fit / 2.0) as f32, (bh * fit / 2.0) as f32];
            let slot = match used.iter().position(|&u| u == id) {
                Some(slot) => slot,
                None => {
                    used.push(id);
                    used.len() - 1
                }
            };
            quads[quad_index].mask = Some(slot);
            quads[quad_index].mask_invert = inverted;
            quads[quad_index].mask_half = half;
            if let Some(p) = placement {
                quads[quad_index].mask_zoom_y = p.zoom_y as f32;
                quads[quad_index].mask_transform = [
                    p.dx as f32,
                    p.dy as f32,
                    p.zoom as f32,
                    p.rotation_deg as f32,
                ];
            }
        }

        // Shadows slide in UNDER their layers only now: texture references
        // are VALUES into `used` by this point (not positions), and the two
        // loops above addressed quads by index. Reverse order keeps each
        // recorded index valid while earlier ones are still to be inserted.
        // Rings first, each just above its quad; then shadows, each just
        // below — both in reverse, both against the indices recorded above.
        for (index, ring) in overlay_inserts.into_iter().rev() {
            quads.insert(index + 1, ring);
        }
        for (index, sq) in shadow_inserts.into_iter().rev() {
            quads.insert(index, sq);
        }

        Ok((
            Scene {
                canvas_width: canvas.width(),
                canvas_height: canvas.height(),
                background_rgba: background,
                background_gradient,
                output_width,
                output_height,
                bars_rgba: background,
                quads,
            },
            used,
        ))
    }
}

/// One document level for the scene builder: the project, or a nested
/// composition. Layers are cloned in (the builder sorts and walks them);
/// resources are always the project's, whichever level is being built.
struct SceneDoc {
    layers: Vec<ProjectLayer>,
    canvas: Size,
    plate: Plate,
    depth: u32,
}

/// What lies under a level's layers: the project's settings and background
/// machinery, or a composition's own colour (none = transparent).
enum Plate {
    Project,
    Composition(Option<String>),
    /// The project rendered over nothing — an alpha export: no settings
    /// colour, no gradient; a background LAYER still paints.
    Transparent,
}

impl SceneDoc {
    fn project(meta: &ProjectMetadata) -> Self {
        SceneDoc {
            layers: meta.layers.clone().unwrap_or_default(),
            canvas: Size::new(
                meta.composition_settings.canvas_width,
                meta.composition_settings.canvas_height,
            ),
            plate: Plate::Project,
            depth: 0,
        }
    }

    fn composition(composition: &promo_model::Composition, depth: u32) -> Self {
        SceneDoc {
            layers: composition.layers.clone(),
            canvas: Size::new(composition.canvas_width, composition.canvas_height),
            plate: Plate::Composition(composition.background_color_hex.clone()),
            depth,
        }
    }
}

impl PreviewEngine {
    /// A composition's picture at `source_time` on its own clock — the
    /// nested layers built with the same scene builder and composed into
    /// a texture of the composition's canvas (halved past tier 0), which
    /// then enters the cache as a frame would: `media_quad` reads its
    /// size, placement rules see the resource's pixel size, transitions
    /// and blur apply to it as to any clip. Keyed by layer, resource,
    /// quantized time and tier, transient in export mode like any frame.
    /// Nothing past the depth cap — a file that slipped past validation
    /// cannot recurse the GPU.
    /// Hand the engine a model's bytes (a `.glb`) for a resource. The host
    /// reads the file — I/O is the host's, as with every pixel source —
    /// and calls this once per resource; calling again replaces it. Decode
    /// failures come back as text, and the layer then draws nothing.
    pub fn provide_model(&mut self, resource_id: &str, bytes: &[u8]) -> Result<(), String> {
        let model = crate::model::Model::from_glb(bytes).map_err(|e| e.to_string())?;
        if self.model_pass.is_none() {
            self.model_pass = Some(ModelPass::new(self.ctx).map_err(|e| format!("{e:?}"))?);
        }
        let pass = self.model_pass.as_ref().expect("built above");
        let meshes: Vec<MeshInput<'_>> = model
            .meshes
            .iter()
            .map(|m| MeshInput {
                positions: &m.positions,
                normals: &m.normals,
                uvs: &m.uvs,
                indices: &m.indices,
                material: m.material,
            })
            .collect();
        let materials: Vec<MaterialInput<'_>> = model
            .materials
            .iter()
            .map(|m| MaterialInput {
                base_color: m.base_color,
                metallic: m.metallic,
                roughness: m.roughness,
                double_sided: m.double_sided,
                texture: m
                    .base_texture
                    .as_ref()
                    .map(|t| (t.width, t.height, t.rgba.as_slice())),
            })
            .collect();
        let gpu = pass
            .upload(self.ctx, &meshes, &materials)
            .map_err(|e| format!("{e:?}"))?;
        // Any frame drawn from the previous bytes is stale.
        let stale: Vec<u64> = self
            .id_of
            .iter()
            .filter(|(_, k)| {
                k.0.ends_with(&format!("\u{1f}{resource_id}")) && k.0.starts_with("model")
            })
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            if let Some(k) = self.id_of.remove(&id) {
                self.key_of.remove(&k);
            }
            self.cache.remove(&id);
        }
        self.models.insert(
            resource_id.to_string(),
            LoadedModel {
                model,
                gpu,
                painted: HashMap::new(),
                bound: HashMap::new(),
            },
        );
        Ok(())
    }

    /// Whether a model has been provided for a resource.
    pub fn has_model(&self, resource_id: &str) -> bool {
        self.models.contains_key(resource_id)
    }

    /// The model layer's picture at `time`: the model pass into a square
    /// texture the height of the canvas (so `zoom` 1 fills the frame's
    /// height with the model's bounding sphere), cached like a composition
    /// frame — per time when a camera, light or clip is keyed, once
    /// otherwise.
    fn model_frame(
        &mut self,
        layer: &ProjectLayer,
        resource: &promo_model::ProjectResource,
        settings: &promo_model::CompositionSettings,
        time: f64,
        tier: i32,
        pinned: &[u64],
    ) -> Option<u64> {
        if !self.models.contains_key(&resource.id) {
            return None;
        }
        let animated = layer
            .keyframes
            .iter()
            .any(|k| k.camera.is_some() || k.light.is_some() || k.clip.is_some());
        let key_time = if animated { time } else { -1.0 };
        let transient = self.export_mode && animated;
        let key = (
            format!("model\u{1f}{}\u{1f}{}", layer.id, resource.id),
            quantize(key_time),
            tier,
        );
        if transient {
            if let Some(&id) = self.scratch_key.get(&key) {
                self.hits += 1;
                return Some(id);
            }
        } else if let Some(&id) = self.key_of.get(&key) {
            self.governor.touch(id);
            self.hits += 1;
            return Some(id);
        }

        let canvas_h = settings.canvas_height.max(1.0);
        let scale = if tier > 0 { 0.5 } else { 1.0 };
        let side = ((canvas_h * scale).round() as u32).clamp(8, 2048);

        // The camera and the light, each field on the keyframe clock with
        // the model's own defaults where nothing is keyed.
        let local = tl::layer_local_time(layer, time);
        let scalar = |select: fn(&promo_model::ProjectLayerKeyframe) -> Option<f64>| {
            tl::interpolation::layer_interpolated_scalar(layer, local, select)
        };
        let camera = promo_model::Camera {
            yaw: scalar(|k| k.camera.and_then(|c| c.yaw)),
            pitch: scalar(|k| k.camera.and_then(|c| c.pitch)),
            roll: scalar(|k| k.camera.and_then(|c| c.roll)),
            distance: scalar(|k| k.camera.and_then(|c| c.distance)),
            fov: scalar(|k| k.camera.and_then(|c| c.fov)),
        };
        let light = promo_model::Light {
            yaw: scalar(|k| k.light.and_then(|l| l.yaw)),
            pitch: scalar(|k| k.light.and_then(|l| l.pitch)),
            intensity: scalar(|k| k.light.and_then(|l| l.intensity)),
        };
        // Lighting from the theme: ambient is the canvas colour, dimmed;
        // the key light white; the rim the accent, if the palette has one.
        let linear = |rgba: [f32; 4]| -> [f32; 3] {
            let f = |c: f32| {
                if c <= 0.04045 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            };
            [f(rgba[0]), f(rgba[1]), f(rgba[2])]
        };
        let background = linear(rgba_from_hex(
            settings.resolve_color(&settings.background_color_hex),
        ));
        let ambient = [
            background[0] * 0.45 + 0.08,
            background[1] * 0.45 + 0.08,
            background[2] * 0.45 + 0.08,
        ];
        let accent = settings.resolve_color("@accent");
        let rim = if accent == "@accent" {
            [0.25, 0.3, 0.4]
        } else {
            let c = linear(rgba_from_hex(accent));
            [c[0] * 0.8 + 0.1, c[1] * 0.8 + 0.1, c[2] * 0.8 + 0.1]
        };

        // Bindings paint their slots: a colour through the palette, a
        // resource as the slot's picture — the host serves it under a
        // synthetic layer, keyed by the resource, exactly as a LUT strip is.
        let (colours, pictures): (Vec<(usize, [f32; 4])>, SlotPictures) = {
            let loaded = self.models.get(&resource.id)?;
            let mut colours = Vec::new();
            let mut pictures = Vec::new();
            for (slot, binding) in resource.materials.iter().flat_map(|m| m.iter()) {
                let Some(index) = loaded.model.materials.iter().position(|m| &m.name == slot)
                else {
                    continue;
                };
                match binding {
                    promo_model::MaterialBinding::Color(hex) => {
                        let srgb = rgba_from_hex(settings.resolve_color(hex));
                        let lin = linear(srgb);
                        colours.push((index, [lin[0], lin[1], lin[2], srgb[3]]));
                    }
                    promo_model::MaterialBinding::Resource { resource_id } => {
                        pictures.push((index, resource_id.clone()));
                    }
                }
            }
            (colours, pictures)
        };
        let mut bound_frames: Vec<(usize, u64)> = Vec::new();
        for (index, bound_id) in pictures {
            // A still: source time -1, as an image layer asks.
            if let Some(id) = self.frame(
                &format!("slot\u{1f}{bound_id}"),
                &bound_id,
                -1.0,
                tier,
                pinned,
            ) {
                bound_frames.push((index, id));
            }
        }
        let pass = self.model_pass.as_ref()?;
        let loaded = self.models.get_mut(&resource.id)?;
        for (index, rgba) in colours {
            if loaded.painted.get(&index) != Some(&rgba) {
                pass.recolor(self.ctx, &mut loaded.gpu, index, rgba);
                loaded.painted.insert(index, rgba);
            }
        }
        for (index, frame_id) in bound_frames {
            if loaded.bound.get(&index) == Some(&frame_id) {
                continue;
            }
            let texture = match self
                .cache
                .get(&frame_id)
                .or_else(|| self.scratch.get(&frame_id))
            {
                Some(entry) => &entry.frame.texture,
                None => continue,
            };
            pass.set_texture(self.ctx, &mut loaded.gpu, index, texture.view());
            loaded.bound.insert(index, frame_id);
        }
        let view = ModelView {
            yaw: camera.yaw(),
            pitch: camera.pitch(),
            roll: camera.roll(),
            distance: camera.distance(),
            fov: camera.fov(),
            bounds_center: loaded.model.bounds_center,
            bounds_radius: loaded.model.bounds_radius,
            light_yaw: light.yaw(),
            light_pitch: light.pitch(),
            light_intensity: light.intensity(),
            key_rgb: [1.0, 1.0, 1.0],
            ambient_rgb: ambient,
            rim_rgb: rim,
        };
        let texture = pass
            .render_to_texture(self.ctx, &loaded.gpu, &view, side, side)
            .ok()?;
        let frame = promo_gpu::ImportedFrame::from_owned_texture(texture, side, side);
        let bytes = frame.byte_size();

        self.misses += 1;
        let id = self.next_id;
        self.next_id += 1;
        let entry = CachedFrame {
            frame,
            flags: 0,
            caption_origin: None,
        };
        if transient {
            self.scratch.insert(id, entry);
            self.scratch_key.insert(key, id);
            return Some(id);
        }
        for victim in self.governor.admit(id, bytes, pinned) {
            if let Some(k) = self.id_of.remove(&victim) {
                self.key_of.remove(&k);
            }
            self.cache.remove(&victim);
        }
        self.cache.insert(id, entry);
        self.key_of.insert(key.clone(), id);
        self.id_of.insert(id, key);
        Some(id)
    }

    fn composition_frame(
        &mut self,
        layer: &ProjectLayer,
        resource: &promo_model::ProjectResource,
        source_time: f64,
        tier: i32,
        depth: u32,
        pinned: &[u64],
    ) -> Option<u64> {
        let composition = resource.composition.as_ref()?;
        if depth as usize >= promo_model::nesting::MAX_DEPTH {
            return None;
        }
        let transient = self.export_mode;
        let key = (
            format!("comp\u{1f}{}\u{1f}{}", layer.id, resource.id),
            quantize(source_time),
            tier,
        );
        if transient {
            if let Some(&id) = self.scratch_key.get(&key) {
                self.hits += 1;
                return Some(id);
            }
        } else if let Some(&id) = self.key_of.get(&key) {
            self.governor.touch(id);
            self.hits += 1;
            return Some(id);
        }

        let scale = if tier > 0 { 0.5 } else { 1.0 };
        let width = ((composition.canvas_width * scale).round() as u32).max(1);
        let height = ((composition.canvas_height * scale).round() as u32).max(1);
        let doc = SceneDoc::composition(composition, depth + 1);
        let (scene, nested_used) = self
            .build_scene_for(&doc, source_time, width, height)
            .ok()?;

        let texture = self
            .ctx
            .device
            .create_texture(&promo_gpu::wgpu::TextureDescriptor {
                label: Some("composition"),
                size: promo_gpu::wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: promo_gpu::wgpu::TextureDimension::D2,
                format: promo_gpu::wgpu::TextureFormat::Bgra8Unorm,
                usage: promo_gpu::wgpu::TextureUsages::RENDER_ATTACHMENT
                    | promo_gpu::wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
        {
            // Field-disjoint lookup, as in `render_with_overlay`.
            let textures: Vec<&InputTexture> = nested_used
                .iter()
                .map(|id| {
                    let frame = self
                        .cache
                        .get(id)
                        .or_else(|| self.scratch.get(id))
                        .expect("a nested scene refers to a frame the engine no longer holds");
                    &frame.frame.texture
                })
                .collect();
            self.compositor
                .compose_to_texture_borrowed(self.ctx, &scene, &textures, &texture)
                .ok()?;
        }
        let frame = promo_gpu::ImportedFrame::from_owned_texture(texture, width, height);
        let bytes = frame.byte_size();

        self.misses += 1;
        let id = self.next_id;
        self.next_id += 1;
        let entry = CachedFrame {
            frame,
            flags: 0,
            caption_origin: None,
        };
        if transient {
            self.scratch.insert(id, entry);
            self.scratch_key.insert(key, id);
            return Some(id);
        }
        for victim in self.governor.admit(id, bytes, pinned) {
            if let Some(k) = self.id_of.remove(&victim) {
                self.key_of.remove(&k);
            }
            self.cache.remove(&victim);
        }
        self.cache.insert(id, entry);
        self.key_of.insert(key.clone(), id);
        self.id_of.insert(id, key);
        Some(id)
    }
}

/// Corner radius, border width, and border color for a media quad.
#[derive(Debug, Clone, Copy, PartialEq)]
struct MediaBorderStyle {
    corner_radius: f64,
    border_width: f64,
    border_rgba: [f32; 4],
}

/// The export path's rule, now the engine's too: a BORDER-kind
/// `ResourceFrame` supplies the radius, thickness, and color — authored
/// against a 1080-wide reference, floored, then scaled by zoom (the exact
/// order `ResourceFrame.borderPixels`/`cornerRadiusPixels` use). Without a
/// border frame, the settings/layer fallbacks apply as before. This used to
/// exist only in `VideoComposer`, so a border-framed video rendered
/// differently in preview and export.
///
/// A DEVICE frame reaches here only on a layer nothing pre-baked a slab for —
/// video, which no bake site covers. The slab can't be drawn, so the frame
/// degrades to its border rather than to the canvas-wide default: the layer
/// keeps the radius and edge the project authored instead of losing its frame
/// outright. Callers styling a pre-framed quad don't call this at all.
fn media_border_style(
    frame: Option<&promo_model::ResourceFrame>,
    layer: &ProjectLayer,
    settings: &promo_model::CompositionSettings,
    zoom: f64,
    canvas_width: f64,
) -> MediaBorderStyle {
    let zoom = tl::clamped_zoom(zoom);
    if let Some(frame) = frame {
        if frame.kind != promo_model::ResourceFrameKind::None {
            return MediaBorderStyle {
                corner_radius: (frame.corner_radius * canvas_width / 1080.0).max(0.0) * zoom,
                border_width: (frame.border_width * canvas_width / 1080.0).max(1.0) * zoom,
                border_rgba: rgba_from_hex(settings.resolve_color(&frame.border_color_hex)),
            };
        }
    }
    MediaBorderStyle {
        corner_radius: settings.video_corner_radius * zoom,
        border_width: layer
            .image_border_width
            .unwrap_or(settings.video_border_width),
        border_rgba: rgba_from_hex(
            settings.resolve_color(
                layer
                    .image_border_color_hex
                    .as_deref()
                    .unwrap_or(&settings.video_border_color_hex),
            ),
        ),
    }
}

/// Whether a media layer casts NO shadow at all, whatever
/// [`media_shadow_style`] would resolve. Mirrored by Swift's
/// `LayerLayout.mediaShadowSuppressed`, which feeds the SwiftUI fallback
/// canvas — the two disagreed for a release, and an unbaked device frame
/// cast in the export while the preview showed none.
///
/// A MASKED layer's silhouette is the mask's, not the rect, so a rect
/// shadow would trace the wrong outline. So would a device SLAB's — but
/// only where a slab was actually built, which is what `pre_framed` says:
/// the frame is already baked into the bitmap this quad samples. Nothing
/// bakes a slab for video, and an unbaked device frame degrades to its
/// border (see [`media_border_style`]), so its silhouette IS the rect and
/// it casts like any other bordered layer. Pre-framed BORDER bakes cast
/// too: the baked bitmap's radius comes from the same numbers.
fn media_shadow_suppressed(
    frame: Option<&promo_model::ResourceFrame>,
    pre_framed: bool,
    masked: bool,
) -> bool {
    if masked {
        return true;
    }
    pre_framed
        && frame
            .map(|f| f.kind == promo_model::ResourceFrameKind::Device)
            .unwrap_or(false)
}

/// The drop shadow under a media quad: colour (straight alpha), penumbra
/// length, and offset, all in canvas px.
#[derive(Debug, Clone, Copy, PartialEq)]
struct MediaShadowStyle {
    rgba: [f32; 4],
    soften: f64,
    offset: [f64; 2],
}

/// Resolves the shadow the way captions resolve theirs: each frame field
/// falls back PER-FIELD to the composition's `video_shadow_*` default.
/// Frame-authored lengths are against the 1080-wide reference like
/// `border_width`; settings-authored lengths are canvas px. Both scale with
/// zoom — a card zoomed larger casts a larger shadow, exactly as its
/// corners round wider. An absent offset derives the caption drop: straight
/// down by half the blur. `None` when the resolved shadow would not draw.
fn media_shadow_style(
    frame: Option<&promo_model::ResourceFrame>,
    settings: &promo_model::CompositionSettings,
    zoom: f64,
    canvas_width: f64,
) -> Option<MediaShadowStyle> {
    let zoom = tl::clamped_zoom(zoom);
    let frame_scale = canvas_width / 1080.0;
    let opacity = frame
        .and_then(|f| f.shadow_opacity)
        .unwrap_or(settings.video_shadow_opacity);
    if opacity <= 0.0 {
        return None;
    }
    let soften = match frame.and_then(|f| f.shadow_radius) {
        Some(r) => (r * frame_scale).max(0.0) * zoom,
        None => settings.video_shadow_radius.max(0.0) * zoom,
    };
    let offset = match frame.and_then(|f| f.shadow_offset) {
        Some([x, y]) => [x * frame_scale * zoom, y * frame_scale * zoom],
        None => match settings.video_shadow_offset {
            Some([x, y]) => [x * zoom, y * zoom],
            None => [0.0, soften / 2.0],
        },
    };
    if soften <= 0.0 && offset[0].abs() < 1e-9 && offset[1].abs() < 1e-9 {
        return None;
    }
    let hex = frame
        .and_then(|f| f.shadow_color_hex.as_deref())
        .unwrap_or(&settings.video_shadow_color_hex);
    let mut rgba = rgba_from_hex(settings.resolve_color(hex));
    rgba[3] = (opacity as f32).clamp(0.0, 1.0);
    Some(MediaShadowStyle {
        rgba,
        soften,
        offset,
    })
}

#[cfg(test)]
mod border_style_tests {
    use super::*;

    fn layer() -> ProjectLayer {
        serde_json::from_value(serde_json::json!({
            "id": "L", "name": "L", "sortIndex": 1, "kind": "video",
            "isEnabled": true, "startTime": 0.0, "keyframes": []
        }))
        .unwrap()
    }

    fn border_frame() -> promo_model::ResourceFrame {
        serde_json::from_value(serde_json::json!({
            "kind": "border", "borderColorHex": "FF0000",
            "borderWidth": 6.0, "cornerRadius": 24.0,
            "material": "spaceBlack", "tiltY": 0.0, "tiltX": 0.0,
            "bezelFraction": 0.03, "depthFraction": 0.06
        }))
        .unwrap()
    }

    #[test]
    fn a_border_frame_supplies_radius_thickness_and_color() {
        let settings = promo_model::CompositionSettings::default();
        let style = media_border_style(Some(&border_frame()), &layer(), &settings, 1.0, 1920.0);
        // Authored at 1080-wide: 24 * 1920/1080, 6 * 1920/1080 — the exact
        // Swift math, floor applied before zoom.
        assert!((style.corner_radius - 24.0 * 1920.0 / 1080.0).abs() < 1e-9);
        assert!((style.border_width - 6.0 * 1920.0 / 1080.0).abs() < 1e-9);
        assert!(style.border_rgba[0] > 0.99 && style.border_rgba[1] < 0.01);
    }

    #[test]
    fn no_frame_keeps_the_settings_fallback() {
        let settings = promo_model::CompositionSettings::default();
        let with_none = media_border_style(None, &layer(), &settings, 1.0, 1920.0);
        assert!((with_none.corner_radius - settings.video_corner_radius).abs() < 1e-9);
        assert!((with_none.border_width - settings.video_border_width).abs() < 1e-9);

        // An explicit None-kind frame is the same as no frame: it must not
        // borrow the frame's authored radius/thickness.
        let mut off = border_frame();
        off.kind = promo_model::ResourceFrameKind::None;
        assert_eq!(
            media_border_style(Some(&off), &layer(), &settings, 1.0, 1920.0),
            with_none
        );
    }

    #[test]
    fn an_unbaked_device_frame_degrades_to_its_border() {
        // A Device frame only reaches `media_border_style` when nothing baked
        // a slab for the layer — video, which no bake site covers. It used to
        // fall through to the canvas-wide default, so a 3D Box on a video
        // rendered as a plain rect with none of the frame the project
        // authored. It now keeps the frame's radius, thickness, and color.
        let settings = promo_model::CompositionSettings::default();
        let mut device = border_frame();
        device.kind = promo_model::ResourceFrameKind::Device;
        let with_device = media_border_style(Some(&device), &layer(), &settings, 1.0, 1920.0);
        assert_eq!(
            with_device,
            media_border_style(Some(&border_frame()), &layer(), &settings, 1.0, 1920.0),
            "an unbaked device frame styles exactly as the border it degrades to"
        );
        assert!((with_device.corner_radius - 24.0 * 1920.0 / 1080.0).abs() < 1e-9);
        assert!(
            (with_device.corner_radius - settings.video_corner_radius).abs() > 1e-6,
            "and NOT the canvas-wide default it used to fall back to"
        );
    }

    #[test]
    fn thin_frame_borders_floor_at_one_pixel_before_zoom() {
        let settings = promo_model::CompositionSettings::default();
        let mut frame = border_frame();
        frame.border_width = 0.1; // 0.1 * 540/1080 = 0.05 → floors to 1
        let style = media_border_style(Some(&frame), &layer(), &settings, 2.0, 540.0);
        assert!(
            (style.border_width - 2.0).abs() < 1e-9,
            "1px floor × zoom 2"
        );
    }

    fn device_frame() -> promo_model::ResourceFrame {
        let mut frame = border_frame();
        frame.kind = promo_model::ResourceFrameKind::Device;
        frame
    }

    /// The divergence this pins: a device frame on a layer nothing baked a
    /// slab for draws as a plain bordered rect, so it CASTS. The SwiftUI
    /// fallback skipped every device frame and the preview lost a shadow
    /// the export drew.
    #[test]
    fn only_a_baked_slab_escapes_the_shadow() {
        let device = device_frame();
        assert!(
            !media_shadow_suppressed(Some(&device), false, false),
            "an unbaked device frame degrades to its border and casts"
        );
        assert!(
            media_shadow_suppressed(Some(&device), true, false),
            "a baked slab's silhouette is not the rect"
        );
        assert!(
            !media_shadow_suppressed(Some(&border_frame()), true, false),
            "a baked BORDER is still a rounded rect"
        );
        assert!(!media_shadow_suppressed(None, true, false));
    }

    #[test]
    fn a_masked_layer_never_casts() {
        assert!(media_shadow_suppressed(None, false, true));
        assert!(media_shadow_suppressed(Some(&device_frame()), false, true));
        assert!(media_shadow_suppressed(Some(&border_frame()), false, true));
    }

    #[test]
    fn shadow_is_off_by_default_and_settings_switch_it_on() {
        let mut settings = promo_model::CompositionSettings::default();
        assert!(media_shadow_style(None, &settings, 1.0, 1920.0).is_none());
        settings.video_shadow_opacity = 0.5;
        settings.video_shadow_radius = 20.0;
        let style = media_shadow_style(None, &settings, 2.0, 1920.0).unwrap();
        // Canvas px × zoom, and the derived drop: down by half the blur.
        assert!((style.soften - 40.0).abs() < 1e-9);
        assert!((style.offset[0]).abs() < 1e-9);
        assert!((style.offset[1] - 20.0).abs() < 1e-9);
        assert!((style.rgba[3] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn frame_shadow_fields_override_per_field_with_1080_reference() {
        // Inherited: the frame sets no opacity of its own.
        let settings = promo_model::CompositionSettings {
            video_shadow_opacity: 0.5,
            ..Default::default()
        };
        let mut frame = border_frame();
        frame.shadow_radius = Some(10.0); // authored at 1080-wide
        let style = media_shadow_style(Some(&frame), &settings, 1.0, 1920.0).unwrap();
        assert!((style.soften - 10.0 * 1920.0 / 1080.0).abs() < 1e-9);
        // Opacity came from the composition — the caption fallback rule.
        assert!((style.rgba[3] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn zero_opacity_or_zero_geometry_draws_nothing() {
        let mut settings = promo_model::CompositionSettings {
            video_shadow_radius: 20.0, // opacity still 0
            ..Default::default()
        };
        assert!(media_shadow_style(None, &settings, 1.0, 1920.0).is_none());
        settings.video_shadow_radius = 0.0;
        settings.video_shadow_opacity = 1.0; // no blur, no offset
        assert!(media_shadow_style(None, &settings, 1.0, 1920.0).is_none());
        // A hard offset shadow with no blur still draws.
        settings.video_shadow_offset = Some([6.0, 6.0]);
        assert!(media_shadow_style(None, &settings, 1.0, 1920.0).is_some());
    }
}

/// A fingerprint of everything the vector rasterizer reads — the piece a
/// raster cache key needs so an EDIT is a different key.
///
/// Both vector caches key by size, and the size comes from the document's
/// content bounds, which come from its POINTS alone. So recolouring a mask,
/// thickening its stroke, dropping its fill, flipping its even-odd rule, or
/// swapping pen for oval all landed on the identical key and the canvas kept
/// compositing the raster from before the edit. macOS hid it — the resource
/// editor tears the engine down — while on iOS the editor is a sheet over a
/// live engine, which is the feature's whole edit-and-check loop. Same bug
/// the caption's `content_stamp` exists for, one cache along.
///
/// Hashed on the RESOLVED shapes rather than the document, so `@name`
/// colours are already looked up: a palette edit re-rasterizes too, and the
/// hash covers exactly the bytes the tessellator will read.
///
/// Colours are in the stamp even though a mask samples only ALPHA (the
/// compositor's `textureSample(quad_mask, …).a`, and the vector pipeline
/// blends alpha independently of RGB, so hue provably cannot move a mask).
/// Leaving hue out would save one rasterization per recolour and buy a key
/// that is silently wrong the day anything reads a mask's colour — the same
/// trade that produced this bug. The cache is not worth being clever with.
fn vector_content_stamp(shapes: &[promo_gpu::vector::VectorShape]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    shapes.len().hash(&mut hasher);
    for shape in shapes {
        // Order matters as much as content: the tessellator draws
        // fill-then-stroke per shape in array order.
        (shape.kind as u8).hash(&mut hasher);
        shape.points.len().hash(&mut hasher);
        for &(x, y) in &shape.points {
            x.to_bits().hash(&mut hasher);
            y.to_bits().hash(&mut hasher);
        }
        shape.stroke_width.to_bits().hash(&mut hasher);
        for c in &shape.stroke_rgba {
            c.to_bits().hash(&mut hasher);
        }
        // NO fill and a fully transparent fill are different rasters, so the
        // discriminant is hashed, not just the components — an unparseable
        // `fillColorHex` collapses to `None` in `vector_shapes`.
        match shape.fill_rgba {
            Some(rgba) => {
                1u8.hash(&mut hasher);
                for c in &rgba {
                    c.to_bits().hash(&mut hasher);
                }
            }
            None => 0u8.hash(&mut hasher),
        }
        shape.arrow_start.hash(&mut hasher);
        shape.arrow_end.hash(&mut hasher);
        shape.even_odd_fill.hash(&mut hasher);
    }
    hasher.finish()
}

/// A drawing quad sits where `drawing_rect` put it, in canvas space.
fn drawing_scene_quad(rect: &promo_model::Rect) -> SceneQuad {
    SceneQuad {
        texture: Some(0), // patched with the rest
        rect: [rect.x(), rect.y(), rect.width(), rect.height()],
        ..Default::default()
    }
}

/// A caption quad sits where promo-text put it, in canvas space.
/// Crops a quad to a band of its own texture, rect and uv together.
///
/// Same rule the transitions use: the picture must stay where it is while a
/// piece of it is shown, so the drawn rect shrinks by exactly the fraction
/// the texture window does.
/// The furthest any quad travels between two scenes, in OUTPUT pixels —
/// what decides how many samples a shutter needs. Corners, not origins
/// (rotation and scale move corners while the centre sits still), plus the
/// content shift a `uv_rect` pan produces inside a static rect. None when
/// the scenes disagree about how many quads exist.
fn scene_displacement(a: &Scene, b: &Scene) -> Option<f64> {
    if a.quads.len() != b.quads.len() {
        return None;
    }
    let scale = if a.canvas_width > 0.0 && a.canvas_height > 0.0 {
        (a.output_width as f64 / a.canvas_width).min(a.output_height as f64 / a.canvas_height)
    } else {
        1.0
    };
    let mut max_d: f64 = 0.0;
    for (qa, qb) in a.quads.iter().zip(&b.quads) {
        let corners = |q: &SceneQuad| -> [[f64; 2]; 4] {
            let [x, y, w, h] = q.rect;
            let (cx, cy) = (x + w / 2.0, y + h / 2.0);
            let (sin, cos) = q.rotation_deg.to_radians().sin_cos();
            let rot = |dx: f64, dy: f64| [cx + dx * cos - dy * sin, cy + dx * sin + dy * cos];
            [
                rot(-w / 2.0, -h / 2.0),
                rot(w / 2.0, -h / 2.0),
                rot(-w / 2.0, h / 2.0),
                rot(w / 2.0, h / 2.0),
            ]
        };
        for (ca, cb) in corners(qa).iter().zip(&corners(qb)) {
            let d = ((ca[0] - cb[0]).powi(2) + (ca[1] - cb[1]).powi(2)).sqrt();
            max_d = max_d.max(d * scale);
        }
        // A pan inside the window: the rect holds still while the texture
        // slides under it. Screen speed = uv shift over window width times
        // the rect's on-screen size.
        let [ua, va, uwa, vha] = qa.uv_rect;
        let [ub, vb, uwb, vhb] = qb.uv_rect;
        if uwa > 0.0 && vha > 0.0 {
            let du = ((ua - ub).abs() + (uwa - uwb).abs()) as f64 / uwa as f64;
            let dv = ((va - vb).abs() + (vha - vhb).abs()) as f64 / vha as f64;
            max_d = max_d.max(du * qa.rect[2] * scale);
            max_d = max_d.max(dv * qa.rect[3] * scale);
        }
    }
    Some(max_d)
}

fn crop_to_band(quad: &mut SceneQuad, uv: [f64; 4]) {
    let [x, y, w, h] = quad.rect;
    quad.rect = [x + w * uv[0], y + h * uv[1], w * uv[2], h * uv[3]];
    let base = quad.uv_rect;
    quad.uv_rect = [
        base[0] + base[2] * uv[0] as f32,
        base[1] + base[3] * uv[1] as f32,
        base[2] * uv[2] as f32,
        base[3] * uv[3] as f32,
    ];
}

/// Applies the layer's entry/exit transition to a finished quad.
///
/// Every drawable kind goes through here, so a caption wipes exactly as a
/// screenshot does. Opacity is already multiplied in by `layer_opacity` —
/// what is left is the geometry: where the quad sits, how much of it shows,
/// and how big it is.
fn apply_transition(
    quad: &mut SceneQuad,
    layer: &promo_model::ProjectLayer,
    time: f64,
    canvas: Size,
) {
    // The opacity is already in the quad — `layer_opacity` multiplies the
    // layer's own transition in — so only the rest of the effect applies
    // here; multiplying it again made every fade-in quadratic.
    apply_effect_with(
        quad,
        tl::transition::effect(layer, time),
        canvas,
        time,
        false,
    );
}

/// Extruded type by stacking: the copies drawn UNDER a caption quad —
/// farthest first — each offset further along and gelled toward the side
/// colour. The face itself is not among them. Pushed before the face so
/// they sit beneath it; each carries the face's frame id, so the
/// positional patch pairs every copy with the same raster.
fn extrusion(
    face: &SceneQuad,
    depth: &promo_model::TextDepth,
    settings: &promo_model::CompositionSettings,
    canvas: Size,
) -> Vec<SceneQuad> {
    let count = depth.count();
    if count == 0 {
        return Vec::new();
    }
    let scale = canvas.height() / 900.0;
    let [dx, dy] = depth.offset();
    let side = depth
        .color_hex
        .as_deref()
        .map(|hex| rgba_from_hex(settings.resolve_color(hex)))
        .unwrap_or([0.0, 0.0, 0.0, 1.0]);
    (1..=count)
        .rev()
        .map(|i| {
            let mut copy = *face;
            copy.rect[0] += dx * scale * i as f64;
            copy.rect[1] += dy * scale * i as f64;
            // The side: the face's own pixels, gelled toward the side
            // colour by the shade — the glyph alpha is untouched, so the
            // side has the letters' exact silhouette.
            copy.tint_rgba = [side[0], side[1], side[2], 1.0];
            copy.adjust[3] = depth.shade() as f32;
            copy
        })
        .collect()
}

/// The whole effect, for one that came from somewhere other than the
/// layer's own edges — a resource swap, whose opacity nothing else has
/// applied. `time` seeds the glitch, so its tear is new every frame.
fn apply_effect(quad: &mut SceneQuad, effect: tl::transition::Effect, canvas: Size, time: f64) {
    apply_effect_with(quad, effect, canvas, time, true);
}

fn apply_effect_with(
    quad: &mut SceneQuad,
    effect: tl::transition::Effect,
    canvas: Size,
    time: f64,
    with_opacity: bool,
) {
    if effect.is_identity() {
        return;
    }
    if with_opacity {
        quad.opacity *= effect.opacity as f32;
    }
    // The blurring kinds: the wider of the transition's softness and the
    // layer's own blur, scaled to this canvas.
    if effect.blur > 0.0 {
        let px = (effect.blur * canvas.height() / 900.0) as f32;
        if px > quad.blur {
            quad.blur = px;
            quad.blur_angle = None;
        }
    }
    if effect.flash > 0.0 {
        // White mixed in through the grade's brightness, which clamps at 1.
        quad.adjust[2] = (quad.adjust[2] + effect.flash as f32).min(1.0);
    }
    if effect.glitch > 0.0 {
        quad.glitch = [
            effect.glitch as f32,
            ((time * 60.0).round() as i64 % 10_000) as f32,
        ];
    }
    let (rect, uv) = tl::transition::apply(
        &effect,
        quad.rect,
        quad.uv_rect,
        (canvas.width(), canvas.height()),
    );
    quad.rect = rect;
    quad.uv_rect = uv;
    // A unit turning in does so about ITS OWN centre, whatever pivot the
    // whole caption's lean set; the camera distance stays the caption's.
    if effect.tilt != [0.0, 0.0] {
        quad.tilt = [quad.tilt[0] + effect.tilt[0], quad.tilt[1] + effect.tilt[1]];
        quad.tilt_pivot = Some([
            quad.rect[0] + quad.rect[2] / 2.0,
            quad.rect[1] + quad.rect[3] / 2.0,
        ]);
    }
    if effect.rotate != 0.0 {
        quad.rotation_deg += effect.rotate;
    }
}

fn caption_scene_quad(x: f64, y: f64, w: f64, h: f64) -> SceneQuad {
    SceneQuad {
        texture: Some(0), // patched with the rest
        rect: [x, y, w, h],
        ..Default::default()
    }
}

/// Bridges the project's subtitle style to promo-text, falling back to the
/// composition defaults exactly as `SubtitleStyle.xxx(defaults:)` does.
pub fn caption_style(
    style: Option<&promo_model::SubtitleStyle>,
    settings: &promo_model::CompositionSettings,
) -> promo_text::TextStyle {
    let get = |pick: fn(&promo_model::SubtitleStyle) -> Option<f64>, fallback: f64| -> f64 {
        style.and_then(pick).unwrap_or(fallback)
    };
    let text_rgba = rgba_bytes(
        settings.resolve_color(
            &style
                .and_then(|s| s.text_color_hex.clone())
                .unwrap_or_else(|| settings.subtitle_color_hex.clone()),
        ),
        1.0,
    );
    let bg_opacity = style
        .and_then(|s| s.background_opacity)
        .unwrap_or(settings.subtitle_background_opacity);
    let background_rgba = rgba_bytes(
        settings.resolve_color(
            &style
                .and_then(|s| s.background_color_hex.clone())
                .unwrap_or_else(|| settings.subtitle_background_color_hex.clone()),
        ),
        bg_opacity,
    );
    promo_text::TextStyle {
        font_family: style
            .and_then(|s| s.font_family.as_ref())
            .map(|f| f.as_str().to_string())
            .or_else(|| Some(settings.subtitle_font_family.as_str().to_string())),
        font_size: get(|s| s.font_size, settings.subtitle_font_size),
        bold: style
            .and_then(|s| s.is_bold)
            .unwrap_or(settings.subtitle_bold),
        italic: style
            .and_then(|s| s.is_italic)
            .unwrap_or(settings.subtitle_italic),
        // Per caption, then the composition's default. Nothing falls back to
        // a constant here any more: the constant was "center" while the app
        // assumed "leading", so an unaligned caption edited one way rendered
        // another.
        align: promo_text::Align::parse(
            style
                .and_then(|s| s.alignment.as_ref())
                .unwrap_or(&settings.subtitle_alignment)
                .as_str(),
        ),
        text_rgba,
        background_rgba,
        stroke_rgba: rgba_bytes(
            settings.resolve_color(
                &style
                    .and_then(|s| s.stroke_color_hex.clone())
                    .unwrap_or_else(|| settings.subtitle_stroke_color_hex.clone()),
            ),
            1.0,
        ),
        stroke_width: get(|s| s.stroke_width, settings.subtitle_stroke_width),
        shadow_rgba: rgba_bytes(
            settings.resolve_color(
                &style
                    .and_then(|s| s.shadow_color_hex.clone())
                    .unwrap_or_else(|| settings.subtitle_shadow_color_hex.clone()),
            ),
            style
                .and_then(|s| s.shadow_opacity)
                .unwrap_or(settings.subtitle_shadow_opacity),
        ),
        shadow_radius: get(|s| s.shadow_radius, settings.subtitle_shadow_radius),
        // Default the drop from the EFFECTIVE radius, not the composition's.
        // Deriving it from the settings value gave a caption that set its own
        // radius an offset of zero, so its shadow sat directly under the
        // glyphs where they hid it.
        shadow_offset: style
            .and_then(|s| s.shadow_offset)
            .or(settings.subtitle_shadow_offset)
            .unwrap_or_else(|| {
                [
                    0.0,
                    get(|s| s.shadow_radius, settings.subtitle_shadow_radius) / 2.0,
                ]
            }),
        padding: get(|s| s.padding, settings.subtitle_background_padding),
        corner_radius: get(
            |s| s.corner_radius,
            settings.subtitle_background_corner_radius,
        ),
        left_margin: get(|s| s.left_margin, settings.subtitle_left_margin),
        right_margin: get(|s| s.right_margin, settings.subtitle_right_margin),
        vertical_margin: get(|s| s.vertical_margin, settings.subtitle_vertical_margin),
        // Per caption only — there is no composition-wide caption placement,
        // and that is a choice: the settings margins are the composition's
        // statement of where captions live, and a placement is one caption
        // saying otherwise.
        placement: style.and_then(|s| s.placement.clone()),
        line_height: 1.25,
        // Let promo-text choose from the text colour.
        smoothing: None,
    }
}

/// Hex + alpha as straight RGBA bytes.
fn rgba_bytes(hex: &str, alpha: f64) -> [u8; 4] {
    let c = rgba_from_hex(hex);
    [
        (c[0] * 255.0).round() as u8,
        (c[1] * 255.0).round() as u8,
        (c[2] * 255.0).round() as u8,
        (alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

fn rgba_from_hex(hex: &str) -> [f32; 4] {
    let mut value = hex.trim().to_uppercase();
    if let Some(stripped) = value.strip_prefix('#') {
        value = stripped.to_string();
    }
    let Ok(parsed) = u64::from_str_radix(&value, 16) else {
        return [0.0, 0.0, 0.0, 1.0];
    };
    if value.len() != 6 {
        return [0.0, 0.0, 0.0, 1.0];
    }
    [
        ((parsed & 0xFF0000) >> 16) as f32 / 255.0,
        ((parsed & 0x00FF00) >> 8) as f32 / 255.0,
        (parsed & 0x0000FF) as f32 / 255.0,
        1.0,
    ]
}

// Fixtures shared by both suites. Kept out of either module so the IOSurface
// path and the portable path assert against the same composition — that is
// what makes a disagreement between them meaningful.
#[cfg(test)]
mod tests_support {
    use super::*;

    pub(super) fn fixture_meta(canvas: f64) -> ProjectMetadata {
        let json = format!(
            r#"{{
            "id": "AAAAAAAA-0000-0000-0000-000000000001",
            "name": "preview", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 0, "videoDuration": 0,
            "subtitles": [],
            "compositionSettings": {{
                "canvasWidth": {canvas}, "canvasHeight": {canvas},
                "backgroundColorHex": "003300"
            }},
            "layers": [
                {{"id": "BG", "name": "bg", "sortIndex": 0, "kind": "background",
                  "isEnabled": true, "startTime": 0, "keyframes": []}},
                {{"id": "VID", "name": "clip", "sortIndex": 1, "kind": "video",
                  "isEnabled": true, "startTime": 2,
                  "resourceID": "AAAAAAAA-0000-0000-0000-00000000BB01",
                  "keyframes": [{{"id": "K", "time": 0, "zoom": 0.5,
                    "verticalShift": {q}, "horizontalShift": {q},
                    "transitionDuration": 0}}]}}
            ],
            "resources": [
                {{"id": "AAAAAAAA-0000-0000-0000-00000000BB01", "kind": "video",
                  "filename": "c.mp4", "displayName": "c", "addedAt": 0,
                  "duration": 10, "trimStart": 1, "trimEnd": 9,
                  "imageCuts": [], "disabledAudioTrackIndices": []}}
            ]}}"#,
            canvas = canvas,
            q = canvas / 4.0,
        );
        ProjectMetadata::from_json(&json).expect("fixture")
    }
}

// The IOSurface suite: still the reference for the Apple path.
#[cfg(all(test, any(target_os = "macos", target_os = "ios")))]
mod tests {
    use super::*;
    use promo_gpu::iosurface::OwnedIoSurface;
    use std::ffi::CStr;
    use std::sync::Mutex;

    /// Test provider state: per-layer BGRA fill color; keeps every surface it
    /// hands out alive (the engine retains its own reference on top) and
    /// records each request.
    struct ProviderState {
        colors: Vec<(String, [u8; 4], usize)>, // (layer id, BGRA, size px)
        keep_alive: Vec<OwnedIoSurface>,
        requests: Vec<(String, f64)>,
    }

    extern "C" fn test_provider(
        user: *mut c_void,
        layer_id: *const c_char,
        _resource_id: *const c_char,
        source_time: f64,
        _tier: i32,
        out_surface: *mut HostSurface,
        out_flags: *mut i32,
    ) -> i32 {
        let state = unsafe { &*(user as *const Mutex<ProviderState>) };
        let mut state = state.lock().unwrap();
        let id = unsafe { CStr::from_ptr(layer_id) }
            .to_string_lossy()
            .to_string();
        state.requests.push((id.clone(), source_time));
        let Some((_, color, size)) = state.colors.iter().find(|(l, _, _)| *l == id).cloned() else {
            return 1;
        };
        let surface = OwnedIoSurface::new_bgra(size, size).expect("surface");
        surface
            .write_pixels(&color.repeat(size * size))
            .expect("fill");
        unsafe {
            *out_surface = HostSurface {
                kind: SURFACE_IOSURFACE,
                handle: surface.raw(),
                ..Default::default()
            };
            *out_flags = 0;
        }
        state.keep_alive.push(surface);
        0
    }

    fn make_engine(
        meta: ProjectMetadata,
        colors: Vec<(String, [u8; 4], usize)>,
        budget: usize,
    ) -> (PreviewEngine, Box<Mutex<ProviderState>>) {
        let state = Box::new(Mutex::new(ProviderState {
            colors,
            keep_alive: Vec::new(),
            requests: Vec::new(),
        }));
        let user = &*state as *const Mutex<ProviderState> as *mut c_void;
        let engine = PreviewEngine::new(meta, test_provider, user, budget).expect("engine");
        (engine, state)
    }

    fn pixel(out: &OwnedIoSurface, x: usize, y: usize) -> [u8; 4] {
        let px = out.read_pixels().unwrap();
        let i = (y * out.width + x) * 4;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    }

    #[test]
    fn renders_background_and_video_layer() {
        let meta = tests_support::fixture_meta(64.0);
        let (mut engine, state) = make_engine(
            meta,
            vec![("VID".into(), [255, 0, 0, 255], 32)], // blue frame
            64 << 20,
        );
        let out = OwnedIoSurface::new_bgra(64, 64).unwrap();
        engine.render(3.0, out.raw(), 64, 64).expect("render");

        // Background: layer has no color keyframes → settings 003300.
        assert_eq!(pixel(&out, 2, 2), [0, 51, 0, 255], "background");
        // Video quad: zoom 0.5 → 32px at (16,16); center is the blue frame.
        assert_eq!(pixel(&out, 30, 30), [255, 0, 0, 255], "video frame");

        // The video source time honors the resource trim (trimStart 1):
        // t=3 → local 1 → source 1 + 1 = 2.
        let requests = &state.lock().unwrap().requests;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, "VID");
        assert!((requests[0].1 - 2.0).abs() < 1e-9, "got {}", requests[0].1);
    }

    /// A BACKGROUND resource paints the scene: its colour floods the
    /// canvas, and its image plate draws bare per its fill — here FIT, so
    /// the plate letterboxes over the resource's own colour.
    #[test]
    fn a_background_resource_paints_colour_and_fitted_plate() {
        let json = r#"{
            "id": "AAAAAAAA-0000-0000-0000-000000000041",
            "name": "bg", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 0, "videoDuration": 0,
            "subtitles": [],
            "compositionSettings": {
                "canvasWidth": 96, "canvasHeight": 48,
                "backgroundColorHex": "00FF00"
            },
            "layers": [
                {"id": "BG", "name": "bg", "sortIndex": 0, "kind": "background",
                 "isEnabled": true, "startTime": 0,
                 "resourceID": "AAAAAAAA-0000-0000-0000-00000000BB41",
                 "keyframes": []}
            ],
            "resources": [
                {"id": "AAAAAAAA-0000-0000-0000-00000000BB41",
                 "kind": "background", "filename": "plate.png",
                 "displayName": "Plate", "addedAt": 0,
                 "imageCuts": [], "disabledAudioTrackIndices": [],
                 "background": {"fill": "fit", "colorHex": "FF0000"}}
            ]}"#;
        let meta = ProjectMetadata::from_json(json).expect("background fixture");
        assert_eq!(meta.minimum_reader_version(), 16, "background is rung 16");
        let (mut engine, _state) = make_engine(
            meta,
            vec![("BG".into(), [255, 0, 0, 255], 32)], // blue 32x32 plate
            64 << 20,
        );
        let out = OwnedIoSurface::new_bgra(96, 48).unwrap();
        engine.render(1.0, out.raw(), 96, 48).expect("render");
        // FIT: 32x32 into 96x48 -> 48x48 centred at x=24..72.
        assert_eq!(pixel(&out, 48, 24), [255, 0, 0, 255], "plate centre");
        // The letterbox shows the RESOURCE's colour, not the settings'.
        assert_eq!(pixel(&out, 6, 24), [0, 0, 255, 255], "resource colour bars");
    }

    /// The whole shadow path at once: settings resolve, the soft quad slides
    /// in UNDER its layer, and — the part only a render can say — the
    /// post-hoc splice leaves every texture index pointing at the right
    /// frame (a broken splice here paints the video with the wrong texture,
    /// not just a missing shadow).
    #[test]
    fn a_media_shadow_draws_under_the_layer_and_shifts_no_textures() {
        let mut meta = tests_support::fixture_meta(64.0);
        // Loud red, fully opaque, 16 canvas px of penumbra (× the layer's
        // 0.5 zoom → 8), offset derived: straight down by half the blur.
        meta.composition_settings.video_shadow_color_hex = "FF0000".into();
        meta.composition_settings.video_shadow_opacity = 1.0;
        meta.composition_settings.video_shadow_radius = 16.0;
        let (mut engine, _state) = make_engine(
            meta,
            vec![("VID".into(), [255, 0, 0, 255], 32)], // blue frame
            64 << 20,
        );
        let out = OwnedIoSurface::new_bgra(64, 64).unwrap();
        engine.render(3.0, out.raw(), 64, 64).expect("render");

        // The media quad still shows ITS texture at its place (16..48).
        assert_eq!(pixel(&out, 30, 30), [255, 0, 0, 255], "video frame");
        // Just below the media's bottom edge (48): the red penumbra over
        // the green background — red present, background green suppressed.
        let below = pixel(&out, 32, 50);
        assert!(
            below[2] > 100 && below[1] < 60,
            "shadow below the layer: {below:?}"
        );
        // Far corners stay pure background: the shadow ends.
        assert_eq!(pixel(&out, 2, 2), [0, 51, 0, 255], "background untouched");
    }

    /// The live canvas's miss rule: a provider that cannot serve a frame
    /// (a long video's mid-GOP seek failing under playback pressure) must
    /// leave the layer showing its LAST picture, not blank it for one
    /// render — the blank-every-few-ticks failure reads as blinking. A
    /// layer never shown stays absent: there is nothing to hold.
    #[test]
    fn a_provider_miss_holds_the_last_frame_instead_of_blinking() {
        let meta = tests_support::fixture_meta(64.0);
        let (mut engine, state) = make_engine(meta, vec![], 64 << 20);
        let out = OwnedIoSurface::new_bgra(64, 64).unwrap();

        // Failing provider, never-shown layer: background, no phantom.
        engine.render(3.0, out.raw(), 64, 64).expect("render");
        assert_eq!(pixel(&out, 30, 30), [0, 51, 0, 255], "no phantom frame");

        // A frame arrives...
        state.lock().unwrap().colors = vec![("VID".into(), [255, 0, 0, 255], 32)];
        engine.render(3.5, out.raw(), 64, 64).expect("render");
        assert_eq!(pixel(&out, 30, 30), [255, 0, 0, 255], "live frame");

        // ...then the decoder misses at a NEW time: the layer holds the
        // last picture instead of blinking out.
        state.lock().unwrap().colors = vec![];
        engine.render(4.5, out.raw(), 64, 64).expect("render");
        assert_eq!(pixel(&out, 30, 30), [255, 0, 0, 255], "held frame");
    }

    #[test]
    fn a_drawing_layer_renders_without_any_provider_frame() {
        // The bug this pins: drawings used to be host-supplied, so a front
        // end that did not rasterize them (the CLI) rendered compositions
        // with the strokes silently missing while `inspect` still called the
        // layer renderable. The engine draws them now — the provider here
        // knows about no drawing at all, and a stroke must still appear.
        let mut meta = tests_support::fixture_meta(64.0);
        let mut resources = meta.resources.clone().unwrap_or_default();
        let drawing: promo_model::ProjectResource = serde_json::from_value(serde_json::json!({
            "id": "DRAW", "kind": "drawing", "filename": "d.json",
            "displayName": "Marks", "addedAt": 0,
            "drawing": {
                "shapes": [{
                    "id": "S1", "kind": "line",
                    "points": [[0.0, 0.0], [100.0, 100.0]],
                    "strokeColorHex": "FF0000", "strokeWidth": 24.0,
                    "arrowStart": false, "arrowEnd": false
                }]
            }
        }))
        .unwrap();
        resources.push(drawing);
        meta.resources = Some(resources);
        let mut layers = meta.layers.clone().unwrap_or_default();
        let layer: ProjectLayer = serde_json::from_value(serde_json::json!({
            "id": "DRAWL", "name": "Marks", "sortIndex": 9, "kind": "drawing",
            "isEnabled": true, "startTime": 0.0, "duration": 100.0,
            "resourceID": "DRAW", "keyframes": []
        }))
        .unwrap();
        layers.push(layer);
        meta.layers = Some(layers);

        let (mut engine, state) = make_engine(meta, vec![], 64 << 20);
        let out = OwnedIoSurface::new_bgra(64, 64).unwrap();
        engine.render(3.0, out.raw(), 64, 64).expect("render");

        assert!(
            !state
                .lock()
                .unwrap()
                .requests
                .iter()
                .any(|r| r.0 == "DRAWL"),
            "the engine must not ask a host for vector content"
        );
        // The stroke runs corner to corner: the centre pixel is on it, and
        // red (not the 003300 background) proves it was actually drawn.
        let centre = pixel(&out, 32, 32);
        assert!(
            centre[2] > 100 && centre[1] < 80,
            "expected a red stroke at the centre, got {centre:?}"
        );
    }

    /// A PALETTE edit must reach the canvas — the case the eviction sweep
    /// structurally cannot see, and so the one that pins the content stamp.
    ///
    /// `set_project` evicts by comparing RESOURCES, but a palette lives in
    /// `compositionSettings`: recolouring `@ink` leaves every resource byte
    /// for byte identical, so nothing is ever marked stale. The raster's
    /// pixel size is unchanged too (it comes from the points), so before the
    /// stamp the key `drawing:{layer}:{w}x{h}` was identical across the edit
    /// and the canvas kept the old colour. The stamp is hashed on RESOLVED
    /// shapes — `@ink` already looked up — which is what makes it move here.
    #[test]
    fn a_palette_edit_re_rasterizes_a_drawing() {
        let build = |ink: &str| {
            let mut meta = tests_support::fixture_meta(64.0);
            meta.composition_settings.palette = Some(vec![promo_model::PaletteColor {
                name: "ink".into(),
                color_hex: ink.into(),
            }]);
            let mut resources = meta.resources.clone().unwrap_or_default();
            resources.push(
                serde_json::from_value(serde_json::json!({
                    "id": "DRAW", "kind": "drawing", "filename": "d.json",
                    "displayName": "Marks", "addedAt": 0,
                    "drawing": {"shapes": [{
                        "id": "S1", "kind": "line",
                        "points": [[0.0, 0.0], [100.0, 100.0]],
                        "strokeColorHex": "@ink", "strokeWidth": 24.0,
                        "arrowStart": false, "arrowEnd": false
                    }]}
                }))
                .unwrap(),
            );
            meta.resources = Some(resources);
            let mut layers = meta.layers.clone().unwrap_or_default();
            layers.push(
                serde_json::from_value(serde_json::json!({
                    "id": "DRAWL", "name": "Marks", "sortIndex": 9, "kind": "drawing",
                    "isEnabled": true, "startTime": 0.0, "duration": 100.0,
                    "resourceID": "DRAW", "keyframes": []
                }))
                .unwrap(),
            );
            meta.layers = Some(layers);
            meta
        };

        let (mut engine, _state) = make_engine(build("FF0000"), vec![], 64 << 20);
        let out = OwnedIoSurface::new_bgra(64, 64).unwrap();
        engine.render(3.0, out.raw(), 64, 64).expect("render");
        let before = pixel(&out, 32, 32);
        assert!(
            before[2] > 100 && before[1] < 80,
            "the stroke starts red, got {before:?}"
        );

        // The ONLY change is the palette entry. No resource moves, so the
        // eviction sweep never fires; only a content-derived key can notice.
        engine.set_project(build("00FF00"));
        engine.render(3.0, out.raw(), 64, 64).expect("render");
        let after = pixel(&out, 32, 32);
        assert!(
            after[1] > 100 && after[2] < 80,
            "the recoloured stroke must reach the canvas, got {after:?}"
        );
    }

    #[test]
    fn overlay_composites_on_top() {
        let meta = tests_support::fixture_meta(64.0);
        let (mut engine, _state) = make_engine(
            meta,
            vec![("VID".into(), [255, 0, 0, 255], 32)], // blue video frame
            64 << 20,
        );
        let out = OwnedIoSurface::new_bgra(64, 64).unwrap();

        // Opaque white overlay covering the canvas hides everything below.
        let overlay = OwnedIoSurface::new_bgra(64, 64).expect("overlay");
        overlay
            .write_pixels(&[255, 255, 255, 255].repeat(64 * 64))
            .unwrap();
        engine
            .render_with_overlay(3.0, out.raw(), 64, 64, Some((overlay.raw(), 64, 64)))
            .expect("render");
        assert_eq!(
            pixel(&out, 30, 30),
            [255, 255, 255, 255],
            "overlay covers video"
        );
        assert_eq!(pixel(&out, 2, 2), [255, 255, 255, 255], "overlay covers bg");

        // A transparent overlay leaves the composition untouched.
        let clear = OwnedIoSurface::new_bgra(64, 64).expect("clear");
        clear.write_pixels(&[0, 0, 0, 0].repeat(64 * 64)).unwrap();
        engine
            .render_with_overlay(3.0, out.raw(), 64, 64, Some((clear.raw(), 64, 64)))
            .expect("render");
        assert_eq!(pixel(&out, 30, 30), [255, 0, 0, 255], "video still visible");
        assert_eq!(
            pixel(&out, 2, 2),
            [0, 51, 0, 255],
            "background still visible"
        );
    }

    #[test]
    fn set_project_keeps_cache_for_untouched_layers() {
        let meta = tests_support::fixture_meta(64.0);
        let (mut engine, _state) = make_engine(
            meta.clone(),
            vec![("VID".into(), [255, 0, 0, 255], 32)],
            64 << 20,
        );
        let out = OwnedIoSurface::new_bgra(64, 64).unwrap();
        engine.render(3.0, out.raw(), 64, 64).unwrap();
        assert_eq!(engine.stats().misses, 1);

        // An edit that leaves VID alone: its cached frame survives, so the
        // next render is a hit and the provider is not called again.
        let mut edited = meta.clone();
        if let Some(layers) = edited.layers.as_mut() {
            if let Some(bg) = layers.iter_mut().find(|l| l.id == "BG") {
                bg.name = "renamed".into();
            }
        }
        engine.set_project(edited);
        engine.render(3.0, out.raw(), 64, 64).unwrap();
        assert_eq!(engine.stats().misses, 1, "untouched layer keeps its frame");
        assert!(engine.stats().hits >= 1);

        // An edit that DOES touch VID drops its frames.
        let mut retimed = meta.clone();
        if let Some(layers) = retimed.layers.as_mut() {
            if let Some(vid) = layers.iter_mut().find(|l| l.id == "VID") {
                vid.start_time += 0.5;
            }
        }
        engine.set_project(retimed);
        engine.render(3.5, out.raw(), 64, 64).unwrap();
        assert_eq!(engine.stats().misses, 2, "edited layer re-fetches");
    }

    #[test]
    fn caches_frames_and_reports_hits() {
        let meta = tests_support::fixture_meta(64.0);
        let (mut engine, state) =
            make_engine(meta, vec![("VID".into(), [0, 255, 0, 255], 32)], 64 << 20);
        let out = OwnedIoSurface::new_bgra(64, 64).unwrap();
        engine.render(3.0, out.raw(), 64, 64).unwrap();
        engine.render(3.0, out.raw(), 64, 64).unwrap();
        engine.render(3.0004, out.raw(), 64, 64).unwrap(); // same 1ms bucket

        let stats = engine.stats();
        assert_eq!(stats.misses, 1, "one decode");
        assert_eq!(stats.hits, 2, "two cache hits");
        assert_eq!(state.lock().unwrap().requests.len(), 1);
        assert_eq!(stats.cached_bytes, 32 * 32 * 4);
    }

    /// A JSON fixture with two image layers: IMG (plain) and TILT (device
    /// frame + tilt keyframes). Only the latter must be requested per-time.
    fn tilt_fixture_meta(canvas: f64) -> ProjectMetadata {
        let json = format!(
            r#"{{
            "id": "AAAAAAAA-0000-0000-0000-000000000002",
            "name": "tilt", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 0, "videoDuration": 0,
            "subtitles": [],
            "compositionSettings": {{
                "canvasWidth": {canvas}, "canvasHeight": {canvas},
                "backgroundColorHex": "003300"
            }},
            "layers": [
                {{"id": "IMG", "name": "flat", "sortIndex": 0, "kind": "image",
                  "isEnabled": true, "startTime": 0, "duration": 10,
                  "resourceID": "AAAAAAAA-0000-0000-0000-00000000CC01",
                  "keyframes": []}},
                {{"id": "TILT", "name": "framed", "sortIndex": 1, "kind": "image",
                  "isEnabled": true, "startTime": 0, "duration": 10,
                  "resourceID": "AAAAAAAA-0000-0000-0000-00000000CC02",
                  "keyframes": [
                    {{"id": "K1", "time": 0, "zoom": 1, "verticalShift": 0,
                      "horizontalShift": 0, "transitionDuration": 0,
                      "tiltX": 0, "tiltY": 0}},
                    {{"id": "K2", "time": 5, "zoom": 1, "verticalShift": 0,
                      "horizontalShift": 0, "transitionDuration": 1,
                      "tiltX": 20, "tiltY": -10}}
                  ]}}
            ],
            "resources": [
                {{"id": "AAAAAAAA-0000-0000-0000-00000000CC01", "kind": "image",
                  "filename": "a.png", "displayName": "a", "addedAt": 0,
                  "imageCuts": [], "disabledAudioTrackIndices": []}},
                {{"id": "AAAAAAAA-0000-0000-0000-00000000CC02", "kind": "image",
                  "filename": "b.png", "displayName": "b", "addedAt": 0,
                  "imageCuts": [], "disabledAudioTrackIndices": [],
                  "frame": {{"kind": "device"}}}}
            ]}}"#,
            canvas = canvas,
        );
        ProjectMetadata::from_json(&json).expect("tilt fixture")
    }

    #[test]
    fn animated_tilt_image_is_requested_per_time() {
        let meta = tilt_fixture_meta(64.0);
        let (mut engine, state) = make_engine(
            meta,
            vec![
                ("IMG".into(), [255, 0, 0, 255], 16),
                ("TILT".into(), [0, 255, 0, 255], 16),
            ],
            64 << 20,
        );
        let out = OwnedIoSurface::new_bgra(64, 64).unwrap();
        engine.render(1.0, out.raw(), 64, 64).unwrap();
        engine.render(5.5, out.raw(), 64, 64).unwrap();

        let requests = state.lock().unwrap().requests.clone();
        let img: Vec<f64> = requests
            .iter()
            .filter(|(l, _)| l == "IMG")
            .map(|(_, t)| *t)
            .collect();
        let tilt: Vec<f64> = requests
            .iter()
            .filter(|(l, _)| l == "TILT")
            .map(|(_, t)| *t)
            .collect();
        // Plain image: one static request, cached across renders.
        assert_eq!(img, vec![-1.0]);
        // Device frame with tilt keyframes: requested at each render time so
        // the provider can re-bake the interpolated tilt.
        assert_eq!(tilt, vec![1.0, 5.5]);
    }

    #[test]
    fn prefetch_makes_render_all_hits() {
        let meta = tests_support::fixture_meta(64.0);
        let (mut engine, state) =
            make_engine(meta, vec![("VID".into(), [255, 128, 0, 255], 32)], 64 << 20);
        assert_eq!(engine.prefetch(3.0), 1, "one video frame fetched");
        assert_eq!(engine.prefetch(3.0), 0, "already cached");
        let out = OwnedIoSurface::new_bgra(64, 64).unwrap();
        engine.render(3.0, out.raw(), 64, 64).unwrap();
        let stats = engine.stats();
        assert_eq!(stats.misses, 1, "render decoded nothing new");
        assert_eq!(stats.hits, 2, "prefetch re-check + render hit");
        assert_eq!(state.lock().unwrap().requests.len(), 1);
    }

    /// A word-by-word reveal must actually show fewer words early on — the
    /// geometry is unit-tested in promo-timeline, but only a render says the
    /// bands the engine emits crop the raster the way they describe.
    #[test]
    fn a_reveal_shows_the_caption_a_piece_at_a_time() {
        let json = r#"{
            "id": "AAAAAAAA-0000-0000-0000-00000000000C",
            "name": "reveal", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
            "compositionSettings": {
                "canvasWidth": 512, "canvasHeight": 128,
                "backgroundColorHex": "000000",
                "subtitleFontSize": 36, "subtitleColorHex": "FFFFFF",
                "subtitleVerticalMargin": 30, "subtitleBackgroundOpacity": 0,
                "subtitleLeftMargin": 10, "subtitleRightMargin": 10
            },
            "layers": [
                {"id": "CAP", "name": "words", "sortIndex": 0, "kind": "caption",
                 "isEnabled": true, "startTime": 0, "duration": 4,
                 "captionText": "one two three four",
                 "captionStyle": {"reveal": {"by": "word", "mode": "wipe"}},
                 "keyframes": []}
            ]}"#;
        let meta = ProjectMetadata::from_json(json).expect("reveal fixture");
        let (mut engine, _state) = make_engine(meta, vec![], 64 << 20);
        let out = OwnedIoSurface::new_bgra(512, 128).unwrap();
        let mut ink = |time: f64| -> usize {
            engine.render(time, out.raw(), 512, 128).expect("render");
            out.read_pixels()
                .unwrap()
                .chunks_exact(4)
                .filter(|p| p[1] > 100)
                .count()
        };

        let first = ink(0.0);
        let half = ink(2.0);
        let whole = ink(3.9);
        assert!(
            first > 0,
            "the first word is there when the caption is ({first} px)"
        );
        assert!(
            half > first,
            "more words by the middle: {first} then {half}"
        );
        assert!(
            whole > half,
            "and all of them by the end: {half} then {whole}"
        );

        // The reveal must not MOVE the caption: laid out whole, the last
        // word lands where it always was.
        engine.render(3.9, out.raw(), 512, 128).expect("render");
        let revealed = out.read_pixels().unwrap();
        let plain = r#"{"reveal": null}"#;
        let _ = plain;
        let leftmost = |px: &[u8]| -> usize {
            (0..512)
                .find(|x| (0..128).any(|y| px[(y * 512 + x) * 4 + 1] > 100))
                .unwrap_or(usize::MAX)
        };
        let x_when_revealed = leftmost(&revealed);
        assert!(x_when_revealed < 512, "something rendered");
        // The first word starts at the same x throughout — nothing re-flows.
        engine.render(0.0, out.raw(), 512, 128).expect("render");
        let at_start = out.read_pixels().unwrap();
        assert_eq!(
            leftmost(&at_start),
            x_when_revealed,
            "a caption that re-flows as it types has been laid out per frame"
        );
    }

    /// A staggered reveal changes WHEN each word arrives, never where it
    /// ends up: the words must flow the way the caption laid them out, not
    /// stack one per line. The wipe is the oracle — it is known to keep the
    /// layout, so once everything has landed the two must be pixel-alike.
    #[test]
    fn a_staggered_reveal_moves_each_word_and_keeps_the_layout() {
        // Long enough to wrap, so "one unit per line" would be obvious.
        let fixture = |text: &str, mode: &str, extra: &str| {
            format!(
                r#"{{
            "id": "AAAAAAAA-0000-0000-0000-00000000000D",
            "name": "rise", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
            "compositionSettings": {{
                "canvasWidth": 512, "canvasHeight": 256,
                "backgroundColorHex": "000000",
                "subtitleFontSize": 36, "subtitleColorHex": "FFFFFF",
                "subtitleVerticalMargin": 30, "subtitleBackgroundOpacity": 0,
                "subtitleLeftMargin": 10, "subtitleRightMargin": 10
            }},
            "layers": [
                {{"id": "CAP", "name": "words", "sortIndex": 0, "kind": "caption",
                 "isEnabled": true, "startTime": 0, "duration": 4,
                 "captionText": "{text}",
                 "captionStyle": {{"reveal":
                    {{"by": "word", "mode": "{mode}"{extra}}}}},
                 "keyframes": []}}
            ]}}"#
            )
        };
        const WRAPS: &str = "one two three four five six seven";
        const RISE: &str = r#", "rise": 0.4"#;
        let out = OwnedIoSurface::new_bgra(512, 256).unwrap();
        // A low threshold on purpose: a word part-way through its arrival is
        // dim, and the question is where it IS, not how solid.
        let scan = |px: &[u8]| -> (Vec<usize>, Vec<usize>) {
            let lit = |x: usize, y: usize| px[(y * 512 + x) * 4 + 1] > 40;
            (
                (0..256).filter(|y| (0..512).any(|x| lit(x, *y))).collect(),
                (0..512).filter(|x| (0..256).any(|y| lit(*x, y))).collect(),
            )
        };
        let render = |json: String, time: f64| {
            let meta = ProjectMetadata::from_json(&json).expect("reveal fixture");
            let (mut engine, _state) = make_engine(meta, vec![], 64 << 20);
            engine.render(time, out.raw(), 512, 256).expect("render");
            scan(&out.read_pixels().unwrap())
        };

        // How many separate lines of text the ink falls on.
        let lines =
            |rows: &[usize]| rows.windows(2).filter(|pair| pair[1] > pair[0] + 1).count() + 1;
        let (wipe_rows, wipe_cols) = render(fixture(WRAPS, "wipe", ""), 3.99);
        let (rise_rows, rise_cols) = render(fixture(WRAPS, "rise", RISE), 3.99);
        assert_eq!(lines(&wipe_rows), 2, "the fixture wraps onto two lines");
        assert_eq!(
            lines(&rise_rows),
            2,
            "so does the stagger — the words flow as laid out, not one per line",
        );
        // Not pixel-exact: cropping per word instead of per line moves the
        // seams, which shifts the odd edge pixel by a level or two. The
        // bounding box is what says the words landed where they belong.
        let box_of = |rows: &[usize], cols: &[usize]| {
            (
                rows[0],
                *rows.last().unwrap(),
                cols[0],
                *cols.last().unwrap(),
            )
        };
        let wipe_box = box_of(&wipe_rows, &wipe_cols);
        let rise_box = box_of(&rise_rows, &rise_cols);
        let near = |a: usize, b: usize| a.abs_diff(b) <= 1;
        assert!(
            near(rise_box.0, wipe_box.0)
                && near(rise_box.1, wipe_box.1)
                && near(rise_box.2, wipe_box.2)
                && near(rise_box.3, wipe_box.3),
            "once every word has landed a stagger fills the same place as a \
             wipe: {rise_box:?} against {wipe_box:?}",
        );
        assert!(
            rise_cols.len().abs_diff(wipe_cols.len()) <= 2,
            "and lights the same columns — words that piled up would leave gaps",
        );

        // Mid-flight, a rising word sits below where it lands. Measured
        // against a FADE at the same instant, not against the finished
        // frame: same word, same dimness, so the only thing that can move
        // the ink is the rise itself.
        let alone = |mode: &str, extra: &str| render(fixture("one", mode, extra), 1.6);
        let (rising, rising_cols) = alone("rise", RISE);
        let (fading, fading_cols) = alone("fade", "");
        assert!(
            !rising.is_empty() && !fading.is_empty(),
            "the word is arriving"
        );
        assert!(
            rising[0] > fading[0] + 2,
            "a word on its way up sits LOWER than one merely fading in: {} \
             against {}",
            rising[0],
            fading[0],
        );
        assert_eq!(
            rising_cols, fading_cols,
            "and does not drift sideways doing it"
        );

        // And a fade is exactly a rise that travels nowhere.
        let (still, _) = render(fixture("one", "rise", r#", "rise": 0"#), 1.6);
        assert_eq!(still, fading, "rise 0 is a fade");
    }

    /// Motion blur fixture: a caption on an opaque plate, sliding across a
    /// black canvas via shift keyframes. The plate's edge is the
    /// measurement: sharp, it steps from background to plate inside a pixel
    /// or two of antialiasing; blurred, it ramps across the distance the
    /// plate travels while the shutter is open.
    fn blur_fixture(vertical: f64, plate_hex: &str, blur: &str) -> String {
        format!(
            r#"{{"id":"CAP-{plate_hex}", "name":"mover", "sortIndex": 1, "kind":"caption",
                "isEnabled": true, "startTime": 0, "duration": 1,
                "captionText": "MOTION"{blur},
                "captionStyle": {{"backgroundColorHex": "{plate_hex}",
                                  "backgroundOpacity": 1.0, "fontSize": 18}},
                "keyframes": [
                  {{"id":"K0-{plate_hex}", "time": 0,
                    "horizontalShift": -200, "verticalShift": {vertical},
                    "transitionDuration": 0}},
                  {{"id":"K1-{plate_hex}", "time": 1,
                    "horizontalShift": 200, "verticalShift": {vertical},
                    "transitionDuration": 1}}
                ]}}"#
        )
    }

    fn blur_project(layers: &str) -> ProjectMetadata {
        let json = format!(
            r#"{{"id": "AAAAAAAA-0000-0000-0000-00000000000E",
                 "name": "blur", "createdAt": 0, "state": "recorded",
                 "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
                 "compositionSettings": {{
                    "canvasWidth": 512, "canvasHeight": 128,
                    "backgroundColorHex": "000000",
                    "subtitleFontSize": 18, "subtitleColorHex": "FFFFFF",
                    "subtitleVerticalMargin": 8, "subtitleBackgroundOpacity": 1.0,
                    "subtitleLeftMargin": 10, "subtitleRightMargin": 10
                 }},
                 "layers": [{layers}]}}"#
        );
        ProjectMetadata::from_json(&json).expect("blur fixture")
    }

    /// Columns at `row` where `channel` sits between clearly-background and
    /// clearly-plate: the width of the edge ramp, which is what a shutter
    /// widens.
    fn ramp_columns(px: &[u8], row: usize, channel: usize) -> usize {
        (0..512)
            .filter(|x| {
                let v = px[(row * 512 + x) * 4 + channel];
                v > 40 && v < 215
            })
            .count()
    }

    /// The centre row of the horizontal band whose `channel` is dominant —
    /// found by scanning, so the test does not hard-code caption layout.
    fn band_centre_row(px: &[u8], channel: usize) -> usize {
        let rows: Vec<usize> = (0..128)
            .filter(|y| {
                (0..512).any(|x| {
                    let o = (y * 512 + x) * 4;
                    px[o + channel] > 200
                        && (0..4).all(|c| c == channel || c == 3 || px[o + c] <= px[o + channel])
                })
            })
            .collect();
        rows[rows.len() / 2]
    }

    /// A layer that asks for a shutter smears by the distance it travels
    /// while the shutter is open; the same layer without the field keeps its
    /// hard edge. The plate's edge ramp is the measurement, so the test
    /// reads the OUTCOME — softened pixels — not the sample count.
    #[test]
    fn a_blurred_mover_smears_and_a_sharp_one_does_not() {
        let sharp_meta = blur_project(&blur_fixture(0.0, "FF0000", ""));
        let blurred_meta = blur_project(&blur_fixture(
            0.0,
            "FF0000",
            r#", "motionBlur": {"shutter": 1.0}"#,
        ));
        let out = OwnedIoSurface::new_bgra(512, 128).unwrap();
        let render = |meta: ProjectMetadata| {
            let (mut engine, _state) = make_engine(meta, vec![], 64 << 20);
            engine.render(0.5, out.raw(), 512, 128).expect("render");
            out.read_pixels().unwrap()
        };
        let sharp = render(sharp_meta);
        let blurred = render(blurred_meta);

        let row = band_centre_row(&sharp, 2);
        let sharp_ramp = ramp_columns(&sharp, row, 2);
        let blurred_ramp = ramp_columns(&blurred, band_centre_row(&blurred, 2), 2);
        // 400 canvas-px/s under a full-frame shutter at the default 30fps is
        // a 13px smear; antialiasing alone is a pixel or two per edge.
        assert!(
            sharp_ramp <= 6,
            "a sharp plate edge is antialiasing-wide, got {sharp_ramp} ramp columns",
        );
        assert!(
            blurred_ramp >= sharp_ramp + 6,
            "the shutter should widen the ramp well past antialiasing: \
             {sharp_ramp} sharp, {blurred_ramp} blurred",
        );
    }

    /// The blur is PER LAYER: two captions ride the same move, one asks for
    /// a shutter, and only that one smears. The other resolves at the
    /// frame's own time in every sub-sample — pinned by construction, and
    /// this is the test that fails if the pin ever loosens.
    #[test]
    fn blur_is_per_layer_not_per_frame() {
        let meta = blur_project(&format!(
            "{},{}",
            blur_fixture(64.0, "FF0000", r#", "motionBlur": {"shutter": 1.0}"#),
            blur_fixture(0.0, "0000FF", "")
        ));
        let out = OwnedIoSurface::new_bgra(512, 128).unwrap();
        let (mut engine, _state) = make_engine(meta, vec![], 64 << 20);
        engine.render(0.5, out.raw(), 512, 128).expect("render");
        let px = out.read_pixels().unwrap();

        let red_ramp = ramp_columns(&px, band_centre_row(&px, 2), 2);
        let blue_ramp = ramp_columns(&px, band_centre_row(&px, 0), 0);
        assert!(
            red_ramp >= blue_ramp + 6,
            "the blurred plate smears ({red_ramp}) while its sharp twin does \
             not ({blue_ramp}) — anything else means the pin failed",
        );
        assert!(
            blue_ramp <= 6,
            "the sharp layer stays antialiasing-sharp: {blue_ramp}"
        );
    }

    /// A blurred layer that is not actually moving costs nothing and changes
    /// nothing: the walk measures the shutter's endpoints, sees no travel,
    /// and renders the one sharp frame — bit-exact, which is what proves the
    /// early-out ran instead of a 24-sample average of identical scenes.
    #[test]
    fn a_still_blurred_layer_renders_bit_exact_sharp() {
        let still = |blur: &str| {
            blur_project(&format!(
                r#"{{"id":"CAP", "name":"still", "sortIndex": 1, "kind":"caption",
                    "isEnabled": true, "startTime": 0, "duration": 1,
                    "captionText": "STILL"{blur},
                    "captionStyle": {{"backgroundColorHex": "FF0000",
                                      "backgroundOpacity": 1.0, "fontSize": 18}},
                    "keyframes": []}}"#
            ))
        };
        let out = OwnedIoSurface::new_bgra(512, 128).unwrap();
        let render = |meta: ProjectMetadata| {
            let (mut engine, _state) = make_engine(meta, vec![], 64 << 20);
            engine.render(0.5, out.raw(), 512, 128).expect("render");
            out.read_pixels().unwrap()
        };
        let sharp = render(still(""));
        let blurred = render(still(r#", "motionBlur": {"shutter": 1.0}"#));
        assert_eq!(sharp, blurred, "no motion, no difference — to the bit");

        // And the cost: two endpoint probes plus one sharp frame. Honest
        // scope: a minimal three-sample walk ALSO builds three scenes, so
        // this pins the probe design (and documents the cost), not the
        // early-out — whose saving is the skipped GPU accumulation, which
        // no assertion here can see. The bit-exact check above is the
        // correctness half; this is the ledger.
        let (mut engine, _state) = make_engine(
            still(r#", "motionBlur": {"shutter": 1.0}"#),
            vec![],
            64 << 20,
        );
        engine.render(0.5, out.raw(), 512, 128).expect("render");
        assert_eq!(
            engine.builds, 3,
            "two endpoint probes plus the sharp frame — the cap fallback would be 8",
        );
    }

    /// A keyframed shutter is a RAMP: the same move renders nearly sharp
    /// where the shutter is closed and smeared where it has opened — which
    /// is the whip-pan idiom, blur arriving with the speed.
    #[test]
    fn a_keyframed_shutter_ramps_the_blur_in() {
        let meta = blur_project(
            r#"{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0B01", "name":"mover",
                "sortIndex": 1, "kind":"caption",
                "isEnabled": true, "startTime": 0, "duration": 1,
                "captionText": "MOTION",
                "captionStyle": {"backgroundColorHex": "FF0000",
                                  "backgroundOpacity": 1.0, "fontSize": 18},
                "keyframes": [
                  {"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0B02", "time": 0,
                    "horizontalShift": -200, "verticalShift": 0,
                    "shutter": 0, "transitionDuration": 0},
                  {"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0B03", "time": 1,
                    "horizontalShift": 200, "verticalShift": 0,
                    "shutter": 1, "transitionDuration": 1}
                ]}"#,
        );
        let out = OwnedIoSurface::new_bgra(512, 128).unwrap();
        let (mut engine, _state) = make_engine(meta, vec![], 64 << 20);
        let mut ramp_at = |time: f64| {
            engine.render(time, out.raw(), 512, 128).expect("render");
            let px = out.read_pixels().unwrap();
            ramp_columns(&px, band_centre_row(&px, 2), 2)
        };
        // Same speed both times — only the shutter differs.
        let early = ramp_at(0.1);
        let late = ramp_at(0.9);
        assert!(
            late >= early + 6,
            "the blur should arrive with the ramp: {early} columns early, \
             {late} late",
        );
    }

    /// When a layer carries BOTH the constant and keyframed shutters, the
    /// keyframes win — and a keyframed zero really is sharp, bit-exact, so
    /// "ramp to nothing" costs nothing.
    #[test]
    fn keyframed_shutter_zero_beats_the_constant_and_is_free() {
        let mover = |blur: &str, shutter: &str| {
            blur_project(&format!(
                r#"{{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0B04", "name":"mover",
                    "sortIndex": 1, "kind":"caption",
                    "isEnabled": true, "startTime": 0, "duration": 1,
                    "captionText": "MOTION"{blur},
                    "captionStyle": {{"backgroundColorHex": "FF0000",
                                      "backgroundOpacity": 1.0, "fontSize": 18}},
                    "keyframes": [
                      {{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0B05", "time": 0,
                        "horizontalShift": -200, "verticalShift": 0{shutter},
                        "transitionDuration": 0}},
                      {{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0B06", "time": 1,
                        "horizontalShift": 200, "verticalShift": 0{shutter},
                        "transitionDuration": 1}}
                    ]}}"#
            ))
        };
        let out = OwnedIoSurface::new_bgra(512, 128).unwrap();
        let render = |meta: ProjectMetadata| {
            let (mut engine, _state) = make_engine(meta, vec![], 64 << 20);
            engine.render(0.5, out.raw(), 512, 128).expect("render");
            out.read_pixels().unwrap()
        };
        let sharp = render(mover("", ""));
        let overridden = render(mover(
            r#", "motionBlur": {"shutter": 1.0}"#,
            r#", "shutter": 0"#,
        ));
        assert_eq!(
            sharp, overridden,
            "keyframed zero beats the constant, to the bit"
        );
    }

    /// A push swap's travel smears when the layer asks for blur: the swap's
    /// IDENTITY stays on the frame's clock (a cut inside the shutter stays a
    /// cut, every sub-sample agrees what exists) while its travel reads the
    /// sub-sample's clock. Sharp twin as the control.
    #[test]
    fn a_swap_push_travel_smears_under_blur() {
        let fixture = |blur: &str| {
            format!(
                r#"{{
            "id": "D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0B07",
            "name": "words", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 0, "videoDuration": 0,
            "subtitles": [], "minReaderVersion": 10,
            "compositionSettings": {{
                "canvasWidth": 256, "canvasHeight": 128,
                "backgroundColorHex": "000000",
                "subtitleFontSize": 30, "subtitleVerticalMargin": 30,
                "subtitleBackgroundOpacity": 1.0,
                "subtitleLeftMargin": 10, "subtitleRightMargin": 10
            }},
            "resources": [
                {{"id": "D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0B08", "kind": "caption",
                 "filename": "", "displayName": "one",
                 "addedAt": 0, "captionText": "AAA",
                 "captionStyle": {{"backgroundColorHex": "FF0000"}}}},
                {{"id": "D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0B09", "kind": "caption",
                 "filename": "", "displayName": "two",
                 "addedAt": 0, "captionText": "BBB",
                 "captionStyle": {{"backgroundColorHex": "0000FF"}}}}
            ],
            "layers": [
                {{"id": "D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0B10", "name": "words",
                 "sortIndex": 0, "kind": "caption",
                 "isEnabled": true, "startTime": 0, "duration": 10,
                 "resourceID": "D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0B08"{blur},
                 "keyframes": [
                   {{"id": "D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0B11", "time": 4,
                    "transitionDuration": 0,
                    "resourceID": "D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0B09",
                    "transition": {{"kind": "push", "from": "left", "duration": 0.5}}}}
                 ]}}
            ]}}"#
            )
        };
        let out = OwnedIoSurface::new_bgra(256, 128).unwrap();
        let render = |json: String| {
            let meta = ProjectMetadata::from_json(&json).expect("swap fixture");
            let (mut engine, _state) = make_engine(meta, vec![], 64 << 20);
            // Mid-push: both plates are travelling at canvas-width per
            // half-second, the fastest thing this suite draws.
            engine.render(4.25, out.raw(), 256, 128).expect("render");
            out.read_pixels().unwrap()
        };
        let ramp = |px: &[u8], channel: usize| -> usize {
            let row = (0..128)
                .filter(|y| (0..256).any(|x| px[((y * 256 + x) * 4) + channel] > 200))
                .collect::<Vec<_>>();
            let row = row[row.len() / 2];
            (0..256)
                .filter(|x| {
                    let v = px[((row * 256 + x) * 4) + channel];
                    v > 40 && v < 215
                })
                .count()
        };
        let sharp = render(fixture(""));
        let blurred = render(fixture(r#", "motionBlur": {"shutter": 1.0}"#));
        assert!(
            ramp(&blurred, 2) >= ramp(&sharp, 2) + 6,
            "the outgoing plate's travel should smear: sharp {} vs blurred {}",
            ramp(&sharp, 2),
            ramp(&blurred, 2),
        );
        assert!(
            ramp(&blurred, 0) >= ramp(&sharp, 0) + 6,
            "and the incoming plate's too: sharp {} vs blurred {}",
            ramp(&sharp, 0),
            ramp(&blurred, 0),
        );
    }

    /// The staggered modes' first frame is EMPTY — nothing has arrived —
    /// and it must render that way. The engine used to fall through to the
    /// whole caption when the reveal produced no bands, which flashed the
    /// full text at rest for a frame before every rise: a blink at the top
    /// of every caption in a template, found by watching one.
    #[test]
    fn a_staggered_reveal_does_not_flash_the_whole_caption_first() {
        let fixture = |reveal: &str| {
            blur_project(&format!(
                r#"{{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0D01", "name":"riser",
                    "sortIndex": 1, "kind":"caption",
                    "isEnabled": true, "startTime": 0.5, "duration": 2,
                    "captionText": "RISE UP NOW"{reveal},
                    "captionStyle": {{"backgroundColorHex": "FF0000",
                                      "backgroundOpacity": 1.0, "fontSize": 18{reveal_style}}},
                    "keyframes": []}}"#,
                reveal = "",
                reveal_style = reveal,
            ))
        };
        let out = OwnedIoSurface::new_bgra(512, 128).unwrap();
        let ink = |meta: ProjectMetadata, time: f64| -> usize {
            let (mut engine, _state) = make_engine(meta, vec![], 64 << 20);
            engine.render(time, out.raw(), 512, 128).expect("render");
            out.read_pixels()
                .unwrap()
                .chunks_exact(4)
                .filter(|p| p[2] > 60)
                .count()
        };
        let rise = |t: f64| ink(fixture(r#", "reveal": {"by": "word", "mode": "rise"}"#), t);
        assert_eq!(rise(0.5), 0, "at the first frame nothing has arrived");
        assert!(rise(1.2) > 0, "and the words duly arrive");
        // The no-reveal caption still draws whole from its first frame —
        // the fallback arm is for it, not for an active reveal.
        assert!(ink(fixture(""), 0.5) > 0, "a plain caption is simply there");
    }

    /// The grade fixture: a solid plate whose colour makes every adjustment
    /// legible in numbers. Layer-level constants or keyframed ramps via
    /// `extra`.
    fn grade_fixture(plate: &str, extra: &str) -> ProjectMetadata {
        blur_project(&format!(
            r#"{{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0E01", "name":"graded",
                "sortIndex": 1, "kind":"caption",
                "isEnabled": true, "startTime": 0, "duration": 2,
                "captionText": "GRADE"{extra},
                "captionStyle": {{"backgroundColorHex": "{plate}",
                                  "backgroundOpacity": 1.0, "fontSize": 18}},
                "keyframes": []}}"#
        ))
    }

    fn centre_of_band(px: &[u8]) -> (u8, u8, u8) {
        centre_of_band_rows(px, 0, 128)
    }

    fn centre_of_band_rows(px: &[u8], y0: usize, y1: usize) -> (u8, u8, u8) {
        // The plate's centre row and column: the widest lit band.
        let rows: Vec<usize> = (y0..y1)
            .filter(|y| {
                (0..512).any(|x| {
                    let o = (y * 512 + x) * 4;
                    px[o] > 30 || px[o + 1] > 30 || px[o + 2] > 30
                })
            })
            .collect();
        let y = rows[rows.len() / 2];
        let cols: Vec<usize> = (0..512)
            .filter(|x| {
                let o = (y * 512 + x) * 4;
                px[o] > 30 || px[o + 1] > 30 || px[o + 2] > 30
            })
            .collect();
        // A tenth in from the band's left edge: inside the plate's padding,
        // clear of the white glyphs at its centre.
        let x = cols[cols.len() / 10];
        let o = (y * 512 + x) * 4;
        (px[o + 2], px[o + 1], px[o]) // r, g, b from BGRA
    }

    /// Saturation zero turns the layer's own pixels grey — and ONLY its
    /// own: an ungraded neighbour in the same frame keeps its colour.
    #[test]
    fn a_grade_desaturates_its_own_layer_and_nobody_else() {
        let meta = blur_project(&format!(
            "{},{}",
            // Red plate, graded to grey.
            r#"{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0E02", "name":"grey",
                "sortIndex": 1, "kind":"caption",
                "isEnabled": true, "startTime": 0, "duration": 2,
                "captionText": "GRADED",
                "adjustments": {"saturation": 0},
                "captionStyle": {"backgroundColorHex": "FF0000",
                                 "backgroundOpacity": 1.0, "fontSize": 18},
                "keyframes": [
                  {"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0E03","time":0,
                   "horizontalShift":-120,"verticalShift":0,"transitionDuration":0}]}"#,
            // Blue plate, untouched.
            r#"{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0E04", "name":"blue",
                "sortIndex": 2, "kind":"caption",
                "isEnabled": true, "startTime": 0, "duration": 2,
                "captionText": "PLAIN",
                "captionStyle": {"backgroundColorHex": "0000FF",
                                 "backgroundOpacity": 1.0, "fontSize": 18},
                "keyframes": [
                  {"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0E05","time":0,
                   "horizontalShift":-120,"verticalShift":64,"transitionDuration":0}]}"#,
        ));
        let out = OwnedIoSurface::new_bgra(512, 128).unwrap();
        let (mut engine, _state) = make_engine(meta, vec![], 64 << 20);
        engine.render(1.0, out.raw(), 512, 128).expect("render");
        let px = out.read_pixels().unwrap();

        // Top band: the graded plate. Its red must have collapsed to luma.
        let (r, g, b) = centre_of_band_rows(&px, 0, 60);
        assert!(
            r.abs_diff(g) <= 2 && g.abs_diff(b) <= 2,
            "grey means r=g=b, got ({r},{g},{b})"
        );
        assert!(r < 120, "and far from full red, got {r}");
        // Bottom band: the neighbour keeps its blue.
        let (nr, _ng, nb) = centre_of_band_rows(&px, 60, 128);
        assert!(
            nb > 150 && nr < 60,
            "the ungraded neighbour stays blue, got r={nr} b={nb}"
        );
    }

    /// A tint is a gel: at full amount a white plate takes the tint's own
    /// colour, saturation-first so grade-plus-tint reads as a duotone.
    #[test]
    fn a_full_tint_gels_the_layer() {
        let meta = grade_fixture(
            "FFFFFF",
            r#", "adjustments": {"tintHex": "FF8000", "tintAmount": 1.0}"#,
        );
        let out = OwnedIoSurface::new_bgra(512, 128).unwrap();
        let (mut engine, _state) = make_engine(meta, vec![], 64 << 20);
        engine.render(1.0, out.raw(), 512, 128).expect("render");
        let (r, g, b) = centre_of_band(&out.read_pixels().unwrap());
        assert!(r > 230, "the gel passes its red, got {r}");
        assert!((100..=160).contains(&g), "half-passes green, got {g}");
        assert!(b < 30, "and blocks blue, got {b}");
    }

    /// The grade RAMPS: keyframed saturation walks a red plate to grey
    /// across the layer, and the keyframed track beats the layer constant.
    #[test]
    fn a_keyframed_grade_ramps_and_beats_the_constant() {
        let meta = blur_project(
            r#"{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0E08", "name":"ramped",
                "sortIndex": 1, "kind":"caption",
                "isEnabled": true, "startTime": 0, "duration": 2,
                "captionText": "GRADE",
                "adjustments": {"saturation": 1.0},
                "captionStyle": {"backgroundColorHex": "FF0000",
                                 "backgroundOpacity": 1.0, "fontSize": 18},
                "keyframes": [
                  {"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0E06","time":0,
                   "saturation":1,"transitionDuration":0},
                  {"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0E07","time":2,
                   "saturation":0,"transitionDuration":2}]}"#,
        );
        let out = OwnedIoSurface::new_bgra(512, 128).unwrap();
        let (mut engine, _state) = make_engine(meta, vec![], 64 << 20);
        let mut green_at = |t: f64| {
            engine.render(t, out.raw(), 512, 128).expect("render");
            centre_of_band(&out.read_pixels().unwrap()).1
        };
        let start = green_at(0.05);
        let late = green_at(1.9);
        assert!(start < 15, "fully saturated red has no green, got {start}");
        assert!(
            late > start + 25,
            "desaturating pulls green toward luma: {start} then {late}"
        );
    }

    /// The three blend modes against a red ground: screen drops a black
    /// plate out entirely, multiply lets the ground through a white plate,
    /// add pushes past both — and normal, the control, just covers.
    #[test]
    fn blend_modes_combine_with_what_is_beneath() {
        let fixture = |plate: &str, blend: &str| {
            blur_project(&format!(
                r#"{{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0F01", "name":"over",
                    "sortIndex": 1, "kind":"caption",
                    "isEnabled": true, "startTime": 0, "duration": 2,
                    "captionText": "BLEND"{blend},
                    "captionStyle": {{"backgroundColorHex": "{plate}",
                                     "backgroundOpacity": 1.0, "fontSize": 18}},
                    "keyframes": []}}"#
            ))
        };
        let out = OwnedIoSurface::new_bgra(512, 128).unwrap();
        let frame = |plate: &str, blend: &str| {
            let mut meta = fixture(plate, blend);
            // A red ground beneath everything, so "what is beneath" is a
            // known number.
            meta.composition_settings.background_color_hex = "B00000".into();
            let (mut engine, _state) = make_engine(meta, vec![], 64 << 20);
            engine.render(1.0, out.raw(), 512, 128).expect("render");
            out.read_pixels().unwrap()
        };
        // The plate's geometry is identical in every variant, so find it
        // ONCE — as the dark region of the normal-black control — and probe
        // that same spot everywhere. (A screen-blended black plate matches
        // the ground exactly; that sameness is the assertion, so it cannot
        // also be the finder.)
        let control = frame("000000", "");
        let dark: Vec<(usize, usize)> = (0..128)
            .flat_map(|y| (0..512).map(move |x| (x, y)))
            .filter(|(x, y)| {
                let o = (y * 512 + x) * 4;
                control[o] < 40 && control[o + 1] < 40 && control[o + 2] < 40
            })
            .collect();
        assert!(dark.len() > 200, "the control's black plate is findable");
        let (px_x, px_y) = dark[dark.len() / 10];
        let probe = |px: &[u8]| {
            let o = (px_y * 512 + px_x) * 4;
            (px[o + 2], px[o + 1], px[o])
        };
        let plate_px = |plate: &str, blend: &str| probe(&frame(plate, blend));

        // Screen with a BLACK plate: black drops out, the ground shows.
        let (r, g, _b) = plate_px("000000", r#", "blendMode": "screen""#);
        assert!(
            r > 140 && g < 40,
            "screen lets the red ground through black, got r={r} g={g}"
        );
        // The control: a normal black plate covers the ground.
        let (nr, _, _) = probe(&control);
        assert!(nr < 40, "normal black covers, got r={nr}");

        // Multiply with a WHITE plate: white drops out, the ground shows.
        let (mr, mg, _b) = plate_px("FFFFFF", r#", "blendMode": "multiply""#);
        assert!(
            mr > 140 && mg < 40,
            "multiply lets red through white, got r={mr} g={mg}"
        );
        // Control: normal white covers.
        let (wr, wg, _) = plate_px("FFFFFF", "");
        assert!(
            wr > 200 && wg > 200,
            "normal white covers, got r={wr} g={wg}"
        );

        // Add with a dim grey plate over the red ground: brighter than
        // either alone, in every channel the sources carry.
        let (ar, ag, _b) = plate_px("303030", r#", "blendMode": "add""#);
        assert!(
            ar > 190,
            "add sums the ground's red and the plate, got {ar}"
        );
        assert!(
            (30..=90).contains(&ag),
            "and the plate's own grey rides along, got {ag}"
        );
    }

    /// A mask project: a full-canvas red image layer over a blue ground,
    /// windowed by an oval drawing. The oval is inscribed in the square
    /// canvas, so the centre is ink and the corners are not.
    fn mask_meta(mask: &str) -> ProjectMetadata {
        ProjectMetadata::from_json(&format!(
            r#"{{
            "id": "AAAAAAAA-0000-0000-0000-00000000MA5C",
            "name": "mask", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 0, "videoDuration": 0,
            "subtitles": [], "minReaderVersion": 13,
            "compositionSettings": {{
                "canvasWidth": 128, "canvasHeight": 128,
                "backgroundColorHex": "0000FF"
            }},
            "resources": [
                {{"id": "IMGR", "kind": "image", "filename": "a.png",
                  "displayName": "a", "addedAt": 0}},
                {{"id": "IMGR2", "kind": "image", "filename": "b.png",
                  "displayName": "b", "addedAt": 0}},
                {{"id": "MASK", "kind": "drawing", "filename": "m.json",
                  "displayName": "Oval", "addedAt": 0,
                  "drawing": {{"shapes": [{{
                      "id": "S", "kind": "oval",
                      "points": [[0.0, 0.0], [100.0, 100.0]],
                      "strokeColorHex": "FFFFFF", "strokeWidth": 1.0,
                      "fillColorHex": "FFFFFF",
                      "arrowStart": false, "arrowEnd": false}}]}}}}
            ],
            "layers": [
                {{"id": "IMG", "name": "Shot", "sortIndex": 1, "kind": "image",
                  "isEnabled": true, "startTime": 0, "duration": 4,
                  "resourceID": "IMGR"{mask}, "keyframes": []}}
            ]}}"#
        ))
        .expect("fixture")
    }

    /// The porthole: where the mask drawing has ink the layer shows, where
    /// it has none the GROUND shows — the layer's rect still covers the
    /// whole canvas, which is what the unmasked control proves.
    #[test]
    fn a_mask_windows_a_media_layer() {
        let render = |mask: &str| {
            let meta = mask_meta(mask);
            let (mut engine, _state) =
                make_engine(meta, vec![("IMG".into(), [0, 0, 255, 255], 32)], 64 << 20);
            let out = OwnedIoSurface::new_bgra(128, 128).unwrap();
            engine.render(1.0, out.raw(), 128, 128).expect("render");
            out.read_pixels().unwrap()
        };
        let at = |px: &[u8], x: usize, y: usize| -> [u8; 4] {
            let i = (y * 128 + x) * 4;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };

        // Control: without a mask the red layer covers the corner too.
        let control = render("");
        assert_eq!(at(&control, 64, 64), [0, 0, 255, 255], "control centre red");
        assert_eq!(at(&control, 6, 6), [0, 0, 255, 255], "control corner red");

        // Masked: the oval's centre keeps the layer, the corner loses it.
        let masked = render(r#", "maskResourceID": "MASK""#);
        assert_eq!(
            at(&masked, 64, 64),
            [0, 0, 255, 255],
            "ink: the layer shows"
        );
        assert_eq!(
            at(&masked, 6, 6),
            [255, 0, 0, 255],
            "no ink: the ground shows"
        );

        // Inverted: the ink is the hole now.
        let inverted = render(r#", "maskResourceID": "MASK", "maskInverted": true"#);
        assert_eq!(
            at(&inverted, 64, 64),
            [255, 0, 0, 255],
            "inverted: the hole"
        );
        assert_eq!(
            at(&inverted, 6, 6),
            [0, 0, 255, 255],
            "inverted: layer outside the ink"
        );
    }

    /// `mask_meta`'s oval with its fill opacity overridden — the smallest
    /// edit that changes the mask's ALPHA while leaving every point alone.
    fn mask_meta_with_fill_opacity(opacity: f32) -> ProjectMetadata {
        let mut meta = mask_meta(r#", "maskResourceID": "MASK""#);
        let resources = meta.resources.as_mut().expect("resources");
        let mask = resources.iter_mut().find(|r| r.id == "MASK").expect("MASK");
        mask.drawing.as_mut().expect("drawing").shapes[0].fill_opacity = Some(opacity);
        meta
    }

    /// An edit to the mask DRAWING must reach the canvas even when it leaves
    /// the drawing's content bounds — and so the raster's pixel size — exactly
    /// where they were. `mask:{resource}:{w}x{h}` was the whole key, so
    /// turning the oval's fill transparent hit the FILLED raster from before
    /// the edit and the porthole never changed. macOS hid it (the resource
    /// editor tears the engine down); on iOS the editor is a sheet over a
    /// live engine, which is this feature's whole edit-and-check loop.
    #[test]
    fn editing_the_mask_drawing_re_rasterizes_it() {
        let meta = mask_meta(r#", "maskResourceID": "MASK""#);
        let (mut engine, _state) = make_engine(
            meta.clone(),
            vec![("IMG".into(), [0, 0, 255, 255], 32)],
            64 << 20,
        );
        let out = OwnedIoSurface::new_bgra(128, 128).unwrap();

        engine.render(1.0, out.raw(), 128, 128).expect("render");
        assert_eq!(
            pixel(&out, 64, 64),
            [0, 0, 255, 255],
            "filled oval: the layer shows"
        );
        let before = engine.stats().misses;

        // The ONLY edit is the fill's opacity: not one point moves, so the
        // content bounds — and the raster's pw×ph — are bit-identical. That
        // is exactly the case the unstamped key could not tell apart. Note
        // nothing evicts here either: no layer changed, and the masked
        // layer's own resource (IMGR) is untouched.
        engine.set_project(mask_meta_with_fill_opacity(0.0));
        engine.render(1.0, out.raw(), 128, 128).expect("render");
        assert_eq!(
            pixel(&out, 64, 64),
            [255, 0, 0, 255],
            "fill turned transparent: no ink, the ground shows"
        );
        assert_eq!(pixel(&out, 6, 6), [255, 0, 0, 255], "corner never had ink");
        assert!(
            engine.stats().misses > before,
            "the edited mask re-rasterized instead of hitting the old key"
        );

        // And back: the stamp returns to what it was, so does the porthole.
        engine.set_project(meta.clone());
        engine.render(1.0, out.raw(), 128, 128).expect("render");
        assert_eq!(pixel(&out, 64, 64), [0, 0, 255, 255], "fill restored");
    }

    /// The window FLIES: maskOffsetX keyframes carry the oval across the
    /// rect while the layer itself never moves. At each end the probes sit
    /// where the window is — and where it has left, the ground shows,
    /// including beyond the flown box.
    #[test]
    fn a_mask_placement_keyframe_flies_the_window() {
        let mut meta = mask_meta(r#", "maskResourceID": "MASK""#);
        let mut layers = meta.layers.clone().unwrap_or_default();
        layers[0].keyframes = serde_json::from_value(serde_json::json!([
            {"id": "K1", "time": 0.0, "maskOffsetX": -40.0, "transitionDuration": 0},
            {"id": "K2", "time": 2.0, "maskOffsetX": 40.0, "transitionDuration": 2.0}
        ]))
        .unwrap();
        meta.layers = Some(layers);
        let (mut engine, _state) =
            make_engine(meta, vec![("IMG".into(), [0, 0, 255, 255], 32)], 64 << 20);
        let out = OwnedIoSurface::new_bgra(128, 128).unwrap();

        engine.render(0.0, out.raw(), 128, 128).expect("render");
        assert_eq!(pixel(&out, 22, 64), [0, 0, 255, 255], "window flown left");
        assert_eq!(pixel(&out, 106, 64), [255, 0, 0, 255], "right is beyond it");

        engine.render(2.0, out.raw(), 128, 128).expect("render");
        assert_eq!(pixel(&out, 106, 64), [0, 0, 255, 255], "window flown right");
        assert_eq!(pixel(&out, 22, 64), [255, 0, 0, 255], "left is beyond it");
    }

    /// A round mask stays round on a layer of any proportion.
    ///
    /// The mask used to be stretched corner-to-corner over the layer's rect,
    /// so a circle came out an oval on anything but a square layer and the
    /// only cure was to draw the distortion in reverse. The mask carries its
    /// own proportions now, fitted and centred, and `maskZoomY` is the ONLY
    /// way to stretch it.
    #[test]
    fn a_round_mask_stays_round_on_an_oblong_layer() {
        // The canvas is 2:1, and the layer fills it — so a stretched mask
        // would be twice as wide as it is tall.
        let mut meta = mask_meta(r#", "maskResourceID": "MASK""#);
        meta.composition_settings.canvas_width = 256.0;
        meta.composition_settings.canvas_height = 128.0;
        // The layer must lay out OBLONG or a stretch is uniform and proves
        // nothing: a viewport over half the source's height makes the rect
        // 2:1, and a stretched circle would come out 2:1 with it.
        let mut layers = meta.layers.clone().unwrap_or_default();
        layers[0].keyframes = serde_json::from_value(serde_json::json!([
            {"id": "VP", "time": 0, "viewport": [0.0, 0.0, 1.0, 0.5],
             "transitionDuration": 0}
        ]))
        .unwrap();
        meta.layers = Some(layers);
        let (mut engine, _state) =
            make_engine(meta, vec![("IMG".into(), [0, 0, 255, 255], 32)], 64 << 20);
        let out = OwnedIoSurface::new_bgra(256, 128).unwrap();
        engine.render(1.0, out.raw(), 256, 128).expect("render");
        let px = out.read_pixels().unwrap();

        // The ink's extent, measured directly off the frame.
        let lit = |x: usize, y: usize| -> bool {
            let o = (y * 256 + x) * 4;
            // BGRA: the layer's frame is [0,0,255,255], the ground is blue.
            px[o] < 80 && px[o + 2] > 100
        };
        let mut min_x = 256usize;
        let mut max_x = 0usize;
        let mut min_y = 128usize;
        let mut max_y = 0usize;
        for y in 0..128 {
            for x in 0..256 {
                if lit(x, y) {
                    min_x = min_x.min(x);
                    max_x = max_x.max(x);
                    min_y = min_y.min(y);
                    max_y = max_y.max(y);
                }
            }
        }
        assert!(max_x > min_x, "the mask drew something");
        let width = (max_x - min_x + 1) as f64;
        let height = (max_y - min_y + 1) as f64;
        assert!(
            (width / height - 1.0).abs() < 0.06,
            "a round mask must stay round: got {width}x{height} on a 2:1 layer"
        );
        // And it is fitted, not shrunk to nothing: it fills the short axis.
        assert!(height > 100.0, "fitted to the rect's height, got {height}");
    }

    /// Mid-swap, BOTH materials are on screen — and both stay inside the
    /// window. The outgoing quad is a separate attach site in the walk, so
    /// a corner probe at the swap's midpoint is what catches it going
    /// unmasked.
    #[test]
    fn a_swap_happens_inside_the_mask_window() {
        let mut meta = mask_meta(r#", "maskResourceID": "MASK""#);
        let mut layers = meta.layers.clone().unwrap_or_default();
        layers[0].keyframes = serde_json::from_value(serde_json::json!([
            {"id": "K", "time": 1.0, "resourceID": "IMGR2", "transitionDuration": 0,
             "transition": {"kind": "wipe", "from": "left", "duration": 1.0}}
        ]))
        .unwrap();
        meta.layers = Some(layers);
        let (mut engine, _state) =
            make_engine(meta, vec![("IMG".into(), [0, 0, 255, 255], 32)], 64 << 20);
        let out = OwnedIoSurface::new_bgra(128, 128).unwrap();
        engine.render(1.5, out.raw(), 128, 128).expect("render");
        assert_eq!(
            pixel(&out, 64, 64),
            [0, 0, 255, 255],
            "the swap shows inside"
        );
        assert_eq!(
            pixel(&out, 6, 6),
            [255, 0, 0, 255],
            "and the corner stays ground"
        );
    }

    /// A caption layer can have its WORDS replaced by a keyframe, exactly as
    /// an image layer has its picture replaced — and until now the renderer
    /// read the layer's own resource and showed the first caption forever.
    /// With a transition, the outgoing words are still there while the new
    /// ones arrive.
    #[test]
    fn a_caption_swap_replaces_the_words_and_can_transition() {
        let json = r#"{
            "id": "AAAAAAAA-0000-0000-0000-00000000000B",
            "name": "words", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 0, "videoDuration": 0,
            "subtitles": [], "minReaderVersion": 9,
            "compositionSettings": {
                "canvasWidth": 256, "canvasHeight": 128,
                "backgroundColorHex": "000000",
                "subtitleFontSize": 40, "subtitleVerticalMargin": 30,
                "subtitleLeftMargin": 10, "subtitleRightMargin": 10
            },
            "resources": [
                {"id": "ONE", "kind": "caption", "filename": "", "displayName": "one",
                 "addedAt": 0, "captionText": "FIRST",
                 "captionStyle": {"textColorHex": "FF0000"}},
                {"id": "TWO", "kind": "caption", "filename": "", "displayName": "two",
                 "addedAt": 0, "captionText": "SECOND",
                 "captionStyle": {"textColorHex": "0000FF"}}
            ],
            "layers": [
                {"id": "CAP", "name": "words", "sortIndex": 0, "kind": "caption",
                 "isEnabled": true, "startTime": 0, "duration": 10,
                 "resourceID": "ONE",
                 "keyframes": [
                   {"id": "K1", "time": 4, "transitionDuration": 0, "resourceID": "TWO",
                    "transition": {"kind": "wipe", "from": "left", "duration": 2}}
                 ]}
            ]}"#;
        let meta = ProjectMetadata::from_json(json).expect("caption swap fixture");
        let (mut engine, _state) = make_engine(meta, vec![], 64 << 20);
        let out = OwnedIoSurface::new_bgra(256, 128).unwrap();
        let mut counts = |time: f64| -> (usize, usize) {
            engine.render(time, out.raw(), 256, 128).expect("render");
            let px = out.read_pixels().unwrap();
            // BGRA: the first caption is red, the second blue.
            let red = px
                .chunks_exact(4)
                .filter(|p| p[2] > 100 && p[0] < 80)
                .count();
            let blue = px
                .chunks_exact(4)
                .filter(|p| p[0] > 100 && p[2] < 80)
                .count();
            (red, blue)
        };

        let (before_red, before_blue) = counts(1.0);
        assert!(
            before_red > 20,
            "the first caption renders ({before_red} px)"
        );
        assert_eq!(before_blue, 0, "and only it");

        // Half way through the wipe: both sets of words on screen.
        let (mid_red, mid_blue) = counts(5.0);
        assert!(
            mid_red > 0,
            "the outgoing words are still there ({mid_red})"
        );
        assert!(mid_blue > 0, "while the new ones arrive ({mid_blue})");

        let (after_red, after_blue) = counts(8.0);
        assert!(
            after_blue > 20,
            "the second caption renders ({after_blue} px)"
        );
        assert_eq!(
            after_red, 0,
            "and the first is gone — the swap was ignored before"
        );
    }

    #[test]
    fn tier_switch_keys_cache_separately() {
        let meta = tests_support::fixture_meta(64.0);
        let (mut engine, state) =
            make_engine(meta, vec![("VID".into(), [0, 255, 0, 255], 32)], 64 << 20);
        let out = OwnedIoSurface::new_bgra(64, 64).unwrap();

        engine.set_preferred_tier(1);
        engine.render(3.0, out.raw(), 64, 64).unwrap();
        engine.set_preferred_tier(0);
        engine.render(3.0, out.raw(), 64, 64).unwrap();
        // Same time, different tiers: two provider calls, tiers 1 then 0.
        assert_eq!(engine.stats().misses, 2);
        // Back to tier 1: cache hit, no new provider call.
        engine.set_preferred_tier(1);
        engine.render(3.0, out.raw(), 64, 64).unwrap();
        assert_eq!(engine.stats().misses, 2);
        assert_eq!(engine.stats().hits, 1);
        assert_eq!(state.lock().unwrap().requests.len(), 2);
    }

    /// Raster scale (export density for captions): the same caption rendered
    /// at scale 1 and scale 2 must land on the same canvas pixels — the
    /// texture gets denser, the quad must not move or resize. A scale bug
    /// (e.g. forgetting to divide the quad rect back down) doubles the
    /// caption's size and blows the mean diff far past this gate.
    /// Every caption look field resolves the same way: what this caption
    /// says, then what the composition says. Padding, corner radius and
    /// alignment used to skip the first step (composition only) or the
    /// second (caption only), which is why it was never clear which screen
    /// owned a value.
    /// A wipe has to actually reveal a FRACTION of the layer — the geometry
    /// is unit-tested in promo-timeline, but only a render says the quad the
    /// engine builds carries it through the border, the corner radius and the
    /// texture patch loop.
    #[test]
    fn a_wipe_reveals_the_layer_a_piece_at_a_time() {
        let json = r#"{
            "id": "AAAAAAAA-0000-0000-0000-000000000009",
            "name": "wipe", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 0, "videoDuration": 0,
            "subtitles": [],
            "compositionSettings": {
                "canvasWidth": 256, "canvasHeight": 128,
                "backgroundColorHex": "000000",
                "subtitleFontSize": 40, "subtitleColorHex": "FFFFFF",
                "subtitleVerticalMargin": 30,
                "subtitleLeftMargin": 10, "subtitleRightMargin": 10
            },
            "layers": [
                {"id": "CAP", "name": "words", "sortIndex": 0, "kind": "caption",
                 "isEnabled": true, "startTime": 0, "duration": 10,
                 "captionText": "WIDE WIDE WIDE", "keyframes": [],
                 "transitionIn": {"kind": "wipe", "from": "left", "duration": 2}}
            ]}"#;
        let meta = ProjectMetadata::from_json(json).expect("wipe fixture");
        let (mut engine, _state) = make_engine(meta, vec![], 64 << 20);
        let out = OwnedIoSurface::new_bgra(256, 128).unwrap();
        let mut lit = |time: f64| -> usize {
            engine.render(time, out.raw(), 256, 128).expect("render");
            out.read_pixels()
                .unwrap()
                .chunks_exact(4)
                .filter(|p| p[1] > 64)
                .count()
        };

        let whole = lit(5.0);
        assert!(
            whole > 200,
            "the caption must render at all ({whole} lit px)"
        );
        assert_eq!(lit(0.0), 0, "nothing is revealed at the very start");

        let half = lit(1.0);
        let ratio = half as f64 / whole as f64;
        assert!(
            (0.3..=0.7).contains(&ratio),
            "half way through a wipe should show about half: {half} of {whole}"
        );

        // And it grows: a quarter in shows less than half.
        assert!(lit(0.5) < half, "{} at 25% vs {half} at 50%", lit(0.5));
    }

    #[test]
    fn caption_raster_scale_densifies_without_moving_the_quad() {
        let json = r#"{
            "id": "AAAAAAAA-0000-0000-0000-000000000003",
            "name": "cap", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 0, "videoDuration": 0,
            "subtitles": [],
            "compositionSettings": {
                "canvasWidth": 256, "canvasHeight": 128,
                "backgroundColorHex": "000000",
                "subtitleFontSize": 22,
                "subtitleColorHex": "FFFFFF",
                "subtitleVerticalMargin": 20,
                "subtitleLeftMargin": 10,
                "subtitleRightMargin": 10,
                "subtitleStrokeColorHex": "FF0000",
                "subtitleStrokeWidth": 5,
                "subtitleShadowColorHex": "0000FF",
                "subtitleShadowOpacity": 0.9,
                "subtitleShadowRadius": 9
            },
            "layers": [
                {"id": "CAP", "name": "words", "sortIndex": 0, "kind": "caption",
                 "isEnabled": true, "startTime": 0, "duration": 10,
                 "captionText": "Scaled words", "keyframes": []}
            ]}"#;
        let meta = ProjectMetadata::from_json(json).expect("caption fixture");
        let (mut engine, _state) = make_engine(meta, vec![], 64 << 20);
        let out = OwnedIoSurface::new_bgra(256, 128).unwrap();

        let mut render = |scale: f64| -> Vec<u8> {
            engine.set_raster_scale(scale);
            engine.render(1.0, out.raw(), 256, 128).expect("render");
            out.read_pixels().unwrap()
        };
        let at_1x = render(1.0);
        let at_2x = render(2.0);

        // BGRA: the stroke is pure red, the shadow pure blue.
        let count = |px: &[u8], f: fn(&[u8]) -> bool| px.chunks_exact(4).filter(|p| f(p)).count();
        let red = |p: &[u8]| p[2] > 80 && p[1] < 60 && p[0] < 60;
        let blue = |p: &[u8]| p[0] > 40 && p[1] < 60 && p[2] < 60;
        // The outline and the shadow are lengths too. Rasterizing denser
        // without scaling them drew a 5px outline at 1135 lit pixels instead
        // of 1857, and halved the shadow — a 4K export of a 1080p canvas
        // quietly lost the very effects that keep a caption readable.
        for (name, f, floor) in [
            ("outline", red as fn(&[u8]) -> bool, 500usize),
            ("shadow", blue as fn(&[u8]) -> bool, 400usize),
        ] {
            let (one, two) = (count(&at_1x, f), count(&at_2x, f));
            assert!(one > floor, "{name} did not render at 1x ({one} px)");
            let ratio = one.max(two) as f64 / one.min(two).max(1) as f64;
            assert!(ratio < 1.25, "{name}: {one} px at 1x vs {two} px at 2x");
        }
        let ink = |px: &[u8]| px.chunks_exact(4).filter(|p| p[1] > 64).count();
        let ink_1x = ink(&at_1x);
        let ink_2x = ink(&at_2x);
        assert!(
            ink_1x > 50,
            "caption must actually render ({ink_1x} lit px)"
        );
        // Same placement and size: ink counts within 25% of each other …
        let ratio = ink_1x.max(ink_2x) as f64 / ink_1x.min(ink_2x).max(1) as f64;
        assert!(
            ratio < 1.25,
            "ink {ink_1x} vs {ink_2x}: quad moved or resized"
        );
        // … and the frames differ only at glyph-edge level.
        let mean = at_1x
            .iter()
            .zip(&at_2x)
            .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs() as f64)
            .sum::<f64>()
            / at_1x.len() as f64;
        assert!(mean < 4.0, "mean diff {mean} — scaled caption drifted");
    }

    /// Export mode: the clock is monotonic, so per-time (video) frames must
    /// not enter the LRU cache — they would only evict the static content
    /// that IS reused. Rendering still works, and turning the mode off
    /// restores caching.
    #[test]
    fn export_mode_keeps_per_time_frames_out_of_the_cache() {
        let meta = tests_support::fixture_meta(64.0);
        let (mut engine, state) =
            make_engine(meta, vec![("VID".into(), [255, 0, 0, 255], 32)], 64 << 20);
        engine.set_export_mode(true);
        let out = OwnedIoSurface::new_bgra(64, 64).unwrap();

        // A monotonic export clock: every frame decodes, nothing is cached.
        for i in 0..4 {
            engine
                .render(3.0 + f64::from(i) * 0.1, out.raw(), 64, 64)
                .unwrap();
            // The frame still composes: the video quad is present.
            assert_eq!(pixel(&out, 30, 30), [255, 0, 0, 255], "video frame");
        }
        let stats = engine.stats();
        assert_eq!(stats.misses, 4, "every export frame decodes once");
        assert_eq!(stats.cached_bytes, 0, "per-time frames must not be cached");
        assert_eq!(state.lock().unwrap().requests.len(), 4);

        // Same time twice IN export mode: decoded twice (no cache), by design.
        engine.render(5.0, out.raw(), 64, 64).unwrap();
        engine.render(5.0, out.raw(), 64, 64).unwrap();
        assert_eq!(engine.stats().misses, 6);

        // Off again: preview behaviour returns (second render is a hit).
        engine.set_export_mode(false);
        engine.render(6.0, out.raw(), 64, 64).unwrap();
        engine.render(6.0, out.raw(), 64, 64).unwrap();
        let stats = engine.stats();
        assert_eq!(stats.misses, 7);
        assert!(stats.hits >= 1, "cache works again outside export mode");
        assert_eq!(stats.cached_bytes, 32 * 32 * 4);
    }

    #[test]
    fn governor_evicts_under_budget() {
        let meta = tests_support::fixture_meta(64.0);
        // Budget = two 32×32 frames.
        let (mut engine, _state) = make_engine(
            meta,
            vec![("VID".into(), [0, 255, 0, 255], 32)],
            2 * 32 * 32 * 4,
        );
        let out = OwnedIoSurface::new_bgra(64, 64).unwrap();
        for i in 0..5 {
            engine
                .render(3.0 + f64::from(i) * 0.5, out.raw(), 64, 64)
                .unwrap();
        }
        let stats = engine.stats();
        assert_eq!(stats.misses, 5);
        assert_eq!(stats.evictions, 3);
        assert!(stats.cached_bytes <= 2 * 32 * 32 * 4);

        // Re-rendering the newest time is still a hit; the oldest re-decodes.
        engine.render(5.0, out.raw(), 64, 64).unwrap();
        assert_eq!(engine.stats().hits, 1);
        engine.render(3.0, out.raw(), 64, 64).unwrap();
        assert_eq!(engine.stats().misses, 6);
    }
}

/// The portable suite: the same composition, driven by a host that has no
/// platform surfaces at all — plain BGRA pixels in, a wgpu texture out.
///
/// This is the R1 gate. It runs on every target, so it is what proves the
/// preview engine is genuinely portable rather than merely compiling once the
/// Apple parts are configured out. It renders the SAME fixture as the
/// IOSurface suite and asserts the SAME pixels, so the two paths cannot drift.
#[cfg(test)]
mod portable_tests {
    use super::tests_support;
    use super::*;
    use std::ffi::CStr;
    use std::sync::Mutex;

    struct CpuProviderState {
        /// (layer id, BGRA fill, square size in px)
        colors: Vec<(String, [u8; 4], usize)>,
        /// Pixel buffers are reused between calls on purpose: the contract
        /// says the engine copies during the call, and this is what would
        /// catch it if it ever stopped.
        scratch: Vec<u8>,
        requests: Vec<(String, f64)>,
    }

    extern "C" fn cpu_provider(
        user: *mut c_void,
        layer_id: *const c_char,
        resource_id: *const c_char,
        source_time: f64,
        _tier: i32,
        out_surface: *mut HostSurface,
        out_flags: *mut i32,
    ) -> i32 {
        let state = unsafe { &*(user as *const Mutex<CpuProviderState>) };
        let mut state = state.lock().unwrap();
        let id = unsafe { CStr::from_ptr(layer_id) }
            .to_string_lossy()
            .to_string();
        let resource = unsafe { CStr::from_ptr(resource_id) }
            .to_string_lossy()
            .to_string();
        state.requests.push((id.clone(), source_time));
        // Keyed by RESOURCE first so a swap test can tell two sources apart,
        // falling back to the layer for fixtures that name neither.
        let Some((_, color, size)) = state
            .colors
            .iter()
            .find(|(l, _, _)| *l == resource)
            .or_else(|| state.colors.iter().find(|(l, _, _)| *l == id))
            .cloned()
        else {
            return 1;
        };
        // Size 0 is the sprite fixture: a 4x1 sheet of 16px cells in red,
        // green, blue and white, so a test can tell which cell was drawn.
        let (width, height) = if size == 0 { (64, 16) } else { (size, size) };
        if size == 0 {
            let cells: [[u8; 4]; 4] = [
                [0, 0, 255, 255],
                [0, 255, 0, 255],
                [255, 0, 0, 255],
                [255, 255, 255, 255],
            ];
            state.scratch = Vec::with_capacity(width * height * 4);
            for _ in 0..height {
                for cell in cells {
                    for _ in 0..16 {
                        state.scratch.extend_from_slice(&cell);
                    }
                }
            }
        } else {
            state.scratch = color.repeat(size * size);
        }
        unsafe {
            *out_surface = HostSurface {
                kind: SURFACE_CPU_PIXELS,
                data: state.scratch.as_ptr(),
                width: width as u32,
                height: height as u32,
                bytes_per_row: (width * 4) as u32,
                ..Default::default()
            };
            *out_flags = 0;
        }
        0
    }

    fn make_cpu_engine(
        meta: ProjectMetadata,
        colors: Vec<(String, [u8; 4], usize)>,
    ) -> (PreviewEngine, Box<Mutex<CpuProviderState>>) {
        let state = Box::new(Mutex::new(CpuProviderState {
            colors,
            scratch: Vec::new(),
            requests: Vec::new(),
        }));
        let user = &*state as *const Mutex<CpuProviderState> as *mut c_void;
        let engine = PreviewEngine::new(meta, cpu_provider, user, 64 << 20).expect("engine");
        (engine, state)
    }

    /// Renders to a texture and reads it back as BGRA rows.
    fn render_and_read(engine: &mut PreviewEngine, time: f64, size: u32) -> Vec<u8> {
        let ctx = GpuContext::shared().expect("gpu");
        let texture = ctx
            .device
            .create_texture(&promo_gpu::wgpu::TextureDescriptor {
                label: Some("portable-test-target"),
                size: promo_gpu::wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: promo_gpu::wgpu::TextureDimension::D2,
                format: promo_gpu::wgpu::TextureFormat::Bgra8Unorm,
                usage: promo_gpu::wgpu::TextureUsages::RENDER_ATTACHMENT
                    | promo_gpu::wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
        engine
            .render_to_texture(time, &texture, size, size)
            .expect("render");
        read_texture_bgra(ctx, &texture, size, size)
    }

    fn read_texture_bgra(
        ctx: &GpuContext,
        texture: &promo_gpu::wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Vec<u8> {
        use promo_gpu::wgpu;
        // wgpu requires 256-byte-aligned rows for a texture->buffer copy.
        let unpadded = width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;
        let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("portable-test-readback"),
            size: (padded * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        ctx.queue.submit(Some(encoder.finish()));
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        ctx.device.poll(wgpu::Maintain::Wait);
        let mapped = slice.get_mapped_range();
        // Drop the row padding so callers index by width.
        let mut out = Vec::with_capacity((unpadded * height) as usize);
        for row in 0..height as usize {
            let start = row * padded as usize;
            out.extend_from_slice(&mapped[start..start + unpadded as usize]);
        }
        drop(mapped);
        buffer.unmap();
        out
    }

    fn pixel_at(px: &[u8], width: usize, x: usize, y: usize) -> [u8; 4] {
        let i = (y * width + x) * 4;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    }

    /// The same assertions as `tests::renders_background_and_video_layer`,
    /// through the portable path. If these two ever disagree, one of the
    /// import arms is wrong.
    #[test]
    fn cpu_pixels_render_the_same_composition() {
        let Some(_) = GpuContext::shared() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let meta = tests_support::fixture_meta(64.0);
        let (mut engine, state) = make_cpu_engine(meta, vec![("VID".into(), [255, 0, 0, 255], 32)]);
        let px = render_and_read(&mut engine, 3.0, 64);

        assert_eq!(pixel_at(&px, 64, 2, 2), [0, 51, 0, 255], "background");
        assert_eq!(pixel_at(&px, 64, 30, 30), [255, 0, 0, 255], "video frame");

        // Same trim mapping as the Apple suite: t=3 -> local 1 -> source 2.
        let requests = &state.lock().unwrap().requests;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, "VID");
        assert!((requests[0].1 - 2.0).abs() < 1e-9, "got {}", requests[0].1);
    }

    /// A sprite sheet arrives as ONE image and animates by showing a
    /// different part of it — while the layer goes on moving.
    ///
    /// That independence is the whole design: the frame is chosen when
    /// SAMPLING and the movement happens in the geometry, so neither has to
    /// know about the other. This asserts both at once, because either one
    /// working alone would be a broken feature.
    #[test]
    fn a_sprite_animates_while_the_layer_moves() {
        let Some(_) = GpuContext::shared() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        // A 4x1 sheet of 16px cells: red, green, blue, white. At 1fps the
        // frame index IS the second. The layer slides 32px right over 2s.
        let json = r#"{
            "id": "AAAAAAAA-0000-0000-0000-000000000001",
            "name": "sprite", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
            "compositionSettings": {"canvasWidth": 64, "canvasHeight": 64,
              "backgroundColorHex": "003300"},
            "layers": [
                {"id": "BG", "name": "bg", "sortIndex": 0, "kind": "background",
                 "isEnabled": true, "startTime": 0, "keyframes": []},
                {"id": "SPR", "name": "hero", "sortIndex": 1, "kind": "image",
                 "isEnabled": true, "startTime": 0, "duration": 10,
                 "resourceID": "AAAAAAAA-0000-0000-0000-0000000000S1",
                 "keyframes": [
                   {"id": "K0", "time": 0, "zoom": 0.25,
                    "horizontalShift": 0, "verticalShift": 0,
                    "transitionDuration": 0},
                   {"id": "K1", "time": 2, "zoom": 0.25,
                    "horizontalShift": 32, "verticalShift": 0,
                    "transitionPercent": 100, "transitionDuration": 2}]}
            ],
            "resources": [
                {"id": "AAAAAAAA-0000-0000-0000-0000000000S1", "kind": "image",
                 "filename": "hero.png", "displayName": "hero", "addedAt": 0,
                 "imageCuts": [], "disabledAudioTrackIndices": [],
                 "sampling": "nearest",
                 "sprite": {"columns": 4, "rows": 1, "fps": 1}}
            ]}"#;
        let meta = ProjectMetadata::from_json(json).expect("fixture");
        let (mut engine, _state) = make_cpu_engine(meta, vec![("SPR".into(), [0, 0, 0, 0], 0)]);

        // At t=0 the cell is red and the layer sits at the left edge.
        let start = render_and_read(&mut engine, 0.0, 64);
        assert_eq!(
            pixel_at(&start, 64, 8, 8),
            [0, 0, 255, 255],
            "frame 0 is red"
        );
        assert_eq!(
            pixel_at(&start, 64, 40, 8),
            [0, 51, 0, 255],
            "nothing there yet"
        );

        // At t=2 it has moved 32px right AND advanced to the third frame.
        let later = render_and_read(&mut engine, 2.0, 64);
        assert_eq!(
            pixel_at(&later, 64, 40, 8),
            [255, 0, 0, 255],
            "frame 2 is blue"
        );
        assert_eq!(pixel_at(&later, 64, 8, 8), [0, 51, 0, 255], "left behind");
    }

    /// A keyframe swaps what the layer shows, and the layer goes on moving
    /// through it.
    ///
    /// The interesting half is the CACHE. Frames were keyed by layer and
    /// source time, and an image asks at a fixed source time — so before the
    /// key carried the resource, every frame after the swap was answered out
    /// of cache with the bitmap from before it, and the picture never changed.
    /// The transition that matters most: between two pieces of MATERIAL.
    ///
    /// A swap was a hard cut because one layer drew one quad — "there is no
    /// halfway between two images, and dissolving needs both drawn at once,
    /// which one layer cannot do". During a swap transition it draws both, so
    /// this asserts the two are on screen TOGETHER, each on its own side of
    /// the wipe.
    #[test]
    fn a_swap_with_a_transition_shows_both_resources_at_once() {
        let Some(_) = GpuContext::shared() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let json = r#"{
            "id": "AAAAAAAA-0000-0000-0000-00000000000A",
            "name": "swap", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
            "minReaderVersion": 9,
            "compositionSettings": {"canvasWidth": 64, "canvasHeight": 64,
              "backgroundColorHex": "003300"},
            "layers": [
                {"id": "IMG", "name": "img", "sortIndex": 1, "kind": "image",
                 "isEnabled": true, "startTime": 0, "duration": 10,
                 "resourceID": "RED",
                 "keyframes": [
                   {"id": "K0", "time": 0, "zoom": 1,
                    "horizontalShift": 0, "verticalShift": 0,
                    "transitionDuration": 0},
                   {"id": "K1", "time": 2, "resourceID": "BLUE",
                    "transitionDuration": 0,
                    "transition": {"kind": "wipe", "from": "left", "duration": 2}}]}
            ],
            "resources": [
                {"id": "RED", "kind": "image", "filename": "r.png",
                 "displayName": "r", "addedAt": 0, "imageCuts": [],
                 "disabledAudioTrackIndices": []},
                {"id": "BLUE", "kind": "image", "filename": "b.png",
                 "displayName": "b", "addedAt": 0, "imageCuts": [],
                 "disabledAudioTrackIndices": []}
            ]}"#;
        let meta = ProjectMetadata::from_json(json).expect("fixture");
        let (mut engine, _state) = make_cpu_engine(
            meta,
            vec![
                ("RED".into(), [0, 0, 255, 255], 32),
                ("BLUE".into(), [255, 0, 0, 255], 32),
            ],
        );

        let before = render_and_read(&mut engine, 1.0, 64);
        assert_eq!(
            pixel_at(&before, 64, 8, 32),
            [0, 0, 255, 255],
            "red before the swap"
        );
        assert_eq!(
            pixel_at(&before, 64, 56, 32),
            [0, 0, 255, 255],
            "on both sides"
        );

        // Half way through a wipe from the left: the incoming image holds the
        // left of the frame, the outgoing one is still there on the right.
        // This is the assertion a cut could never satisfy.
        let mid = render_and_read(&mut engine, 3.0, 64);
        assert_eq!(
            pixel_at(&mid, 64, 8, 32),
            [255, 0, 0, 255],
            "blue arriving on the left"
        );
        assert_eq!(
            pixel_at(&mid, 64, 56, 32),
            [0, 0, 255, 255],
            "red still leaving on the right"
        );

        let after = render_and_read(&mut engine, 5.0, 64);
        assert_eq!(
            pixel_at(&after, 64, 8, 32),
            [255, 0, 0, 255],
            "blue once it is done"
        );
        assert_eq!(pixel_at(&after, 64, 56, 32), [255, 0, 0, 255], "everywhere");
    }

    #[test]
    fn a_keyframe_swaps_the_resource_and_the_cache_notices() {
        let Some(_) = GpuContext::shared() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let json = r#"{
            "id": "AAAAAAAA-0000-0000-0000-000000000001",
            "name": "swap", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
            "compositionSettings": {"canvasWidth": 64, "canvasHeight": 64,
              "backgroundColorHex": "003300"},
            "layers": [
                {"id": "IMG", "name": "img", "sortIndex": 1, "kind": "image",
                 "isEnabled": true, "startTime": 0, "duration": 10,
                 "resourceID": "RED",
                 "keyframes": [
                   {"id": "K0", "time": 0, "zoom": 0.5,
                    "horizontalShift": 0, "verticalShift": 0,
                    "transitionDuration": 0},
                   {"id": "K1", "time": 2, "resourceID": "BLUE",
                    "transitionDuration": 0},
                   {"id": "K2", "time": 4, "zoom": 0.5,
                    "horizontalShift": 24, "verticalShift": 0,
                    "transitionPercent": 100, "transitionDuration": 4}]}
            ],
            "resources": [
                {"id": "RED", "kind": "image", "filename": "r.png",
                 "displayName": "r", "addedAt": 0, "imageCuts": [],
                 "disabledAudioTrackIndices": []},
                {"id": "BLUE", "kind": "image", "filename": "b.png",
                 "displayName": "b", "addedAt": 0, "imageCuts": [],
                 "disabledAudioTrackIndices": []}
            ]}"#;
        let meta = ProjectMetadata::from_json(json).expect("fixture");
        let (mut engine, _state) = make_cpu_engine(
            meta,
            vec![
                ("RED".into(), [0, 0, 255, 255], 32),
                ("BLUE".into(), [255, 0, 0, 255], 32),
            ],
        );

        // Before the swap: red, at the left.
        let before = render_and_read(&mut engine, 1.0, 64);
        assert_eq!(pixel_at(&before, 64, 8, 8), [0, 0, 255, 255], "red first");

        // After it: blue — and this is the assertion the cache used to fail,
        // since nothing about the request changed except the resource.
        // Sampled at x=30 because the layer is mid-glide by now and has left
        // x=8 behind; the swap and the movement are independent, which is
        // exactly what the next assertions check.
        let after = render_and_read(&mut engine, 3.0, 64);
        assert_eq!(
            pixel_at(&after, 64, 30, 8),
            [255, 0, 0, 255],
            "swapped to blue"
        );

        // And the layer kept moving through the swap: by t=4 it has slid
        // right, still showing the new resource.
        let moved = render_and_read(&mut engine, 4.0, 64);
        assert_eq!(
            pixel_at(&moved, 64, 40, 8),
            [255, 0, 0, 255],
            "moved, still blue"
        );
        assert_eq!(
            pixel_at(&moved, 64, 8, 8),
            [0, 51, 0, 255],
            "left where it was"
        );
    }

    /// A viewport windows the source and RAMPS — the pan travels through the
    /// picture while the layer's own rect on the canvas never moves.
    ///
    /// The fixture bitmap is the 4-cell strip (red, green, blue, white, 16px
    /// each) used as a PLAIN image, so the window's position is legible in
    /// colour: x=0 red, x=0.25 exactly green, x=0.5 blue.
    #[test]
    fn a_viewport_windows_the_source_and_ramps() {
        let Some(_) = GpuContext::shared() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let json = r#"{
            "id": "AAAAAAAA-0000-0000-0000-000000000001",
            "name": "viewport", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
            "compositionSettings": {"canvasWidth": 64, "canvasHeight": 64,
              "backgroundColorHex": "003300"},
            "layers": [
                {"id": "BG", "name": "bg", "sortIndex": 0, "kind": "background",
                 "isEnabled": true, "startTime": 0, "keyframes": []},
                {"id": "IMG", "name": "img", "sortIndex": 1, "kind": "image",
                 "isEnabled": true, "startTime": 0, "duration": 10,
                 "resourceID": "STRIP",
                 "keyframes": [
                   {"id": "K0", "time": 0, "zoom": 0.25,
                    "horizontalShift": 0, "verticalShift": 0,
                    "transitionDuration": 0,
                    "viewport": [0, 0, 0.25, 1]},
                   {"id": "K1", "time": 2,
                    "transitionPercent": 100, "transitionDuration": 2,
                    "viewport": [0.5, 0, 0.25, 1]}]}
            ],
            "resources": [
                {"id": "STRIP", "kind": "image", "filename": "strip.png",
                 "displayName": "strip", "addedAt": 0, "imageCuts": [],
                 "disabledAudioTrackIndices": []}
            ]}"#;
        let meta = ProjectMetadata::from_json(json).expect("fixture");
        let (mut engine, _state) = make_cpu_engine(meta, vec![("IMG".into(), [0, 0, 0, 0], 0)]);

        // The window is 16x16 source pixels, so at zoom 0.25 the layer draws
        // 16x16 at the origin — NOT the 64px-wide strip the whole image
        // would lay out as. The pixel past the window proves the rect
        // followed the window, not the source.
        let start = render_and_read(&mut engine, 0.0, 64);
        assert_eq!(
            pixel_at(&start, 64, 8, 8),
            [0, 0, 255, 255],
            "window on red"
        );
        assert_eq!(
            pixel_at(&start, 64, 40, 8),
            [0, 51, 0, 255],
            "frame is 16px, not 64"
        );

        // Halfway through the ramp the window has slid to x=0.25 — exactly
        // the green cell. This is the pan being a RAMP, not a step.
        let mid = render_and_read(&mut engine, 1.0, 64);
        assert_eq!(
            pixel_at(&mid, 64, 8, 8),
            [0, 255, 0, 255],
            "mid-ramp on green"
        );

        let end = render_and_read(&mut engine, 2.0, 64);
        assert_eq!(pixel_at(&end, 64, 8, 8), [255, 0, 0, 255], "landed on blue");
    }

    /// A viewport windows the sprite CELL, not the sheet.
    ///
    /// The composition order is the assertion: at t=0 the cell is red, and a
    /// window into that cell is still red everywhere. If the viewport
    /// replaced the cell's uv instead of composing with it, [0.5,0.5,0.5,0.5]
    /// would address the sheet itself — the blue/white bottom-right — and
    /// both sampled pixels would change colour. (Verified: with the compose
    /// deliberately broken to an assignment, this fails drawing blue.)
    #[test]
    fn a_viewport_windows_the_sprite_cell_not_the_sheet() {
        let Some(_) = GpuContext::shared() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let json = r#"{
            "id": "AAAAAAAA-0000-0000-0000-000000000001",
            "name": "spritevp", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
            "compositionSettings": {"canvasWidth": 64, "canvasHeight": 64,
              "backgroundColorHex": "003300"},
            "layers": [
                {"id": "BG", "name": "bg", "sortIndex": 0, "kind": "background",
                 "isEnabled": true, "startTime": 0, "keyframes": []},
                {"id": "SPR", "name": "hero", "sortIndex": 1, "kind": "image",
                 "isEnabled": true, "startTime": 0, "duration": 10,
                 "resourceID": "SHEET",
                 "keyframes": [
                   {"id": "K0", "time": 0, "zoom": 0.25,
                    "horizontalShift": 0, "verticalShift": 0,
                    "transitionDuration": 0,
                    "viewport": [0.5, 0.5, 0.5, 0.5]}]}
            ],
            "resources": [
                {"id": "SHEET", "kind": "image", "filename": "hero.png",
                 "displayName": "hero", "addedAt": 0, "imageCuts": [],
                 "disabledAudioTrackIndices": [],
                 "sampling": "nearest",
                 "sprite": {"columns": 4, "rows": 1, "fps": 1}}
            ]}"#;
        let meta = ProjectMetadata::from_json(json).expect("fixture");
        let (mut engine, _state) = make_cpu_engine(meta, vec![("SPR".into(), [0, 0, 0, 0], 0)]);

        // Cell 0 is red; a window inside it is red at both ends.
        let t0 = render_and_read(&mut engine, 0.0, 64);
        assert_eq!(
            pixel_at(&t0, 64, 4, 8),
            [0, 0, 255, 255],
            "inside cell 0: red"
        );
        assert_eq!(
            pixel_at(&t0, 64, 12, 8),
            [0, 0, 255, 255],
            "still red at the right"
        );

        // And the sprite goes on animating with the window applied.
        let t1 = render_and_read(&mut engine, 1.0, 64);
        assert_eq!(pixel_at(&t1, 64, 4, 8), [0, 255, 0, 255], "cell 1: green");
    }

    /// A magnifying viewport escalates a video layer to tier 0.
    ///
    /// The host picks its tier from canvas scale and knows nothing about
    /// per-layer windows — at 2x the user would otherwise be aiming a
    /// precision rectangle at 540p proxy blur blown up 2x.
    #[test]
    fn viewport_magnification_escalates_the_tier() {
        let Some(_) = GpuContext::shared() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let json = r#"{
            "id": "AAAAAAAA-0000-0000-0000-000000000001",
            "name": "tiers", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
            "compositionSettings": {"canvasWidth": 64, "canvasHeight": 64,
              "backgroundColorHex": "003300"},
            "layers": [
                {"id": "ZOOMED", "name": "z", "sortIndex": 0, "kind": "video",
                 "isEnabled": true, "startTime": 0, "duration": 10,
                 "keyframes": [
                   {"id": "K0", "time": 0, "transitionDuration": 0,
                    "viewport": [0.25, 0.25, 0.5, 0.5]}]},
                {"id": "PLAIN", "name": "p", "sortIndex": 1, "kind": "video",
                 "isEnabled": true, "startTime": 0, "duration": 10,
                 "keyframes": []},
                {"id": "WIDE", "name": "w", "sortIndex": 2, "kind": "video",
                 "isEnabled": true, "startTime": 0, "duration": 10,
                 "keyframes": [
                   {"id": "K0", "time": 0, "transitionDuration": 0,
                    "viewport": [0, 0, 1, 0.8]}]}
            ],
            "resources": []}"#;
        let meta = ProjectMetadata::from_json(json).expect("fixture");
        let (mut engine, _state) = make_cpu_engine(meta, vec![]);
        let layers = engine.meta.layers.clone().unwrap_or_default();
        let by_id = |id: &str| layers.iter().find(|l| l.id == id).unwrap();

        engine.set_preferred_tier(1);
        assert_eq!(
            engine.tier_for(by_id("ZOOMED"), 0.0),
            0,
            "2x window: full res"
        );
        assert_eq!(
            engine.tier_for(by_id("PLAIN"), 0.0),
            1,
            "no window: host tier"
        );
        assert_eq!(
            engine.tier_for(by_id("WIDE"), 0.0),
            1,
            "1.25x: proxy still fine"
        );

        // Tier 0 is already the best there is; a window changes nothing.
        engine.set_preferred_tier(0);
        assert_eq!(engine.tier_for(by_id("ZOOMED"), 0.0), 0);
    }

    /// Every colour field may name a palette entry instead of a value —
    /// and the proof has to be a rendered PIXEL, because resolution happens
    /// deep in the scene build where a unit test on the model cannot see.
    #[test]
    fn palette_names_resolve_to_colours_on_screen() {
        let Some(_) = GpuContext::shared() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let json = r#"{
            "id": "AAAAAAAA-0000-0000-0000-000000000001",
            "name": "palette", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
            "compositionSettings": {"canvasWidth": 64, "canvasHeight": 64,
              "backgroundColorHex": "@ink",
              "palette": [{"name": "ink", "colorHex": "FF0000"},
                          {"name": "accent", "colorHex": "00FF00"}]},
            "layers": [
                {"id": "BG", "name": "bg", "sortIndex": 0, "kind": "background",
                 "isEnabled": true, "startTime": 0, "keyframes": []}
            ],
            "resources": []}"#;
        let meta = ProjectMetadata::from_json(json).expect("fixture");
        let (mut engine, _state) = make_cpu_engine(meta, vec![]);
        let px = render_and_read(&mut engine, 0.0, 64);
        // BGRA: @ink is FF0000, so blue=0, green=0, red=255.
        assert_eq!(pixel_at(&px, 64, 32, 32), [0, 0, 255, 255], "@ink drew red");

        // An unknown name must NOT become a colour nobody chose. It falls
        // through to the site's own fallback rather than being guessed at.
        let unknown = json.replace("\"@ink\"", "\"@missing\"");
        let meta = ProjectMetadata::from_json(&unknown).expect("fixture");
        let (mut engine, _state) = make_cpu_engine(meta, vec![]);
        let px = render_and_read(&mut engine, 0.0, 64);
        assert_ne!(
            pixel_at(&px, 64, 32, 32),
            [0, 0, 255, 255],
            "an unknown name must not silently keep the old colour"
        );
    }

    /// A padded stride is what a real decoder hands over; the import repacks.
    #[test]
    fn padded_rows_are_repacked() {
        let Some(ctx) = GpuContext::shared() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        // 2x2 image, rows padded to 12 bytes: [BGRA BGRA | 4 bytes junk]
        let mut data = Vec::new();
        for _ in 0..2 {
            data.extend_from_slice(&[10, 20, 30, 255, 40, 50, 60, 255]);
            data.extend_from_slice(&[99, 99, 99, 99]);
        }
        let surface = GpuSurface::CpuPixels {
            data,
            width: 2,
            height: 2,
            bytes_per_row: 12,
        };
        let frame = Compositor::import(ctx, &surface).expect("import");
        assert_eq!((frame.width, frame.height), (2, 2));
        assert_eq!(frame.byte_size(), 16);
    }

    /// The variants that exist for capability negotiation but have no import
    /// yet must say so rather than silently rendering nothing.
    #[test]
    fn unimplemented_surface_kinds_are_reported() {
        let Some(ctx) = GpuContext::shared() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        assert!(Compositor::import(ctx, &GpuSurface::DmaBuf { fd: 3 }).is_err());
        assert!(Compositor::import(
            ctx,
            &GpuSurface::D3DSharedHandle {
                raw: std::ptr::dangling_mut::<c_void>()
            }
        )
        .is_err());
    }

    /// An empty or malformed descriptor skips the layer instead of reading
    /// whatever the pointer happens to address.
    #[test]
    fn empty_descriptor_yields_no_surface() {
        assert!(unsafe { HostSurface::default().to_gpu_surface() }.is_none());
        assert!(unsafe {
            HostSurface {
                kind: SURFACE_CPU_PIXELS,
                width: 4,
                height: 4,
                ..Default::default()
            }
            .to_gpu_surface()
        }
        .is_none());
        assert!(unsafe {
            HostSurface {
                kind: SURFACE_IOSURFACE,
                ..Default::default()
            }
            .to_gpu_surface()
        }
        .is_none());
    }
}
