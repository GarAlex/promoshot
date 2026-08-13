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

#![cfg(any(target_os = "macos", target_os = "ios"))]

use crate::governor::MemoryGovernor;
use promo_gpu::compositor::{Compositor, InputTexture, Scene, SceneQuad};
use promo_gpu::iosurface::IOSurfaceRef;
use promo_gpu::{GpuContext, GpuError};
use promo_model::{ProjectLayer, ProjectLayerKind, ProjectMetadata, ProjectResource, Size};
use promo_timeline as tl;
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CString};

extern "C" {
    fn CFRetain(cf: *const c_void) -> *const c_void;
    fn CFRelease(cf: *const c_void);
    fn IOSurfaceGetWidth(buffer: IOSurfaceRef) -> usize;
    fn IOSurfaceGetHeight(buffer: IOSurfaceRef) -> usize;
}

/// Provider out-flag: the bitmap already carries its decorative frame —
/// the engine must not apply corner radius or border over it.
pub const FLAG_PRE_FRAMED: i32 = 1;

/// Host frame provider. Called with the layer id (NUL-terminated), the
/// source-media time in seconds (or a negative value for static content),
/// and the proxy tier (0 = full resolution; higher = smaller proxies —
/// unused in slice 1). On success returns 0 and writes a BGRA IOSurfaceRef
/// (retained by the engine via CFRetain until eviction) and optional flags.
/// Non-zero return = no frame (the layer is skipped this render).
pub type FrameProviderFn = extern "C" fn(
    user: *mut c_void,
    layer_id: *const c_char,
    source_time: f64,
    tier: i32,
    out_surface: *mut IOSurfaceRef,
    out_flags: *mut i32,
) -> i32;

struct CachedFrame {
    surface: IOSurfaceRef,
    texture: InputTexture,
    width: usize,
    height: usize,
    flags: i32,
}

// IOSurface refs are kernel objects; the engine is used from one thread at a
// time through the FFI but the handle itself may move between threads.
unsafe impl Send for CachedFrame {}

