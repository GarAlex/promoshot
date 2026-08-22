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
use promo_gpu::{GpuSurface, ImportedFrame};
// Only the Apple-typed render entries name this; the provider no longer does.
#[cfg(any(target_os = "macos", target_os = "ios"))]
use promo_gpu::iosurface::IOSurfaceRef;
use promo_gpu::{GpuContext, GpuError};
use promo_model::{ProjectLayer, ProjectLayerKind, ProjectMetadata, ProjectResource, Size};
use crate::vector::vector_shapes;
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
            key_of: HashMap::new(),
            id_of: HashMap::new(),
            next_id: 1,
            hits: 0,
            misses: 0,
            preferred_tier: 0,
            export_mode: false,
            scratch: HashMap::new(),
            raster_scale: 1.0,
            vector: None,
            blur_sample: None,
            retain_scratch: false,
            builds: 0,
            scratch_key: HashMap::new(),
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
        self.meta = meta;
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
                id == layer_id
                    || id.starts_with(&media_prefix)
                    || id.starts_with(&caption_prefix)
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
            return None;
        }
        // SAFETY: the provider contract requires a CPU_PIXELS descriptor to
        // address bytes_per_row * height readable bytes for this call; the
        // conversion copies them before returning.
        let gpu_surface = unsafe { surface.to_gpu_surface() }?;
        // One import entry point: retains and adopts on Apple, uploads
        // elsewhere, and hands back something that owns what it needs.
        let frame = Compositor::import(self.ctx, &gpu_surface).ok()?;
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
        self.id_of.insert(id, key);
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
            let resource_id = tl::layer_resource_id(
                layer, time, self.meta.resources.as_deref().unwrap_or(&[]))
                .unwrap_or_default()
                .to_string();
            let _ = self.frame(&layer.id, &resource_id, source_time, self.tier_for(layer, time), &[]);
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
        self.compositor
            .accumulate_resolve_to_iosurface(self.ctx, output, output_width, output_height)
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
        let scenes = self.build_scenes(time, output_width, output_height)?;
        let count = scenes.len();
        for (index, (scene, used)) in scenes.iter().enumerate() {
            // Field-disjoint lookup, as in `render_with_overlay`.
            let textures: Vec<&InputTexture> = used
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
    ) -> Option<(SceneQuad, u64)> {
        let Some(frame_id) = self.frame(&layer.id, showing, source_time, tier, &used) else {
            return None;
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
            let Some(cell) =
                tl::sprite_frame_at(sheet, layer, local, Size::new(fw, fh))
            else {
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
        if let Some(vp) = tl::layer_viewport(layer, time) {
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
            layer, time, &settings, self.meta.resources.as_deref().unwrap_or(&[]));
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
                &settings,
                tr.zoom,
                canvas.width(),
            );
            quad.corner_radius = style.corner_radius;
            quad.border_width = style.border_width;
            quad.border_rgba = style.border_rgba;
        }
        Some((quad, frame_id))
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
        let layout =
            promo_text::reveal_layout(&text, canvas.width(), canvas.height(), &style, by);
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
            return Some((caption_scene_quad(x, y, w, h), id));
        }

        // Rasterize at `scale`× density: everything the layout reads scales
        // together, so the quad below lands at the same canvas-space spot —
        // the texture is just denser.
        let dense = style.scaled_lengths(scale);
        let raster =
            promo_text::rasterize(text, canvas.width() * scale, canvas.height() * scale, &dense)?;
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
        Some((
            caption_scene_quad(
                raster.x / scale,
                raster.y / scale,
                raster.width as f64 / scale,
                raster.height as f64 / scale,
            ),
            id,
        ))
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
        let shapes = vector_shapes(doc, &settings);
        if shapes.is_empty() {
            return None;
        }
        let (_, _, bw, bh) = promo_gpu::vector::content_bounds(&shapes);
        // Path-aware: a keyframe carrying a motionPath bends the route to
        // it, and resolving that needs the drawing resource.
        let tr = tl::layer_transform_along_paths(
            layer, time, settings, self.meta.resources.as_deref().unwrap_or(&[]));
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

        let key = (format!("drawing:{}:{}x{}", layer.id, pw, ph), 0i64, 0i32);
        if let Some(&id) = self.key_of.get(&key) {
            self.governor.touch(id);
            self.hits += 1;
            return Some((drawing_scene_quad(&rect), id));
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
        Some((drawing_scene_quad(&rect), id))
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
        // The PREVIOUS render's transient frames die here, not at its end: a
        // deferred-fence compose may still have the GPU sampling them after
        // render returns, and wgpu keeps submitted resources alive only once
        // they are submitted — which the previous render has done by now.
        // A motion-blur walk is the exception: its sub-builds are one
        // render, and the frames its earlier composes reference must live
        // until the walk resolves.
        self.builds += 1;
        if !self.retain_scratch {
            self.scratch.clear();
            self.scratch_key.clear();
        }
        let settings = self.meta.composition_settings.clone();
        let canvas = Size::new(settings.canvas_width, settings.canvas_height);

        let mut layers: Vec<ProjectLayer> = self.meta.layers.clone().unwrap_or_default();
        layers.sort_by_key(|l| l.sort_index);

        // Background color: the first visible background layer's keyframed
        // color, else the settings color (same rule as the stills path).
        let bg_hex = layers
            .iter()
            .find(|l| l.kind == ProjectLayerKind::Background && tl::layer_is_visible(l, time))
            .map(|l| tl::layer_background_color_hex(l, time, &settings))
            .unwrap_or_else(|| settings.background_color_hex.clone());
        // `@name` becomes a colour here and nowhere else, so every field
        // in the document gains the palette at once.
        let background = rgba_from_hex(settings.resolve_color(&bg_hex));

        // The gradient comes from the same background layer, on the same
        // hold-then-ramp timing as its colour — resolved here and converted
        // from unit canvas coordinates to the canvas pixels the shader works
        // in, so the model never has to know the output size.
        let background_gradient = layers
            .iter()
            .find(|l| l.kind == ProjectLayerKind::Background && tl::layer_is_visible(l, time))
            .and_then(|layer| tl::layer_background_gradient(layer, time, &settings))
            .or_else(|| settings.background_gradient.clone())
            .map(|gradient| {
                let (cw, ch) = (canvas.width() as f32, canvas.height() as f32);
                let point = |p: promo_model::Point| [p.x() as f32 * cw, p.y() as f32 * ch];
                promo_gpu::compositor::SceneGradient {
                    radial: gradient.kind == promo_model::GradientKind::Radial,
                    repeat: match gradient.effective_repeat() {
                        promo_model::GradientRepeat::Clamp => 0,
                        promo_model::GradientRepeat::Repeat => 1,
                        promo_model::GradientRepeat::Mirror => 2,
                    },
                    start: point(gradient.start),
                    end: point(gradient.end),
                    stops: gradient
                        .resolved_stops()
                        .iter()
                        .map(|stop| (rgba_from_hex(settings.resolve_color(&stop.color_hex)),
                                     stop.at as f32))
                        .collect(),
                }
            });

        let mut quads: Vec<SceneQuad> = Vec::new();
        let mut used: Vec<u64> = Vec::new();

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
                Some(sample) if tl::layer_shutter(layer, centre).is_some_and(|s| s > 0.0) => {
                    sample
                }
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
                        layer, swap.previous.as_deref(), &settings, canvas, time, &used)
                    {
                        quad.opacity = tl::layer_opacity(layer, time) as f32;
                        // A push shoves the outgoing material out the far
                        // side; every other kind leaves it where it is and
                        // arrives over it.
                        apply_effect(&mut quad, swap.departing, canvas);
                        apply_transition(&mut quad, layer, time, canvas);
                        quads.push(quad);
                        used.push(id);
                    }
                }
                if let Some((mut quad, id)) = self.caption_quad(
                    layer, showing.as_deref(), &settings, canvas, time, &used)
                {
                    quad.opacity = tl::layer_opacity(layer, time) as f32;
                    if let Some(swap) = swap.as_ref() {
                        apply_effect(&mut quad, swap.effect, canvas);
                    }
                    apply_transition(&mut quad, layer, time, canvas);

                    // A reveal draws the SAME raster as a set of bands —
                    // cropped to what has arrived — rather than a new picture
                    // per frame. Laid out whole, so revealing part of it
                    // cannot move the caption, and a word keeps the place the
                    // layout gave it however it arrives.
                    let rule = self
                        .meta
                        .caption_style_showing(layer, showing.as_deref())
                        .and_then(|style| style.reveal)
                        .or_else(|| settings.subtitle_reveal.clone());
                    let bands = rule.as_ref().and_then(|rule| {
                        let by = tl::reveal::unit_of(rule);
                        let layout = self.caption_reveal(
                            layer, showing.as_deref(), &settings, canvas, time, by)?;
                        let progress = tl::reveal::progress(rule, layer, time, layout.units.len());
                        Some((tl::reveal::bands(&layout, progress, rule), rule.clone()))
                    });

                    match bands {
                        Some((bands, rule)) if !bands.is_empty() => {
                            let tinted = rule.highlight_color_hex.as_deref().and_then(|hex| {
                                let rgba = rgba_bytes(&settings.resolve_color(hex), 1.0);
                                self.caption_quad_colored(
                                    layer, showing.as_deref(), &settings, canvas, time,
                                    Some(rgba), &used)
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
                                apply_effect(&mut piece, band.effect_for(full_height), canvas);
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
                            quads.push(quad);
                            used.push(id);
                        }
                    }
                }
                continue;
            }
            let (_is_media, is_drawing) = match layer.kind {
                ProjectLayerKind::Video | ProjectLayerKind::Image => (true, false),
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
                        layer, swap.previous.as_deref(), &settings, canvas, time, &used)
                    {
                        quad.rotation_deg = tl::layer_rotation(layer, time);
                        quad.opacity = tl::layer_opacity(layer, time) as f32;
                        apply_effect(&mut quad, swap.departing, canvas);
                        apply_transition(&mut quad, layer, time, canvas);
                        quads.push(quad);
                        used.push(id);
                    }
                }
                if let Some((quad, id)) = self.drawing_quad(
                    layer, showing.as_deref(), &settings, canvas, time, &used)
                {
                    let mut quad = quad;
                    quad.rotation_deg = tl::layer_rotation(layer, time);
                    quad.opacity = tl::layer_opacity(layer, time) as f32;
                    if let Some(swap) = swap.as_ref() {
                        apply_effect(&mut quad, swap.effect, canvas);
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
            if let Some(swap) = swap.as_ref() {
                // The outgoing material, whole, underneath — a dissolve or a
                // wipe needs both on screen at once, which is exactly what a
                // swap could not do while a layer drew one quad.
                if let Some(previous) = swap.previous.as_deref() {
                    if let Some((mut quad, id)) = self.media_quad(
                        layer, previous, &settings, canvas, time, source_time, tier,
                        is_drawing, &resources, &used)
                    {
                        apply_effect(&mut quad, swap.departing, canvas);
                        apply_transition(&mut quad, layer, time, canvas);
                        quads.push(quad);
                        used.push(id);
                    }
                }
            }
            let Some((mut quad, frame_id)) = self.media_quad(
                layer, &showing, &settings, canvas, time, source_time, tier,
                is_drawing, &resources, &used)
            else {
                continue;
            };
            used.push(frame_id);
            if let Some(swap) = swap.as_ref() {
                // The incoming material arrives over it.
                apply_effect(&mut quad, swap.effect, canvas);
            }
            // After the border, so a wiped edge cuts the frame too rather
            // than leaving a stroke drawn around nothing.
            apply_transition(&mut quad, layer, time, canvas);
            quads.push(quad);
        }

        // Patch texture indices now that the used-frame list is final; the
        // caller borrows the textures in this same order.
        for (i, quad) in quads.iter_mut().enumerate() {
            quad.texture = Some(i);
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
fn media_border_style(
    frame: Option<&promo_model::ResourceFrame>,
    layer: &ProjectLayer,
    settings: &promo_model::CompositionSettings,
    zoom: f64,
    canvas_width: f64,
) -> MediaBorderStyle {
    let zoom = tl::clamped_zoom(zoom);
    if let Some(frame) = frame {
        if frame.kind == promo_model::ResourceFrameKind::Border {
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
                    .unwrap_or(&settings.video_border_color_hex)),
        ),
    }
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
        let style = media_border_style(
            Some(&border_frame()),
            &layer(),
            &settings,
            1.0,
            1920.0,
        );
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
        let mut device = border_frame();
        device.kind = promo_model::ResourceFrameKind::Device;
        let with_device =
            media_border_style(Some(&device), &layer(), &settings, 1.0, 1920.0);
        // A device frame is pre-baked by the provider; the quad falls back to
        // the settings path, same as no frame at all.
        assert_eq!(with_none, with_device);
        assert!((with_none.corner_radius - settings.video_corner_radius).abs() < 1e-9);
    }

    #[test]
    fn thin_frame_borders_floor_at_one_pixel_before_zoom() {
        let settings = promo_model::CompositionSettings::default();
        let mut frame = border_frame();
        frame.border_width = 0.1; // 0.1 * 540/1080 = 0.05 → floors to 1
        let style = media_border_style(Some(&frame), &layer(), &settings, 2.0, 540.0);
        assert!((style.border_width - 2.0).abs() < 1e-9, "1px floor × zoom 2");
    }
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
        (a.output_width as f64 / a.canvas_width)
            .min(a.output_height as f64 / a.canvas_height)
    } else {
        1.0
    };
    let mut max_d: f64 = 0.0;
    for (qa, qb) in a.quads.iter().zip(&b.quads) {
        let corners = |q: &SceneQuad| -> [[f64; 2]; 4] {
            let [x, y, w, h] = q.rect;
            let (cx, cy) = (x + w / 2.0, y + h / 2.0);
            let (sin, cos) = q.rotation_deg.to_radians().sin_cos();
            let rot = |dx: f64, dy: f64| {
                [cx + dx * cos - dy * sin, cy + dx * sin + dy * cos]
            };
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
    quad.rect = [
        x + w * uv[0],
        y + h * uv[1],
        w * uv[2],
        h * uv[3],
    ];
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
fn apply_transition(quad: &mut SceneQuad, layer: &promo_model::ProjectLayer, time: f64, canvas: Size) {
    apply_effect(quad, tl::transition::effect(layer, time), canvas);
}

/// The geometry half, for an effect that came from somewhere other than the
/// layer's own edges — a resource swap, say.
fn apply_effect(quad: &mut SceneQuad, effect: tl::transition::Effect, canvas: Size) {
    if effect.is_identity() {
        return;
    }
    quad.opacity *= effect.opacity as f32;
    let (rect, uv) = tl::transition::apply(
        &effect,
        quad.rect,
        quad.uv_rect,
        (canvas.width(), canvas.height()),
    );
    quad.rect = rect;
    quad.uv_rect = uv;
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
                .unwrap_or_else(|| settings.subtitle_color_hex.clone())),
        1.0,
    );
    let bg_opacity = style
        .and_then(|s| s.background_opacity)
        .unwrap_or(settings.subtitle_background_opacity);
    let background_rgba = rgba_bytes(
        settings.resolve_color(
            &style
                .and_then(|s| s.background_color_hex.clone())
                .unwrap_or_else(|| settings.subtitle_background_color_hex.clone())),
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
                    .unwrap_or_else(|| settings.subtitle_stroke_color_hex.clone())),
            1.0,
        ),
        stroke_width: get(|s| s.stroke_width, settings.subtitle_stroke_width),
        shadow_rgba: rgba_bytes(
            settings.resolve_color(
                &style
                    .and_then(|s| s.shadow_color_hex.clone())
                    .unwrap_or_else(|| settings.subtitle_shadow_color_hex.clone())),
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
                [0.0, get(|s| s.shadow_radius, settings.subtitle_shadow_radius) / 2.0]
            }),
        padding: get(|s| s.padding, settings.subtitle_background_padding),
        corner_radius: get(|s| s.corner_radius, settings.subtitle_background_corner_radius),
        left_margin: get(|s| s.left_margin, settings.subtitle_left_margin),
        right_margin: get(|s| s.right_margin, settings.subtitle_right_margin),
        vertical_margin: get(|s| s.vertical_margin, settings.subtitle_vertical_margin),
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
            !state.lock().unwrap().requests.iter().any(|r| r.0 == "DRAWL"),
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
        assert!(first > 0, "the first word is there when the caption is ({first} px)");
        assert!(half > first, "more words by the middle: {first} then {half}");
        assert!(whole > half, "and all of them by the end: {half} then {whole}");

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
        assert_eq!(leftmost(&at_start), x_when_revealed,
                   "a caption that re-flows as it types has been laid out per frame");
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
        let mut render = |json: String, time: f64| {
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
            0.0, "FF0000", r#", "motionBlur": {"shutter": 1.0}"#));
        let out = OwnedIoSurface::new_bgra(512, 128).unwrap();
        let mut render = |meta: ProjectMetadata| {
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
        assert!(blue_ramp <= 6, "the sharp layer stays antialiasing-sharp: {blue_ramp}");
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
        let mut render = |meta: ProjectMetadata| {
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
        let (mut engine, _state) =
            make_engine(still(r#", "motionBlur": {"shutter": 1.0}"#), vec![], 64 << 20);
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
        let meta = blur_project(&format!(
            r#"{{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0B01", "name":"mover",
                "sortIndex": 1, "kind":"caption",
                "isEnabled": true, "startTime": 0, "duration": 1,
                "captionText": "MOTION",
                "captionStyle": {{"backgroundColorHex": "FF0000",
                                  "backgroundOpacity": 1.0, "fontSize": 18}},
                "keyframes": [
                  {{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0B02", "time": 0,
                    "horizontalShift": -200, "verticalShift": 0,
                    "shutter": 0, "transitionDuration": 0}},
                  {{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0B03", "time": 1,
                    "horizontalShift": 200, "verticalShift": 0,
                    "shutter": 1, "transitionDuration": 1}}
                ]}}"#
        ));
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
        let mut render = |meta: ProjectMetadata| {
            let (mut engine, _state) = make_engine(meta, vec![], 64 << 20);
            engine.render(0.5, out.raw(), 512, 128).expect("render");
            out.read_pixels().unwrap()
        };
        let sharp = render(mover("", ""));
        let overridden = render(mover(
            r#", "motionBlur": {"shutter": 1.0}"#,
            r#", "shutter": 0"#,
        ));
        assert_eq!(sharp, overridden,
                   "keyframed zero beats the constant, to the bit");
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
        let mut render = |json: String| {
            let meta = ProjectMetadata::from_json(&json).expect("swap fixture");
            let (mut engine, _state) = make_engine(meta, vec![], 64 << 20);
            // Mid-push: both plates are travelling at canvas-width per
            // half-second, the fastest thing this suite draws.
            engine.render(4.25, out.raw(), 256, 128).expect("render");
            out.read_pixels().unwrap()
        };
        let ramp = |px: &[u8], channel: usize| -> usize {
            let row = (0..128)
                .filter(|y| {
                    (0..256).any(|x| px[((y * 256 + x) * 4) + channel] > 200)
                })
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
        let mut ink = |meta: ProjectMetadata, time: f64| -> usize {
            let (mut engine, _state) = make_engine(meta, vec![], 64 << 20);
            engine.render(time, out.raw(), 512, 128).expect("render");
            out.read_pixels()
                .unwrap()
                .chunks_exact(4)
                .filter(|p| p[2] > 60)
                .count()
        };
        let rise = |t: f64| {
            ink(fixture(r#", "reveal": {"by": "word", "mode": "rise"}"#), t)
        };
        assert_eq!(rise(0.5), 0, "at the first frame nothing has arrived");
        assert!(rise(1.2) > 0, "and the words duly arrive");
        // The no-reveal caption still draws whole from its first frame —
        // the fallback arm is for it, not for an active reveal.
        assert!(ink(fixture(""), 0.5) > 0, "a plain caption is simply there");
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
            let red = px.chunks_exact(4).filter(|p| p[2] > 100 && p[0] < 80).count();
            let blue = px.chunks_exact(4).filter(|p| p[0] > 100 && p[2] < 80).count();
            (red, blue)
        };

        let (before_red, before_blue) = counts(1.0);
        assert!(before_red > 20, "the first caption renders ({before_red} px)");
        assert_eq!(before_blue, 0, "and only it");

        // Half way through the wipe: both sets of words on screen.
        let (mid_red, mid_blue) = counts(5.0);
        assert!(mid_red > 0, "the outgoing words are still there ({mid_red})");
        assert!(mid_blue > 0, "while the new ones arrive ({mid_blue})");

        let (after_red, after_blue) = counts(8.0);
        assert!(after_blue > 20, "the second caption renders ({after_blue} px)");
        assert_eq!(after_red, 0, "and the first is gone — the swap was ignored before");
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
        assert!(whole > 200, "the caption must render at all ({whole} lit px)");
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
        for (name, f, floor) in [("outline", red as fn(&[u8]) -> bool, 500usize),
                                 ("shadow", blue as fn(&[u8]) -> bool, 400usize)] {
            let (one, two) = (count(&at_1x, f), count(&at_2x, f));
            assert!(one > floor, "{name} did not render at 1x ({one} px)");
            let ratio = one.max(two) as f64 / one.min(two).max(1) as f64;
            assert!(ratio < 1.25, "{name}: {one} px at 1x vs {two} px at 2x");
        }
        let ink = |px: &[u8]| px.chunks_exact(4).filter(|p| p[1] > 64).count();
        let ink_1x = ink(&at_1x);
        let ink_2x = ink(&at_2x);
        assert!(ink_1x > 50, "caption must actually render ({ink_1x} lit px)");
        // Same placement and size: ink counts within 25% of each other …
        let ratio = ink_1x.max(ink_2x) as f64 / ink_1x.min(ink_2x).max(1) as f64;
        assert!(ratio < 1.25, "ink {ink_1x} vs {ink_2x}: quad moved or resized");
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
        assert_eq!(pixel_at(&start, 64, 8, 8), [0, 0, 255, 255], "frame 0 is red");
        assert_eq!(pixel_at(&start, 64, 40, 8), [0, 51, 0, 255], "nothing there yet");

        // At t=2 it has moved 32px right AND advanced to the third frame.
        let later = render_and_read(&mut engine, 2.0, 64);
        assert_eq!(pixel_at(&later, 64, 40, 8), [255, 0, 0, 255], "frame 2 is blue");
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
        assert_eq!(pixel_at(&before, 64, 8, 32), [0, 0, 255, 255], "red before the swap");
        assert_eq!(pixel_at(&before, 64, 56, 32), [0, 0, 255, 255], "on both sides");

        // Half way through a wipe from the left: the incoming image holds the
        // left of the frame, the outgoing one is still there on the right.
        // This is the assertion a cut could never satisfy.
        let mid = render_and_read(&mut engine, 3.0, 64);
        assert_eq!(pixel_at(&mid, 64, 8, 32), [255, 0, 0, 255], "blue arriving on the left");
        assert_eq!(pixel_at(&mid, 64, 56, 32), [0, 0, 255, 255], "red still leaving on the right");

        let after = render_and_read(&mut engine, 5.0, 64);
        assert_eq!(pixel_at(&after, 64, 8, 32), [255, 0, 0, 255], "blue once it is done");
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
        assert_eq!(pixel_at(&after, 64, 30, 8), [255, 0, 0, 255], "swapped to blue");

        // And the layer kept moving through the swap: by t=4 it has slid
        // right, still showing the new resource.
        let moved = render_and_read(&mut engine, 4.0, 64);
        assert_eq!(pixel_at(&moved, 64, 40, 8), [255, 0, 0, 255], "moved, still blue");
        assert_eq!(pixel_at(&moved, 64, 8, 8), [0, 51, 0, 255], "left where it was");
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
        assert_eq!(pixel_at(&start, 64, 8, 8), [0, 0, 255, 255], "window on red");
        assert_eq!(pixel_at(&start, 64, 40, 8), [0, 51, 0, 255], "frame is 16px, not 64");

        // Halfway through the ramp the window has slid to x=0.25 — exactly
        // the green cell. This is the pan being a RAMP, not a step.
        let mid = render_and_read(&mut engine, 1.0, 64);
        assert_eq!(pixel_at(&mid, 64, 8, 8), [0, 255, 0, 255], "mid-ramp on green");

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
        assert_eq!(pixel_at(&t0, 64, 4, 8), [0, 0, 255, 255], "inside cell 0: red");
        assert_eq!(pixel_at(&t0, 64, 12, 8), [0, 0, 255, 255], "still red at the right");

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
        assert_eq!(engine.tier_for(by_id("ZOOMED"), 0.0), 0, "2x window: full res");
        assert_eq!(engine.tier_for(by_id("PLAIN"), 0.0), 1, "no window: host tier");
        assert_eq!(engine.tier_for(by_id("WIDE"), 0.0), 1, "1.25x: proxy still fine");

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
        assert_ne!(pixel_at(&px, 64, 32, 32), [0, 0, 255, 255],
                   "an unknown name must not silently keep the old colour");
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
