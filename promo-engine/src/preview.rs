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
use promo_timeline as tl;
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CString};

/// Provider out-flag: the bitmap already carries its decorative frame —
/// the engine must not apply corner radius or border over it.
pub const FLAG_PRE_FRAMED: i32 = 1;

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
        })
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

    /// Drops every cached frame belonging to `layer_id` — video/image frames
    /// keyed by the bare id AND caption rasters keyed `caption:{id}:…`, which
    /// a bare-id match silently skipped (stale captions survived every edit).
    fn evict_layer(&mut self, layer_id: &str) {
        let caption_prefix = format!("caption:{layer_id}:");
        let victims: Vec<u64> = self
            .id_of
            .iter()
            .filter(|(_, (id, _, _))| id == layer_id || id.starts_with(&caption_prefix))
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

    /// Fetches (or serves from cache) the frame for `layer` at `source_time`.
    /// `pinned`: ids the in-flight scene already holds — they must survive
    /// this admit's eviction or the scene would point at freed frames.
    fn frame(&mut self, layer_id: &str, source_time: f64, tier: i32, pinned: &[u64]) -> Option<u64> {
        let key = (layer_id.to_string(), quantize(source_time), tier);
        if let Some(&id) = self.key_of.get(&key) {
            self.governor.touch(id);
            self.hits += 1;
            return Some(id);
        }

        let c_id = CString::new(layer_id).ok()?;
        let mut surface = HostSurface::default();
        let mut flags: i32 = 0;
        let rc = (self.provider)(
            self.user,
            c_id.as_ptr(),
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
        for victim in self.governor.admit(id, width * height * 4, pinned) {
            if let Some(k) = self.id_of.remove(&victim) {
                self.key_of.remove(&k);
            }
            self.cache.remove(&victim);
        }
        self.cache.insert(
            id,
            CachedFrame {
                frame,
                flags,
                caption_origin: None,
            },
        );
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
            let _ = self.frame(&layer.id, source_time, self.preferred_tier, &[]);
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
        let mut textures: Vec<&InputTexture> = used
            .iter()
            .map(|id| &self.cache[id].frame.texture)
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
        let textures: Vec<&InputTexture> = used
            .iter()
            .map(|id| &self.cache[id].frame.texture)
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
                "caption:{}:{:x}:{}:{}:{}",
                layer.id,
                content_stamp,
                stamp(style.font_size),
                stamp(style.vertical_margin),
                stamp(style.left_margin)
            ),
            0i64,
            0i32,
        );
        if let Some(&id) = self.key_of.get(&key) {
            self.governor.touch(id);
            self.hits += 1;
            let frame = &self.cache[&id];
            let (w, h) = (frame.frame.width as f64, frame.frame.height as f64);
            let (x, y) = frame.caption_origin?;
            return Some((caption_scene_quad(x, y, w, h), id));
        }

        let raster = promo_text::rasterize(text, canvas.width(), canvas.height(), &style)?;
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
                caption_origin: Some((raster.x, raster.y)),
            },
        );
        self.key_of.insert(key.clone(), id);
        self.id_of.insert(id, key);
        Some((
            caption_scene_quad(
                raster.x,
                raster.y,
                raster.width as f64,
                raster.height as f64,
            ),
            id,
        ))
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
        let background = rgba_from_hex(&bg_hex);

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

            let tier = if layer.kind == ProjectLayerKind::Video {
                self.preferred_tier
            } else {
                0
            };
            let Some(frame_id) = self.frame(&layer.id, source_time, tier, &used) else {
                continue;
            };
            let frame = &self.cache[&frame_id];
            let (fw, fh) = (frame.frame.width as f64, frame.frame.height as f64);
            let pre_framed = frame.flags & FLAG_PRE_FRAMED != 0;
            used.push(frame_id);

            let tr = tl::layer_transform(layer, time, &settings);
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
                border_rgba: rgba_from_hex(&frame.border_color_hex),
            };
        }
    }
    MediaBorderStyle {
        corner_radius: settings.video_corner_radius * zoom,
        border_width: layer
            .image_border_width
            .unwrap_or(settings.video_border_width),
        border_rgba: rgba_from_hex(
            layer
                .image_border_color_hex
                .as_deref()
                .unwrap_or(&settings.video_border_color_hex),
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
fn caption_style(
    style: Option<&promo_model::SubtitleStyle>,
    settings: &promo_model::CompositionSettings,
) -> promo_text::TextStyle {
    let get = |pick: fn(&promo_model::SubtitleStyle) -> Option<f64>, fallback: f64| -> f64 {
        style.and_then(pick).unwrap_or(fallback)
    };
    let text_rgba = rgba_bytes(
        &style
            .and_then(|s| s.text_color_hex.clone())
            .unwrap_or_else(|| settings.subtitle_color_hex.clone()),
        1.0,
    );
    let bg_opacity = style
        .and_then(|s| s.background_opacity)
        .unwrap_or(settings.subtitle_background_opacity);
    let background_rgba = rgba_bytes(
        &style
            .and_then(|s| s.background_color_hex.clone())
            .unwrap_or_else(|| settings.subtitle_background_color_hex.clone()),
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
        align: promo_text::Align::parse(
            style
                .and_then(|s| s.alignment.as_ref())
                .map(|a| a.as_str())
                .unwrap_or("center"),
        ),
        text_rgba,
        background_rgba,
        padding: settings.subtitle_background_padding,
        corner_radius: settings.subtitle_background_corner_radius,
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
        state.requests.push((id.clone(), source_time));
        let Some((_, color, size)) = state.colors.iter().find(|(l, _, _)| *l == id).cloned() else {
            return 1;
        };
        state.scratch = color.repeat(size * size);
        unsafe {
            *out_surface = HostSurface {
                kind: SURFACE_CPU_PIXELS,
                data: state.scratch.as_ptr(),
                width: size as u32,
                height: size as u32,
                bytes_per_row: (size * 4) as u32,
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
