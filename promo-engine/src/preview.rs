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
        if !transient {
            if let Some(&id) = self.key_of.get(&key) {
                self.governor.touch(id);
                self.hits += 1;
                return Some(id);
            }
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
        let (mut scene, used) = self.build_scene(time, output_width, output_height)?;
        let canvas = Size::new(
            self.meta.composition_settings.canvas_width,
            self.meta.composition_settings.canvas_height,
        );
        // Field-disjoint lookup (not the `cached_frame` helper): the closure
        // may only borrow the two maps, because `self.compositor` is borrowed
        // mutably for the compose below.
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

        let overlay_texture;
        if let Some((surface, width, height)) = overlay {
            overlay_texture = self
                .compositor
                .import_iosurface_cached(self.ctx, surface, width, height)?;
            scene.quads.push(SceneQuad {
                texture: Some(textures.len()),
                rect: [0.0, 0.0, canvas.width(), canvas.height()],
                ..Default::default()
            });
            textures.push(&overlay_texture);
        }

        self.compositor
            .compose_to_iosurface_borrowed(self.ctx, &scene, &textures, output)
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
        let (scene, used) = self.build_scene(time, output_width, output_height)?;
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
        self.compositor
            .compose_to_texture_borrowed(self.ctx, &scene, &textures, output)
    }

    /// Rasterizes a caption layer and returns its quad plus the cache id of
    /// the texture.
    ///
    /// Cached under the frame cache like any other texture, keyed by the
    /// layer id with a static source time: caption text does not change with
    /// the playhead, so a 30-second title is shaped once, not 900 times.
    fn caption_quad(
        &mut self,
        layer: &ProjectLayer,
        settings: &promo_model::CompositionSettings,
        canvas: Size,
        time: f64,
        pinned: &[u64],
    ) -> Option<(SceneQuad, u64)> {
        // Resource first, layer second — the app's rule (`captionText(for:)`).
        // Captions authored in the app keep their words and style on the
        // resource; reading only the layer left those invisible here, which is
        // why the host once composited its own copy on top — and animated
        // captions rendered twice.
        let text_owned = self.meta.caption_text_for(layer)?;
        let text = text_owned.trim();
        if text.is_empty() {
            return None;
        }
        let style_source = self.meta.caption_style_for(layer);
        let mut style = caption_style(style_source.as_ref(), settings);
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
        settings: &promo_model::CompositionSettings,
        canvas: Size,
        time: f64,
        pinned: &[u64],
    ) -> Option<(SceneQuad, u64)> {
        let doc = self.resource_for(layer)?.drawing.as_ref()?;
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
        self.scratch.clear();
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
            if layer.kind == ProjectLayerKind::Caption {
                // Text is drawn by the core now (promo-text), not by a host
                // overlay — so a headless render keeps its captions.
                if let Some((mut quad, id)) = self.caption_quad(layer, &settings, canvas, time, &used) {
                    quad.opacity = tl::layer_opacity(layer, time) as f32;
                    quads.push(quad);
                    used.push(id);
                }
                continue;
            }
            let (is_media, is_drawing) = match layer.kind {
                ProjectLayerKind::Video | ProjectLayerKind::Image => (true, false),
                ProjectLayerKind::Drawing => (false, true),
                _ => continue,
            };

            let source_time = if layer.kind == ProjectLayerKind::Video {
                let local = tl::layer_local_time(layer, time);
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
                time
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
                if let Some((quad, id)) = self.drawing_quad(layer, &settings, canvas, time, &used) {
                    let mut quad = quad;
                    quad.rotation_deg = tl::layer_rotation(layer, time);
                    quad.opacity = tl::layer_opacity(layer, time) as f32;
                    quads.push(quad);
                    used.push(id);
                }
                continue;
            }

            let tier = self.tier_for(layer, time);
            // What this layer shows right now — its own resource, unless a
            // keyframe has swapped it.
            let resources = self.meta.resources.clone().unwrap_or_default();
            let showing = tl::layer_resource_id(layer, time, &resources)
                .unwrap_or_default()
                .to_string();
            let Some(frame_id) = self.frame(&layer.id, &showing, source_time, tier, &used) else {
                continue;
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
                    continue;
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
            used.push(frame_id);

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
            if is_media && !pre_framed {
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
    #[test]
    fn a_caption_overrides_the_composition_and_otherwise_inherits_it() {
        use promo_model::{CompositionSettings, SubtitleStyle, SubtitleTextAlignment};
        let mut settings = CompositionSettings::default();
        settings.subtitle_background_padding = 16.0;
        settings.subtitle_background_corner_radius = 8.0;
        settings.subtitle_alignment = SubtitleTextAlignment::Leading;
        settings.subtitle_shadow_radius = 12.0;

        let inherited = caption_style(None, &settings);
        assert_eq!(inherited.padding, 16.0);
        assert_eq!(inherited.corner_radius, 8.0);
        assert_eq!(inherited.align, promo_text::Align::Leading);
        // Unset offset still derives the drop from the blur.
        assert_eq!(inherited.shadow_offset, [0.0, 6.0]);

        let overridden = caption_style(
            Some(&SubtitleStyle {
                padding: Some(40.0),
                corner_radius: Some(2.0),
                alignment: Some(SubtitleTextAlignment::Trailing),
                shadow_offset: Some([5.0, -3.0]),
                ..SubtitleStyle::default()
            }),
            &settings,
        );
        assert_eq!(overridden.padding, 40.0);
        assert_eq!(overridden.corner_radius, 2.0);
        assert_eq!(overridden.align, promo_text::Align::Trailing);
        assert_eq!(overridden.shadow_offset, [5.0, -3.0]);

        // A composition-wide offset applies to a caption that has none.
        settings.subtitle_shadow_offset = Some([0.0, 20.0]);
        assert_eq!(caption_style(None, &settings).shadow_offset, [0.0, 20.0]);
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