impl Drop for CachedFrame {
    fn drop(&mut self) {
        unsafe { CFRelease(self.surface as *const c_void) }
    }
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
        let mut stale: Vec<String> = Vec::new();
        for layer in &old {
            match new.iter().find(|l| l.id == layer.id) {
                Some(updated) if updated == layer => {}
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

    /// Drops every cached frame belonging to `layer_id`.
    fn evict_layer(&mut self, layer_id: &str) {
        let victims: Vec<u64> = self
            .id_of
            .iter()
            .filter(|(_, (id, _, _))| id == layer_id)
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
    fn image_has_animated_tilt(&self, layer: &ProjectLayer) -> bool {
        if layer.kind != ProjectLayerKind::Image || !tl::layer_has_tilt_keyframes(layer) {
            return false;
        }
        let Some(res) = self.resource_for(layer) else {
            return false;
        };
        let frame = layer
            .image_cut_id
            .as_ref()
            .and_then(|cid| res.image_cuts.iter().find(|c| &c.id == cid))
            .and_then(|cut| cut.frame.as_ref())
            .or(res.frame.as_ref());
        matches!(frame, Some(f) if f.kind == promo_model::ResourceFrameKind::Device)
    }

    /// Fetches (or serves from cache) the frame for `layer` at `source_time`.
    fn frame(&mut self, layer_id: &str, source_time: f64, tier: i32) -> Option<u64> {
        let key = (layer_id.to_string(), quantize(source_time), tier);
        if let Some(&id) = self.key_of.get(&key) {
            self.governor.touch(id);
            self.hits += 1;
            return Some(id);
        }

        let c_id = CString::new(layer_id).ok()?;
        let mut surface: IOSurfaceRef = std::ptr::null_mut();
        let mut flags: i32 = 0;
        let rc = (self.provider)(
            self.user,
            c_id.as_ptr(),
            source_time,
            tier,
            &mut surface,
            &mut flags,
        );
        if rc != 0 || surface.is_null() {
            return None;
        }
        unsafe { CFRetain(surface as *const c_void) };
        let (width, height) = unsafe { (IOSurfaceGetWidth(surface), IOSurfaceGetHeight(surface)) };
        let texture =
            match Compositor::import_iosurface(self.ctx, surface, width as u32, height as u32) {
                Ok(t) => t,
                Err(_) => {
                    unsafe { CFRelease(surface as *const c_void) };
                    return None;
                }
            };

        self.misses += 1;
        let id = self.next_id;
        self.next_id += 1;
        for victim in self.governor.admit(id, width * height * 4) {
            if let Some(k) = self.id_of.remove(&victim) {
                self.key_of.remove(&k);
            }
            self.cache.remove(&victim);
        }
        self.cache.insert(
            id,
            CachedFrame {
                surface,
                texture,
                width,
                height,
                flags,
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
                Some(res) => tl::source_time_for_local(res, local),
                None => local,
            };
            let before = self.misses;
            let _ = self.frame(&layer.id, source_time, self.preferred_tier);
            if self.misses > before {
                fetched += 1;
            }
        }
        fetched
    }

    /// Renders the composition at `time` into `output` (BGRA IOSurface of
    /// `output_width` × `output_height`; the canvas is aspect-fit inside).
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
    pub fn render_with_overlay(
        &mut self,
        time: f64,
        output: IOSurfaceRef,
        output_width: u32,
        output_height: u32,
        overlay: Option<(IOSurfaceRef, u32, u32)>,
    ) -> Result<(), GpuError> {
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
            let (is_media, is_drawing) = match layer.kind {
                ProjectLayerKind::Video | ProjectLayerKind::Image => (true, false),
                ProjectLayerKind::Drawing => (false, true),
                _ => continue,
            };

            let source_time = if layer.kind == ProjectLayerKind::Video {
                let local = tl::layer_local_time(layer, time);
                match self.resource_for(layer) {
                    Some(res) => tl::source_time_for_local(res, local),
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
            let Some(frame_id) = self.frame(&layer.id, source_time, tier) else {
                continue;
            };
            let frame = &self.cache[&frame_id];
            let (fw, fh) = (frame.width as f64, frame.height as f64);
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
                ..Default::default()
            };
            if is_media && !pre_framed {
                let zoom = tl::clamped_zoom(tr.zoom);
                quad.corner_radius = settings.video_corner_radius * zoom;
                quad.border_width = layer
                    .image_border_width
                    .unwrap_or(settings.video_border_width);
                quad.border_rgba = rgba_from_hex(
                    layer
                        .image_border_color_hex
                        .as_deref()
                        .unwrap_or(&settings.video_border_color_hex),
                );
            }
            quads.push(quad);
        }

        // Patch texture indices now that the used-frame list is final, and
        // borrow the textures in the same order.
        for (i, quad) in quads.iter_mut().enumerate() {
            quad.texture = Some(i);
        }
        let mut textures: Vec<&InputTexture> = used
            .iter()
            .map(|id| &self.cache[id].texture)
            .collect();

        // Caption/watermark overlay: one canvas-sized quad on top. Adopted
        // through the compositor's cache, so a stable overlay surface costs
        // nothing per frame.
        let overlay_texture;
        if let Some((surface, width, height)) = overlay {
            overlay_texture = self
                .compositor
                .import_iosurface_cached(self.ctx, surface, width, height)?;
            quads.push(SceneQuad {
                texture: Some(textures.len()),
                rect: [0.0, 0.0, canvas.width(), canvas.height()],
                ..Default::default()
            });
            textures.push(&overlay_texture);
        }

        let scene = Scene {
            canvas_width: canvas.width(),
            canvas_height: canvas.height(),
            background_rgba: background,
            output_width,
            output_height,
            bars_rgba: background,
            quads,
        };
        self.compositor
            .compose_to_iosurface_borrowed(self.ctx, &scene, &textures, output)
    }
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

#[cfg(test)]
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
        out_surface: *mut IOSurfaceRef,
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
            *out_surface = surface.raw();
            *out_flags = 0;
        }
        state.keep_alive.push(surface);
        0
    }

    fn fixture_meta(canvas: f64) -> ProjectMetadata {
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
        let meta = fixture_meta(64.0);
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
        let meta = fixture_meta(64.0);
        let (mut engine, _state) = make_engine(
            meta,
            vec![("VID".into(), [255, 0, 0, 255], 32)], // blue video frame
            64 << 20,
        );
        let out = OwnedIoSurface::new_bgra(64, 64).unwrap();

        // Opaque white overlay covering the canvas hides everything below.
        let overlay = OwnedIoSurface::new_bgra(64, 64).expect("overlay");
        overlay.write_pixels(&[255, 255, 255, 255].repeat(64 * 64)).unwrap();
        engine
            .render_with_overlay(3.0, out.raw(), 64, 64, Some((overlay.raw(), 64, 64)))
            .expect("render");
        assert_eq!(pixel(&out, 30, 30), [255, 255, 255, 255], "overlay covers video");
        assert_eq!(pixel(&out, 2, 2), [255, 255, 255, 255], "overlay covers bg");

        // A transparent overlay leaves the composition untouched.
        let clear = OwnedIoSurface::new_bgra(64, 64).expect("clear");
        clear.write_pixels(&[0, 0, 0, 0].repeat(64 * 64)).unwrap();
        engine
            .render_with_overlay(3.0, out.raw(), 64, 64, Some((clear.raw(), 64, 64)))
            .expect("render");
        assert_eq!(pixel(&out, 30, 30), [255, 0, 0, 255], "video still visible");
        assert_eq!(pixel(&out, 2, 2), [0, 51, 0, 255], "background still visible");
    }

    #[test]
    fn set_project_keeps_cache_for_untouched_layers() {
        let meta = fixture_meta(64.0);
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
        let meta = fixture_meta(64.0);
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
        let meta = fixture_meta(64.0);
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
        let meta = fixture_meta(64.0);
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
        let meta = fixture_meta(64.0);
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
