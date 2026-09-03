//! Rendering frames from a project folder.
//!
//! The CLI is a *host* for `promo-engine`: it answers the engine's frame
//! provider with plain BGRA pixels loaded from disk, and takes the composited
//! result out of a wgpu texture. That is exactly the portable path R1 opened
//! — on Apple the same engine is fed zero-copy IOSurfaces instead.

use crate::project::Project;
use promo_engine::{HostSurface, PreviewEngine, FLAG_PRE_FRAMED, SURFACE_CPU_PIXELS};
use promo_gpu::{wgpu, GpuContext, GpuSurface};
use promo_media::{Registry, VideoDecoder};
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr};
use std::path::PathBuf;
use std::sync::Mutex;

/// What the host can answer with.
///
/// Images are decoded once up front — the engine caches textures itself, so
/// this only avoids re-decoding a PNG for every frame of a slideshow. Video
/// cannot be held that way: a clip is far too big as frames, and the engine
/// asks for one source time at a time.
struct HostState {
    /// resource id → BGRA pixels + size
    frames: HashMap<String, (Vec<u8>, u32, u32, i32)>,
    /// layer id → the CUT it shows instead of its resource's whole file:
    /// (the base resource id, the cut file's pixels). An image cut's pixels
    /// live in the cut's own staged file, exactly as the Mac app draws them;
    /// the redirect is keyed by LAYER and applies only while the layer shows
    /// its own resource, so a keyframe swap still swaps.
    cuts: HashMap<String, (String, Vec<u8>, u32, u32, i32)>,
    /// layer id → its clip, decoder opened only while it is on screen.
    videos: HashMap<String, VideoLayer>,
}

struct VideoLayer {
    path: PathBuf,
    /// The layer's window on the composition timeline.
    start: f64,
    end: f64,
    /// Opened on first use and dropped once the window has passed — a
    /// decoder is a live ffmpeg process, so holding one per layer for the
    /// whole render costs a process per clip whether or not it is visible.
    /// Five is fine; fifty is a scaling cliff.
    decoder: Option<Box<dyn VideoDecoder>>,
    /// True when `path` is a tier-1 proxy of the resource's file.
    proxied: bool,
    /// The decoded frame, held so the provider can hand out a pointer to it.
    /// The engine copies during the call, but the buffer must outlive the
    /// call itself.
    last: Option<(Vec<u8>, u32, u32)>,
}

impl HostState {
    /// Opens what this moment needs and closes what it has finished with.
    ///
    /// Called before each rendered frame, so decoders track the playhead
    /// rather than the whole composition.
    fn retain_only_active(&mut self, time: f64, registry: &Registry) {
        for layer in self.videos.values_mut() {
            // A small margin keeps a decoder alive across the dissolve into
            // the next clip rather than reopening it a frame later.
            let active = time >= layer.start - 0.25 && time <= layer.end + 0.25;
            match (active, layer.decoder.is_some()) {
                (true, false) => {
                    layer.decoder = registry.open_decoder(&layer.path).ok();
                }
                (false, true) => {
                    layer.decoder = None;
                    layer.last = None;
                }
                _ => {}
            }
        }
    }

    /// Test-only: how many decoders are live right now.
    #[cfg(test)]
    fn open_decoders(&self) -> usize {
        self.videos.values().filter(|v| v.decoder.is_some()).count()
    }
}

extern "C" fn provider(
    user: *mut c_void,
    layer_id: *const c_char,
    resource_id: *const c_char,
    source_time: f64,
    _tier: i32,
    out_surface: *mut HostSurface,
    out_flags: *mut i32,
) -> i32 {
    let state = unsafe { &*(user as *const Mutex<HostState>) };
    let mut state = state.lock().unwrap();
    let id = unsafe { CStr::from_ptr(layer_id) }
        .to_string_lossy()
        .to_string();

    // A still image: the same pixels whatever the time — but WHICH still can
    // change, since a keyframe may swap what the layer shows, so these are
    // keyed by resource rather than by layer.
    let resource = unsafe { CStr::from_ptr(resource_id) }
        .to_string_lossy()
        .to_string();
    // A layer aimed at an image cut shows the cut's staged file, not the
    // source — but only while the engine says it shows its OWN resource:
    // a keyframe swap outranks the cut, whose id means nothing to the
    // swapped-in material.
    if let Some((base, pixels, width, height, flags)) = state.cuts.get(&id) {
        if *base == resource {
            unsafe {
                *out_surface = HostSurface {
                    kind: SURFACE_CPU_PIXELS,
                    data: pixels.as_ptr(),
                    width: *width,
                    height: *height,
                    bytes_per_row: width * 4,
                    ..Default::default()
                };
                *out_flags = *flags;
            }
            return 0;
        }
    }
    if let Some((pixels, width, height, flags)) = state.frames.get(&resource) {
        unsafe {
            *out_surface = HostSurface {
                kind: SURFACE_CPU_PIXELS,
                data: pixels.as_ptr(),
                width: *width,
                height: *height,
                bytes_per_row: width * 4,
                ..Default::default()
            };
            *out_flags = *flags;
        }
        return 0;
    }

    // A video layer: the engine has already mapped composition time through
    // the resource's trim, so `source_time` is where to read the clip.
    let Some(video) = state.videos.get_mut(&id) else {
        return 1; // Not ours to draw — the engine skips the layer.
    };
    let Some(decoder) = video.decoder.as_mut() else {
        // Outside its window, or the file would not open.
        return 1;
    };
    match decoder.frame_at(source_time.max(0.0)) {
        Ok(Some(GpuSurface::CpuPixels {
            data,
            width,
            height,
            ..
        })) => {
            video.last = Some((data, width, height));
        }
        // Past the end, or a decode hiccup: keep showing the last good frame
        // rather than punching a hole in the composition.
        Ok(_) | Err(_) if video.last.is_some() => {}
        Ok(_) => return 1,
        Err(e) => {
            eprintln!("promo: {id}: {e}");
            return 1;
        }
    }
    let Some((pixels, width, height)) = video.last.as_ref() else {
        return 1;
    };
    unsafe {
        *out_surface = HostSurface {
            kind: SURFACE_CPU_PIXELS,
            data: pixels.as_ptr(),
            width: *width,
            height: *height,
            bytes_per_row: width * 4,
            ..Default::default()
        };
        *out_flags = 0;
    }
    0
}

/// Whether a render reads proxies (B3.1): `Auto` uses a tier-1 proxy when
/// one is already built AND the output's long edge fits the proxy's;
/// `On` builds missing proxies first; `Off` never opens one — a full-size
/// export never does either way, by the size rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProxyPolicy {
    #[default]
    Auto,
    On,
    Off,
}

impl ProxyPolicy {
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "auto" => Ok(ProxyPolicy::Auto),
            "on" => Ok(ProxyPolicy::On),
            "off" => Ok(ProxyPolicy::Off),
            other => Err(format!("--proxy: expected auto, on or off, got `{other}`")),
        }
    }
}

pub struct Renderer {
    ctx: &'static GpuContext,
    registry: Registry,
    engine: PreviewEngine,
    // Boxed so the pointer handed to the engine stays valid.
    _state: Box<Mutex<HostState>>,
    target: wgpu::Texture,
    width: u32,
    height: u32,
    /// Host-rasterized overlay (watermark), uploaded once and composited
    /// over every frame until cleared — the same final quad the Apple
    /// path's overlay is, so a watermarked preview matches a watermarked
    /// export.
    overlay: Option<promo_gpu::compositor::InputTexture>,
}

impl Renderer {
    /// Decodes every renderable image layer up front, then builds the engine.
    ///
    /// Loading eagerly keeps the provider free of I/O and makes a missing or
    /// corrupt file an error at startup rather than a silently blank layer
    /// halfway through a render.
    pub fn new(project: &Project, width: u32, height: u32) -> Result<Self, String> {
        Self::with_proxy(project, width, height, ProxyPolicy::Auto)
    }

    /// `new`, with a say on proxies. When any video layer opens a proxy the
    /// engine is asked for tier 1, so its own tier rule and the proxy agree.
    pub fn with_proxy(
        project: &Project,
        width: u32,
        height: u32,
        proxy: ProxyPolicy,
    ) -> Result<Self, String> {
        let ctx = GpuContext::shared().ok_or("no GPU adapter available")?;

        let registry = Registry::with_defaults();
        let (frames, cuts, videos) =
            Self::stage_with(project, &registry, proxy, width.max(height))?;
        let used_proxies = videos.values().any(|v| v.proxied);
        let state = Box::new(Mutex::new(HostState {
            frames,
            cuts,
            videos,
        }));
        let user = &*state as *const Mutex<HostState> as *mut c_void;
        let mut engine = PreviewEngine::new(project.meta.clone(), provider, user, 512 << 20)
            .map_err(|e| format!("engine: {e:?}"))?;
        if used_proxies {
            engine.set_preferred_tier(1);
        }
        // Offline renderer: the export clock is monotonic and quality is
        // the point — per-time frames skip the cache, and a motion-blur
        // walk gets the export-grade sample cap rather than the preview's.
        engine.set_export_mode(true);
        Self::provide_models(&mut engine, project);

        let target = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("promo-cli-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        Ok(Self {
            ctx,
            registry,
            engine,
            _state: state,
            target,
            width,
            height,
            overlay: None,
        })
    }

    /// Reads what every renderable layer needs off disk: images decoded to
    /// premultiplied BGRA up front (the compositor samples premultiplied;
    /// straight alpha saturates every soft edge), videos opened once so a
    /// broken file fails here rather than mid-render, then closed — the
    /// render reopens what its playhead needs. Every resource a layer can
    /// show is staged: its own, plus anything a keyframe swaps to.
    /// The file a video layer decodes from: the source, or its tier-1
    /// proxy when the policy and the output size say so.
    fn pick_proxy(
        source: &std::path::Path,
        proxy: ProxyPolicy,
        output_long_edge: u32,
    ) -> Result<(PathBuf, bool), String> {
        use promo_media::proxy;
        let fits = output_long_edge <= proxy::TIER1_LONG_EDGE;
        let cache = proxy::cache_dir();
        match proxy {
            ProxyPolicy::Off => Ok((source.to_path_buf(), false)),
            ProxyPolicy::Auto => Ok(match proxy::available(&cache, source, 1) {
                Some(ready) if fits => (ready, true),
                _ => (source.to_path_buf(), false),
            }),
            ProxyPolicy::On => {
                let ready = proxy::ensure(&cache, source, proxy::TIER1_LONG_EDGE)
                    .map_err(|e| e.to_string())?;
                Ok(if fits {
                    (ready, true)
                } else {
                    (source.to_path_buf(), false)
                })
            }
        }
    }

    #[allow(clippy::type_complexity)]
    fn stage(
        project: &Project,
        registry: &Registry,
    ) -> Result<
        (
            HashMap<String, (Vec<u8>, u32, u32, i32)>,
            HashMap<String, (String, Vec<u8>, u32, u32, i32)>,
            HashMap<String, VideoLayer>,
        ),
        String,
    > {
        Self::stage_with(project, registry, ProxyPolicy::Off, u32::MAX)
    }

    /// `stage`, choosing a proxy for each video layer by `proxy` and the
    /// output's long edge.
    #[allow(clippy::type_complexity)]
    fn stage_with(
        project: &Project,
        registry: &Registry,
        proxy: ProxyPolicy,
        output_long_edge: u32,
    ) -> Result<
        (
            HashMap<String, (Vec<u8>, u32, u32, i32)>,
            HashMap<String, (String, Vec<u8>, u32, u32, i32)>,
            HashMap<String, VideoLayer>,
        ),
        String,
    > {
        let mut frames = HashMap::new();
        let mut cuts = HashMap::new();
        let mut videos = HashMap::new();
        // Colour look-up tables: every `.cube` resource becomes a strip the
        // engine asks for by resource id (under a synthetic layer) — the
        // same still path an image takes.
        for resource in project
            .resources()
            .iter()
            .filter(|r| r.kind == promo_model::ProjectResourceKind::Lut)
        {
            let Some(path) = project.resource_path(resource) else {
                continue;
            };
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let lut = promo_media::lut::parse_cube(&text)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            let (pixels, width, height) = lut.strip_bgra8();
            frames.insert(resource.id.clone(), (pixels, width, height, 0));
        }
        for layer in promo_model::nesting::all_layers(&project.meta) {
            if project.unsupported(layer).is_some() {
                continue;
            }
            let mut candidates: Vec<String> = layer.resource_id.iter().cloned().collect();
            candidates.extend(layer.keyframes.iter().filter_map(|k| k.resource_id.clone()));
            // A model layer's own file is a `.glb` (the engine gets its bytes
            // through `provide_models`); what it stages are the STILLS its
            // material slots are bound to, served by resource id like any
            // other picture.
            if layer.kind == promo_model::ProjectLayerKind::Model {
                candidates = candidates
                    .iter()
                    .filter_map(|id| project.resource(id))
                    .flat_map(|model| model.materials.iter().flat_map(|m| m.values()))
                    .filter_map(|binding| binding.resource_id().map(str::to_string))
                    .collect();
                // A bound VIDEO decodes like a video layer would, under the
                // synthetic slot layer the engine asks with.
                let bound_videos: Vec<promo_model::ProjectResource> = candidates
                    .iter()
                    .filter_map(|id| project.resource(id))
                    .filter(|r| r.kind == promo_model::ProjectResourceKind::Video)
                    .cloned()
                    .collect();
                for resource in bound_videos {
                    let Some(source) = project.resource_path(&resource) else {
                        continue;
                    };
                    let (path, proxied) = Self::pick_proxy(&source, proxy, output_long_edge)?;
                    registry
                        .open_decoder(&path)
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                    let start = layer.start_time.max(0.0);
                    let end = layer.duration.map(|d| start + d).unwrap_or(f64::MAX);
                    videos.insert(
                        format!("slot\u{1f}{}", resource.id),
                        VideoLayer {
                            path,
                            start,
                            end,
                            decoder: None,
                            last: None,
                            proxied,
                        },
                    );
                }
                candidates.retain(|id| {
                    project
                        .resource(id)
                        .is_some_and(|r| r.kind != promo_model::ProjectResourceKind::Video)
                });
            }
            candidates.dedup();
            // Only stills are preloaded; video opens while it is on screen.
            // Video keys on the BASE resource (swaps never target video).
            if layer.kind == promo_model::ProjectLayerKind::Video {
                let Some(resource) = candidates.first().and_then(|id| project.resource(id)) else {
                    continue;
                };
                let Some(source) = project.resource_path(resource) else {
                    continue;
                };
                let (path, proxied) = Self::pick_proxy(&source, proxy, output_long_edge)?;
                registry
                    .open_decoder(&path)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                let start = layer.start_time.max(0.0);
                let end = layer.duration.map(|d| start + d).unwrap_or(f64::MAX);
                videos.insert(
                    layer.id.clone(),
                    VideoLayer {
                        path,
                        start,
                        end,
                        decoder: None,
                        last: None,
                        proxied,
                    },
                );
                continue;
            }
            for candidate in &candidates {
                let Some(resource) = project.resource(candidate) else {
                    continue;
                };
                let Some(path) = project.resource_path(resource) else {
                    continue;
                };
                if frames.contains_key(candidate) {
                    continue;
                }
                let wears = (resource.kind == promo_model::ProjectResourceKind::Image
                    && resource.sprite.is_none())
                .then_some(resource.frame.as_ref())
                .flatten();
                frames.insert(
                    candidate.clone(),
                    Self::baked(Self::decode_premultiplied(&path)?, wears),
                );
            }
            // The layer's image cut, if it names one its resource holds: the
            // crop's pixels are its own staged file, sitting beside the
            // source (the Mac app's layout). A cut with no file yet stages
            // nothing and the layer keeps showing the whole image — a
            // half-authored cut degrades, it does not blank the layer.
            if let (Some(cut_id), Some(resource_id)) =
                (layer.image_cut_id.as_ref(), layer.resource_id.as_ref())
            {
                if let Some(resource) = project.resource(resource_id) {
                    let cut = resource.image_cuts.iter().find(|c| &c.id == cut_id);
                    if let Some(cut) = cut.filter(|c| !c.filename.is_empty()) {
                        if let Some(path) = project
                            .resource_path(resource)
                            .map(|p| p.with_file_name(&cut.filename))
                            .filter(|p| p.is_file())
                        {
                            cuts.insert(layer.id.clone(), {
                                let wears = cut.frame.as_ref().or(resource.frame.as_ref());
                                let (px, w, h, flags) =
                                    Self::baked(Self::decode_premultiplied(&path)?, wears);
                                (resource_id.clone(), px, w, h, flags)
                            });
                        }
                    }
                }
            }
        }
        // A layer's `imageOrientation` is baked into ITS pixels — a layer
        // field, so the turned copy is keyed by layer like a cut (a keyframe
        // swap outranks it the same way). The apps turn the picture before
        // any device slab is baked, so the slab is baked around the turned
        // picture here too. Until this the field was parsed and ignored,
        // and a sideways photo rendered upright headless and turned in the
        // app.
        for layer in promo_model::nesting::all_layers(&project.meta) {
            let Some(orientation) = layer.image_orientation else {
                continue;
            };
            if matches!(orientation, promo_model::ImageOrientation::Original)
                || layer.kind != promo_model::ProjectLayerKind::Image
            {
                continue;
            }
            let Some(resource) = layer
                .resource_id
                .as_ref()
                .and_then(|id| project.resource(id))
            else {
                continue;
            };
            let Some(base_path) = project.resource_path(resource) else {
                continue;
            };
            // The cut's file when the layer shows one, else the source.
            let cut = layer
                .image_cut_id
                .as_ref()
                .and_then(|cid| resource.image_cuts.iter().find(|c| &c.id == cid))
                .filter(|c| !c.filename.is_empty());
            let path = cut
                .map(|c| base_path.with_file_name(&c.filename))
                .filter(|p| p.is_file())
                .unwrap_or(base_path);
            let wears = cut
                .and_then(|c| c.frame.as_ref())
                .or(resource.frame.as_ref())
                .filter(|_| resource.sprite.is_none());
            let (px, w, h) = Self::decode_premultiplied(&path)?;
            let turned = promo_timeline::rotate_bgra(&px, w, h, orientation);
            let (px, w, h, flags) = Self::baked(turned, wears);
            cuts.insert(layer.id.clone(), (resource.id.clone(), px, w, h, flags));
        }
        Ok((frames, cuts, videos))
    }

    /// Bakes a device slab into freshly decoded pixels — the SAME bake the
    /// apps run with Core Graphics (`promo_timeline::bake_slab`), so a
    /// headless texture arrives already framed and flagged, and the
    /// slab-sized box layout resolves is the box the picture actually fills
    /// (issue #6: the frame silently rendered as nothing, and the bare
    /// picture drew mis-sized inside slab-sized layout).
    fn baked(
        decoded: (Vec<u8>, u32, u32),
        frame: Option<&promo_model::ResourceFrame>,
    ) -> (Vec<u8>, u32, u32, i32) {
        let (px, w, h) = decoded;
        if let Some(f) = frame {
            if let Some((baked, bw, bh)) = promo_timeline::bake_slab(&px, w, h, f) {
                return (baked, bw, bh, FLAG_PRE_FRAMED);
            }
        }
        (px, w, h, 0)
    }

    /// One image file as the compositor wants it: premultiplied BGRA.
    /// Straight alpha saturates every soft edge.
    fn decode_premultiplied(path: &std::path::Path) -> Result<(Vec<u8>, u32, u32), String> {
        let decoded = image::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let rgba = decoded.to_rgba8();
        let (w, h) = rgba.dimensions();
        let mut bgra = rgba.into_raw();
        for px in bgra.chunks_exact_mut(4) {
            px.swap(0, 2);
            let a = px[3] as u32;
            for channel in px.iter_mut().take(3) {
                *channel = ((*channel as u32 * a + 127) / 255) as u8;
            }
        }
        Ok((bgra, w, h))
    }

    /// Replaces the engine's project, keeping the GPU pipeline and every
    /// cached frame whose layer did not change — what an editor calls per
    /// edit instead of rebuilding the renderer. The staged media is
    /// re-read from the project too, so a command that grew the resource
    /// list previews without a reopen. The state swaps INSIDE the mutex:
    /// the engine holds a raw pointer to the box, so the box must never
    /// move.
    /// Every model resource's bytes, read here (the engine does no I/O)
    /// and handed over. A missing or undecodable file leaves the layer
    /// drawing nothing; `unsupported()` names the missing ones.
    fn provide_models(engine: &mut PreviewEngine, project: &Project) {
        for resource in project.meta.resources.as_deref().unwrap_or(&[]) {
            if resource.kind != promo_model::ProjectResourceKind::Model {
                continue;
            }
            let Some(path) = project.resource_path(resource) else {
                continue;
            };
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if let Err(why) = engine.provide_model(&resource.id, &bytes) {
                eprintln!("model {}: {why}", resource.filename);
            }
        }
    }

    pub fn set_project(&mut self, project: &Project) -> Result<(), String> {
        let (frames, cuts, videos) = Self::stage(project, &self.registry)?;
        {
            let mut state = self
                ._state
                .lock()
                .map_err(|_| "host state poisoned".to_string())?;
            state.frames = frames;
            state.cuts = cuts;
            state.videos = videos;
        }
        self.engine.set_project(project.meta.clone());
        Self::provide_models(&mut self.engine, project);
        Ok(())
    }

    /// Sets (or clears, with None) the overlay composited over every
    /// subsequent frame. `bgra` is PREMULTIPLIED BGRA, stretched over the
    /// canvas — rasterize it at canvas size for crisp pixels. Uploaded
    /// once here; per-frame cost is one extra quad.
    pub fn set_overlay(&mut self, overlay: Option<(&[u8], u32, u32)>) -> Result<(), String> {
        self.overlay = match overlay {
            None => None,
            Some((bgra, width, height)) => Some(
                promo_gpu::compositor::Compositor::upload_texture(self.ctx, bgra, width, height)
                    .map_err(|e| format!("overlay upload: {e:?}"))?,
            ),
        };
        Ok(())
    }

    /// Renders one frame and returns it as RGBA rows, ready for PNG.
    pub fn frame_rgba(&mut self, time: f64) -> Result<Vec<u8>, String> {
        let mut bgra = self.frame_bgra(time)?;
        for px in bgra.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        Ok(bgra)
    }

    /// How many decoders are open right now — the lifetime invariant is
    /// only observable from here, so this exists for its test.
    #[cfg(test)]
    pub fn open_decoders(&self) -> usize {
        self._state.lock().map(|s| s.open_decoders()).unwrap_or(0)
    }

    /// Renders one frame as BGRA rows — what a raw-video pipe wants.
    /// Render over nothing — for PNGs and ProRes 4444 exports with alpha.
    pub fn set_transparent_plate(&mut self, on: bool) {
        self.engine.set_transparent_plate(on);
    }

    pub fn frame_bgra(&mut self, time: f64) -> Result<Vec<u8>, String> {
        if let Ok(mut state) = self._state.lock() {
            state.retain_only_active(time, &self.registry);
        }
        self.engine
            .render_to_texture_with_overlay(
                time,
                &self.target,
                self.width,
                self.height,
                self.overlay.as_ref(),
            )
            .map_err(|e| format!("render at {time}s: {e:?}"))?;
        Ok(self.read_back())
    }

    fn read_back(&self) -> Vec<u8> {
        let unpadded = self.width * 4;
        // wgpu requires 256-byte-aligned rows for texture→buffer copies.
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;
        let buffer = self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("promo-cli-readback"),
            size: (padded * self.height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &self.target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        self.ctx.queue.submit(Some(encoder.finish()));
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.ctx.device.poll(wgpu::Maintain::Wait);
        let mapped = slice.get_mapped_range();
        let mut out = Vec::with_capacity((unpadded * self.height) as usize);
        for row in 0..self.height as usize {
            let start = row * padded as usize;
            out.extend_from_slice(&mapped[start..start + unpadded as usize]);
        }
        drop(mapped);
        buffer.unmap();
        out
    }
}

/// How an export ended, when it did not fail.
#[derive(Debug, PartialEq, Eq)]
pub enum ExportOutcome {
    Finished,
    /// The progress callback said stop. The partial file is REMOVED: an
    /// mp4 that ends mid-scene but plays looks exported, and a cancelled
    /// export must never leave something that looks like an answer.
    Cancelled,
}

/// Everything an mp4 export needs decided up front.
pub struct ExportSettings {
    pub width: u32,
    pub height: u32,
    /// H.264 (mp4) by default; ProRes 422 HQ or 4444 (mov).
    pub codec: promo_media::VideoCodec,
    /// Keep alpha: the project renders over nothing and the frames'
    /// alpha goes into a ProRes 4444.
    pub alpha: bool,
    /// Seconds on the composition timeline.
    pub start: f64,
    pub end: f64,
    pub fps: f64,
    /// Host-rasterized overlay (watermark) over every frame: PREMULTIPLIED
    /// BGRA, stretched over the canvas. None exports clean.
    pub overlay: Option<(Vec<u8>, u32, u32)>,
}

/// Renders `project` straight into the encoder as raw BGRA — no
/// intermediate PNGs, no temp directory the size of the uncompressed
/// video. Encoding lives in promo-media, so every front end writes video
/// the same way.
///
/// `progress` is called once per encoded frame with (done, total);
/// returning false cancels. The CLI prints from it; the FFI's export job
/// stores it for polling.
pub fn export_video(
    project: &Project,
    out: &std::path::Path,
    settings: &ExportSettings,
    progress: &mut dyn FnMut(usize, usize) -> bool,
    proxy: ProxyPolicy,
) -> Result<ExportOutcome, String> {
    let ExportSettings {
        width,
        height,
        start,
        end,
        fps,
        ref overlay,
        ..
    } = *settings;
    let count = (((end - start) * fps).round() as usize).max(1);

    let mut renderer = Renderer::with_proxy(project, width, height, proxy)?;
    renderer.set_transparent_plate(settings.alpha);
    if let Some((bgra, overlay_width, overlay_height)) = overlay {
        renderer.set_overlay(Some((bgra, *overlay_width, *overlay_height)))?;
    }
    let audio = build_soundtrack(project, end - start)?;
    let chapters: Vec<(f64, String)> = project
        .meta
        .markers
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter(|m| m.kind == promo_model::MarkerKind::Chapter)
        .map(|m| (m.time, m.name.clone()))
        .collect();
    let spec = promo_media::EncodeSpec {
        chapters,
        chapters_end: (settings.end - settings.start).max(0.0),
        codec: if settings.alpha {
            promo_media::VideoCodec::ProRes4444
        } else {
            settings.codec
        },
        alpha: settings.alpha,
        width,
        height,
        fps,
        quality: 18,
        audio,
    };
    let registry = Registry::with_defaults();
    let mut encoder = registry
        .open_encoder(out, &spec)
        .map_err(|e| e.to_string())?;

    for i in 0..count {
        let time = start + i as f64 / fps;
        let mut bgra = renderer.frame_bgra(time)?;
        if settings.alpha {
            // The compositor's output is premultiplied; the container wants
            // straight alpha.
            for px in bgra.chunks_exact_mut(4) {
                let a = px[3] as u32;
                if a > 0 && a < 255 {
                    px[0] = ((px[0] as u32 * 255 + a / 2) / a).min(255) as u8;
                    px[1] = ((px[1] as u32 * 255 + a / 2) / a).min(255) as u8;
                    px[2] = ((px[2] as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        encoder.write_frame(&bgra).map_err(|e| e.to_string())?;
        if !progress(i + 1, count) {
            // Dropping the encoder kills its ffmpeg (promo-media's Drop),
            // which is what releases the file for deletion on Windows.
            drop(encoder);
            match std::fs::remove_file(out) {
                Ok(()) => {}
                // A cancel that lands before ffmpeg created the file is the
                // promised state already: nothing left behind.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                // Said out loud: a partial mp4 that survives a cancel looks
                // exported, which is worse than an error.
                Err(e) => {
                    return Err(format!(
                        "cancelled, but the partial file could not be removed ({}): {e}",
                        out.display()
                    ))
                }
            }
            return Ok(ExportOutcome::Cancelled);
        }
    }
    encoder.finish().map_err(|e| e.to_string())?;
    Ok(ExportOutcome::Finished)
}

/// Writes RGBA rows as a PNG.
pub fn write_png(
    path: &std::path::Path,
    rgba: &[u8],
    width: u32,
    height: u32,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    image::save_buffer(path, rgba, width, height, image::ColorType::Rgba8)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// The composition's soundtrack: the apps' mix graph (`audio_inputs` —
/// which layers sound, which slice plays where, at what level, ducked by
/// whom) executed by the core mixer. Trims, media cuts, extended pauses,
/// speed (pitch preserved), keyframed volume through the perceptual taper,
/// focus ducking and disabled tracks all land here, so `promo video` and
/// the Mac export describe the same sound.
pub fn build_soundtrack(
    project: &Project,
    duration: f64,
) -> Result<Option<promo_media::AudioBuffer>, String> {
    use promo_engine::{mix_chunk, MixInput};
    use promo_media::TrackSelection;
    use promo_timeline::{
        audio_inputs, level_points, scaled_segments, AudioSource, VolumePoint, DUCK_FACTOR,
        DUCK_RAMP,
    };

    const SAMPLE_RATE: u32 = 48_000;
    const CHANNELS: u16 = 2;

    if duration <= 0.0 {
        return Ok(None);
    }
    let registry = Registry::with_defaults();
    let reader = registry.audio_reader();

    // A layer whose file is gone contributes nothing (Swift `renderableLayers`).
    let renderable = |layer: &promo_model::ProjectLayer| -> bool {
        match layer.resource_id.as_deref() {
            Some(id) => !project.is_missing(id),
            None => true,
        }
    };
    // Stage members (rung 33) sound like any layer, so the mix walks the
    // lowered form the engine draws.
    let lowered = project.meta.lowered();
    let (inputs, focus) = audio_inputs(&lowered, &renderable);

    // One placed slice of decoded PCM with its level curve.
    struct Placed {
        samples: Vec<f32>,
        start_time: f64,
        points: Vec<VolumePoint>,
    }
    let mut placed: Vec<Placed> = Vec::new();

    for input in &inputs {
        let path = match &input.source {
            AudioSource::Resource(id) => project
                .resource(id)
                .and_then(|res| project.resource_path(res)),
            AudioSource::VoiceClip(filename) => {
                let candidate = project.dir.join("Resources").join(filename);
                candidate.is_file().then_some(candidate)
            }
        };
        let Some(path) = path else { continue };

        // The resource's effect chain (rung 21), applied after the tempo.
        let effects: Option<String> = match &input.source {
            AudioSource::Resource(id) => project
                .resource(id)
                .and_then(|res| res.audio_effects.as_deref())
                .and_then(promo_media::effects_chain),
            AudioSource::VoiceClip(_) => None,
        };
        let speed = if input.speed.is_finite() {
            input.speed.clamp(0.1, 10.0)
        } else {
            1.0
        };
        let selection = if input.single_track {
            TrackSelection::First
        } else if input.disabled_audio_track_indices.is_empty() {
            TrackSelection::All
        } else {
            TrackSelection::Except(input.disabled_audio_track_indices.clone())
        };
        // Decode first: a layer contributes only if its asset has audio,
        // which most screen recordings do not. The stretched stream has its
        // own clock; source seconds divide by `speed` to index it.
        let decoded = reader
            .read_tracks_with(
                &path,
                SAMPLE_RATE,
                CHANNELS,
                speed,
                &selection,
                effects.as_deref(),
            )
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let Some(audio) = decoded else { continue };
        let asset_seconds = audio.duration_s() * speed;

        let ranges = match &input.included_ranges {
            Some(ranges) => ranges.clone(),
            None => vec![promo_model::VideoTrimRange {
                start: 0.0,
                end: asset_seconds,
            }],
        };
        if ranges.is_empty() {
            continue;
        }
        let start_output = input.start_time.max(0.0);
        if start_output >= duration {
            continue;
        }
        let layer_limit = input
            .duration_cap
            .unwrap_or(f64::MAX)
            .min(duration - start_output);
        if layer_limit <= 0.0 {
            continue;
        }
        let segments = scaled_segments(
            &ranges,
            start_output,
            layer_limit,
            &input.extended_pauses,
            speed,
        );
        if segments.is_empty() {
            continue;
        }
        let track_start = segments
            .iter()
            .map(|s| s.output_start)
            .fold(f64::MAX, f64::min);
        let track_end = segments
            .iter()
            .map(|s| s.output_start + s.output_duration)
            .fold(f64::MIN, f64::max);
        let base_volume = input.volume.clamp(0.0, 1.0);
        let automation = input.volume_points.clone().unwrap_or_else(|| {
            vec![VolumePoint {
                time: track_start,
                volume: base_volume,
            }]
        });
        let points = level_points(
            &automation,
            track_start,
            track_end,
            &focus,
            input.is_focused,
            DUCK_FACTOR,
            DUCK_RAMP,
        );

        let frame = CHANNELS as usize;
        for seg in segments {
            // Index the STRETCHED stream: a slice at source 4s of a 2x clip
            // begins 2s into the PCM ffmpeg produced.
            let from = ((seg.source_start / speed) * SAMPLE_RATE as f64).round() as usize * frame;
            let take = (seg.output_duration * SAMPLE_RATE as f64).round() as usize * frame;
            if from >= audio.samples.len() || take == 0 {
                continue;
            }
            let to = (from + take).min(audio.samples.len());
            placed.push(Placed {
                samples: audio.samples[from..to].to_vec(),
                start_time: seg.output_start,
                points: points.clone(),
            });
        }
    }
    if placed.is_empty() {
        return Ok(None);
    }

    let frames = (duration * SAMPLE_RATE as f64).ceil() as usize;
    let mut output = vec![0.0f32; frames * CHANNELS as usize];
    let mix_inputs: Vec<MixInput> = placed
        .iter()
        .map(|p| MixInput {
            samples: &p.samples,
            start_time: p.start_time,
            points: &p.points,
        })
        .collect();
    mix_chunk(
        &mut output,
        CHANNELS as usize,
        SAMPLE_RATE as f64,
        0.0,
        &mix_inputs,
    );

    Ok(Some(promo_media::AudioBuffer {
        samples: output,
        channels: CHANNELS,
        sample_rate: SAMPLE_RATE,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Writes a tone file with ffmpeg's `sine` source; None when ffmpeg is
    /// not around.
    fn tone(path: &std::path::Path, hz: u32, seconds: f64) -> bool {
        Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                &format!("sine=frequency={hz}:sample_rate=48000:duration={seconds}"),
                "-c:a",
                "pcm_s16le",
            ])
            .arg(path)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn rms(audio: &promo_media::AudioBuffer, from: f64, to: f64) -> f32 {
        let frame = audio.channels as usize;
        let a = (from * audio.sample_rate as f64) as usize * frame;
        let b = ((to * audio.sample_rate as f64) as usize * frame).min(audio.samples.len());
        let slice = &audio.samples[a..b];
        (slice.iter().map(|s| s * s).sum::<f32>() / slice.len().max(1) as f32).sqrt()
    }

    /// The headless soundtrack is the apps' mix: keyframed volume through
    /// the perceptual taper, a focused layer ducking everything else while
    /// it plays, trims placing a slice where the layer starts. Before this
    /// the CLI mixed every source at unity and `promo video` of a narrated
    /// project came out with the music never dipping under the voice.
    #[test]
    fn the_soundtrack_executes_the_apps_mix_graph() {
        let dir = std::env::temp_dir().join("promo-cli-soundtrack-test.promo");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Resources")).expect("dir");
        if !tone(&dir.join("Resources/music.wav"), 440, 8.0)
            || !tone(&dir.join("Resources/voice.wav"), 660, 2.0)
        {
            eprintln!("ffmpeg unavailable; skipping");
            return;
        }
        // Music 0…8s with a gain step to 0.25 at 6s (taper → 0.0625);
        // a SILENT focused voice at 2…4s that only ducks; a trimmed cut
        // of the music (4…5s of the file) placed at 9s.
        std::fs::write(
            dir.join("metadata.json"),
            r#"{"id":"P","name":"mix","createdAt":0,"state":"recorded",
                "trimStart":0,"trimEnd":0,"videoDuration":0,"subtitles":[],
                "compositionSettings":{"canvasWidth":320,"canvasHeight":180,"backgroundColorHex":"000000"},
                "resources":[
                  {"id":"M","kind":"audio","filename":"music.wav","displayName":"m","addedAt":0,"duration":8,
                   "imageCuts":[],"disabledAudioTrackIndices":[],
                   "mediaCuts":[{"id":"C","name":"cut","trimStart":4,"trimEnd":5}]},
                  {"id":"V","kind":"audio","filename":"voice.wav","displayName":"v","addedAt":0,"duration":2,
                   "volume":0,"imageCuts":[],"disabledAudioTrackIndices":[]}],
                "layers":[
                  {"id":"BG","name":"bg","sortIndex":0,"kind":"background","isEnabled":true,"startTime":0,"duration":11,"keyframes":[]},
                  {"id":"L1","name":"music","sortIndex":1,"kind":"audio","isEnabled":true,"startTime":0,"duration":8,"resourceID":"M",
                   "keyframes":[{"id":"K0","time":0,"gain":1,"transitionDuration":0},{"id":"K1","time":6,"gain":0.25,"transitionDuration":0}]},
                  {"id":"L2","name":"voice","sortIndex":2,"kind":"audio","isEnabled":true,"startTime":2,"duration":2,"resourceID":"V",
                   "audioFocus":true,"keyframes":[]},
                  {"id":"L3","name":"cut","sortIndex":3,"kind":"audio","isEnabled":true,"startTime":9,"resourceID":"M","mediaCutID":"C","keyframes":[]}
                ]}"#,
        )
        .expect("metadata");
        let project = Project::open(&dir).expect("opens");
        let audio = build_soundtrack(&project, 11.0)
            .expect("mixes")
            .expect("has sound");
        let full = rms(&audio, 1.0, 1.5);
        assert!(full > 0.01, "music plays at full level: {full}");
        let ducked = rms(&audio, 3.0, 3.5);
        assert!(
            (ducked / full - DUCK_FACTOR_FOR_TEST).abs() < 0.02,
            "under the focused voice the music sits at the duck factor: {ducked} / {full}"
        );
        let back = rms(&audio, 5.0, 5.5);
        assert!(
            (back / full - 1.0).abs() < 0.02,
            "and comes back after it: {back} / {full}"
        );
        let tapered = rms(&audio, 7.0, 7.5);
        assert!(
            (tapered / full - 0.0625).abs() < 0.01,
            "a 0.25 gain keyframe lands at 0.25² through the taper: {tapered} / {full}"
        );
        let gap = rms(&audio, 8.2, 8.8);
        assert!(
            gap < 0.001,
            "nothing plays between the music and the cut: {gap}"
        );
        let cut = rms(&audio, 9.2, 9.8);
        assert!(
            (cut / full - 1.0).abs() < 0.02,
            "the cut plays its slice at 9s: {cut} / {full}"
        );
        let after = rms(&audio, 10.2, 10.8);
        assert!(after < 0.001, "and only its one second: {after}");
    }

    const DUCK_FACTOR_FOR_TEST: f32 = promo_timeline::DUCK_FACTOR;

    /// Builds a project with three sequential clips over one generated file.
    fn three_clip_project(dir: &std::path::Path) -> Option<Project> {
        std::fs::create_dir_all(dir.join("Resources")).ok()?;
        let clip = dir.join("Resources/clip.mp4");
        let ok = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x120:rate=30:duration=2",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&clip)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return None;
        }
        let mut layers = String::new();
        let mut resources = String::new();
        for i in 0..3 {
            if i > 0 {
                layers.push(',');
                resources.push(',');
            }
            resources.push_str(&format!(
                r#"{{"id":"R{i}","kind":"video","filename":"clip.mp4",
                     "displayName":"c","addedAt":0,"duration":2,
                     "imageCuts":[],"disabledAudioTrackIndices":[]}}"#
            ));
            layers.push_str(&format!(
                r#"{{"id":"V{i}","name":"v","sortIndex":{i},"kind":"video",
                     "isEnabled":true,"startTime":{},"duration":2,"resourceID":"R{i}",
                     "keyframes":[{{"id":"K{i}","time":0,"zoom":1,"transitionDuration":0}}]}}"#,
                i * 2
            ));
        }
        let json = format!(
            r#"{{"id":"AAAAAAAA-0000-0000-0000-0000000000L1","name":"lifetime",
                 "createdAt":0,"state":"recorded","trimStart":0,"trimEnd":0,
                 "videoDuration":6,"subtitles":[],
                 "compositionSettings":{{"canvasWidth":160,"canvasHeight":120,
                   "backgroundColorHex":"000000"}},
                 "resources":[{resources}],"layers":[{layers}]}}"#
        );
        std::fs::write(dir.join("metadata.json"), json).ok()?;
        Project::open(dir).ok()
    }

    /// A layer aimed at an image cut draws the cut's own staged file — the
    /// Mac app's contract, which the portable path used to ignore (it
    /// honoured only the cut's frame, so a crop rendered as the whole
    /// picture). Red source, blue crop: the pointed layer must come out
    /// blue, and dropping the pointer via set_project brings red back.
    /// `imageOrientation` is a LAYER field the parser read and every
    /// headless render ignored: a photo turned right in the app rendered
    /// upright from the CLI. A red-over-blue picture turned right puts red
    /// on the right — sampled left and right of centre.
    #[test]
    fn a_layers_image_orientation_turns_its_picture() {
        let dir = std::env::temp_dir().join("promo-cli-orientation");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        let mut img = image::RgbaImage::from_pixel(8, 16, image::Rgba([0, 0, 255, 255]));
        for y in 0..8 {
            for x in 0..8 {
                img.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
            }
        }
        img.save(dir.join("Resources/tall.png")).unwrap();
        let json = |orientation: &str| {
            format!(
                r#"{{"id":"AAAAAAAA-0000-0000-0000-0000000000OR","name":"turn",
            "createdAt":0,"state":"recorded","trimStart":0,"trimEnd":0,
            "videoDuration":2,"subtitles":[],
            "compositionSettings":{{"canvasWidth":160,"canvasHeight":120,
              "backgroundColorHex":"00FF00"}},
            "resources":[{{"id":"R","kind":"image","filename":"tall.png",
              "displayName":"Tall","addedAt":0,"pixelWidth":8,"pixelHeight":16,"imageCuts":[]}}],
            "layers":[{{"id":"L","name":"Tall","sortIndex":0,"kind":"image",
              "isEnabled":true,"startTime":0.0,"duration":2.0,"resourceID":"R",
              "imageOrientation":"{orientation}",
              "keyframes":[{{"id":"K","time":0.0,"transitionDuration":0.0,
                "placement":{{"mode":"fill","anchor":"center"}}}}]}}]}}"#
            )
        };
        std::fs::write(dir.join("metadata.json"), json("rotateRight")).unwrap();
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let project = crate::project::Project::open(&dir).expect("project");
        // Staged first: the turned copy sits under the layer's key, 16 wide.
        let (_, cuts, _) = Renderer::stage(&project, &Registry::with_defaults()).expect("stage");
        let (base, _, w, h, _) = cuts.get("L").expect("the turned copy is staged by layer");
        assert_eq!((base.as_str(), *w, *h), ("R", 16, 8));
        let mut renderer = Renderer::new(&project, 160, 120).expect("renderer");
        let sample = |px: &[u8], x: usize| {
            let at = (60 * 160 + x) * 4;
            (px[at], px[at + 2]) // (b, r)
        };
        let frame = renderer.frame_bgra(1.0).expect("frame");
        let (lb, lr) = sample(&frame, 40);
        let (rb, rr) = sample(&frame, 120);
        assert!(
            lb > 200 && lr < 40,
            "turned right, blue is on the left: b={lb} r={lr}"
        );
        assert!(rr > 200 && rb < 40, "and red on the right: b={rb} r={rr}");

        std::fs::write(dir.join("metadata.json"), json("original")).unwrap();
        let upright = crate::project::Project::open(&dir).expect("project");
        renderer.set_project(&upright).expect("set_project");
        let frame = renderer.frame_bgra(1.0).expect("frame");
        let at = (20 * 160 + 80) * 4;
        let top = (frame[at], frame[at + 2]);
        let at = (100 * 160 + 80) * 4;
        let bottom = (frame[at], frame[at + 2]);
        assert!(top.1 > 200 && top.0 < 40, "upright, red is on top: {top:?}");
        assert!(
            bottom.0 > 200 && bottom.1 < 40,
            "and blue below: {bottom:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_image_cut_layer_draws_the_cut_file() {
        let dir = std::env::temp_dir().join("promo-cli-imagecut");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        let paint = |name: &str, rgba: [u8; 4]| {
            let img = image::RgbaImage::from_pixel(8, 8, image::Rgba(rgba));
            img.save(dir.join("Resources").join(name)).unwrap();
        };
        paint("red.png", [255, 0, 0, 255]);
        paint("crop.png", [0, 0, 255, 255]);
        let json = r#"{"id":"AAAAAAAA-0000-0000-0000-0000000000IC","name":"cut",
            "createdAt":0,"state":"recorded","trimStart":0,"trimEnd":0,
            "videoDuration":2,"subtitles":[],
            "compositionSettings":{"canvasWidth":160,"canvasHeight":120,
              "backgroundColorHex":"00FF00"},
            "resources":[{"id":"R","kind":"image","filename":"red.png",
              "displayName":"Red","addedAt":0,
              "imageCuts":[{"id":"IC","rect":[[0.25,0.25],[0.5,0.5]],
                "filename":"crop.png","createdAt":0}]}],
            "layers":[{"id":"L","name":"Red","sortIndex":0,"kind":"image",
              "isEnabled":true,"startTime":0.0,"duration":2.0,"resourceID":"R",
              "imageCutID":"IC",
              "keyframes":[{"id":"K","time":0.0,"transitionDuration":0.0,
                "placement":{"mode":"fill","anchor":"center"}}]}]}"#;
        std::fs::write(dir.join("metadata.json"), json).unwrap();
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let project = crate::project::Project::open(&dir).expect("project");
        let mut renderer = Renderer::new(&project, 160, 120).expect("renderer");
        let center = |px: &[u8]| {
            let at = (60 * 160 + 80) * 4;
            (px[at], px[at + 2]) // (b, r)
        };
        let (b, r) = center(&renderer.frame_bgra(1.0).expect("cut frame"));
        assert!(
            b > 200 && r < 40,
            "the cut layer should draw the blue crop file, got b={b} r={r}"
        );

        std::fs::write(
            dir.join("metadata.json"),
            json.replace(r#""imageCutID":"IC","#, ""),
        )
        .unwrap();
        let uncut = crate::project::Project::open(&dir).expect("project");
        renderer.set_project(&uncut).expect("set_project");
        let (b, r) = center(&renderer.frame_bgra(1.0).expect("uncut frame"));
        assert!(
            r > 200 && b < 40,
            "without the pointer the source shows again, got b={b} r={r}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A drawing layer renders on the PORTABLE path — the engine's own
    /// wgpu rasterizer, no Apple anywhere. This failed before the device
    /// carried TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES: the vector
    /// pipeline asked for 8x MSAA on the adapter's word alone, which
    /// Metal allowed and D3D12 refused at pipeline creation.
    #[test]
    fn a_drawing_layer_renders_portably() {
        let dir = std::env::temp_dir().join("promo-cli-drawing");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let json = r#"{"id":"AAAAAAAA-0000-0000-0000-0000000000DR","name":"ink",
            "createdAt":0,"state":"recorded","trimStart":0,"trimEnd":0,
            "videoDuration":2,"subtitles":[],
            "compositionSettings":{"canvasWidth":160,"canvasHeight":120,
              "backgroundColorHex":"000000"},
            "resources":[{"id":"D","kind":"drawing","filename":"",
              "displayName":"Ink","addedAt":0,
              "drawing":{"shapes":[{"id":"S","kind":"oval",
                "points":[[0,0],[400,300]],
                "strokeColorHex":"FF0000","strokeWidth":8,
                "fillColorHex":"FF0000",
                "arrowStart":false,"arrowEnd":false}]}}],
            "layers":[{"id":"L","name":"Ink","sortIndex":0,"kind":"drawing",
              "isEnabled":true,"startTime":0.0,"duration":2.0,"resourceID":"D",
              "keyframes":[{"id":"K","time":0.0,"transitionDuration":0.0,
                "placement":{"mode":"fit","anchor":"center"}}]}]}"#;
        std::fs::write(dir.join("metadata.json"), json).unwrap();
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let project = crate::project::Project::open(&dir).expect("project");
        let mut renderer = Renderer::new(&project, 160, 120).expect("renderer");
        let pixels = renderer.frame_bgra(1.0).expect("drawing frame");
        let at = (60 * 160 + 80) * 4;
        assert!(
            pixels[at + 2] > 180 && pixels[at] < 60,
            "the filled oval's ink should reach the frame, got b={} r={}",
            pixels[at],
            pixels[at + 2]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// B3.1: `auto` reads a proxy only when one is built AND the output
    /// fits it; `off` never; `on` builds it. The staged path says which.
    #[test]
    fn proxy_policy_picks_the_proxy_only_when_small_and_told_to() {
        if std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| !s.success())
            .unwrap_or(true)
        {
            eprintln!("ffmpeg unavailable; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-proxy-policy-{}", std::process::id()));
        let cache = dir.join("cache");
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        std::env::set_var("PROMO_PROXY_DIR", &cache);
        let clip = dir.join("Resources/clip.mp4");
        assert!(std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-nostdin",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=1280x720:rate=30:duration=1",
                "-pix_fmt",
                "yuv420p"
            ])
            .arg(&clip)
            .status()
            .unwrap()
            .success());
        std::fs::write(
            dir.join("metadata.json"),
            r#"{"id":"P","name":"Proxy","createdAt":0,"state":"recorded","trimStart":0,"trimEnd":1,
            "videoDuration":1,"subtitles":[],"compositionSettings":{"canvasWidth":1280,"canvasHeight":720},
            "resources":[{"id":"V","kind":"video","filename":"clip.mp4","displayName":"clip","addedAt":0,
              "duration":1,"imageCuts":[],"disabledAudioTrackIndices":[]}],
            "layers":[{"id":"L","name":"clip","sortIndex":0,"kind":"video","isEnabled":true,
              "startTime":0,"duration":1,"resourceID":"V","keyframes":[]}]}"#,
        )
        .unwrap();
        let project = crate::project::Project::open(&dir).expect("project");
        let registry = Registry::with_defaults();
        let staged = |policy: ProxyPolicy, long_edge: u32| -> (bool, PathBuf) {
            let (_, _, videos) =
                Renderer::stage_with(&project, &registry, policy, long_edge).expect("stage");
            let v = videos.get("L").expect("staged by layer");
            (v.proxied, v.path.clone())
        };
        // Nothing built yet: auto reads the source.
        assert!(!staged(ProxyPolicy::Auto, 640).0);
        // On builds it, and uses it for a small output.
        let (used, path) = staged(ProxyPolicy::On, 640);
        assert!(used && path.starts_with(&cache), "{}", path.display());
        // Now auto finds it — for a small output only.
        assert!(staged(ProxyPolicy::Auto, 640).0);
        assert!(
            !staged(ProxyPolicy::Auto, 1280).0,
            "a full-size render reads the source"
        );
        // Off never.
        assert!(!staged(ProxyPolicy::Off, 640).0);
        std::env::remove_var("PROMO_PROXY_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// B3.4: a chroma key cuts the plate and keeps the subject — the plate's
    /// pixels show the background, the subject's stay their colour, the
    /// unkeyed twin shows the plate.
    #[test]
    fn a_chroma_key_cuts_the_plate_and_keeps_the_subject() {
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-key-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        // A green plate with a red square in the middle.
        let (w, h) = (64u32, 48u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let i = (y * w as usize + x) * 4;
                let red = (20..44).contains(&x) && (12..36).contains(&y);
                rgba[i..i + 4].copy_from_slice(if red {
                    &[220, 30, 30, 255]
                } else {
                    &[0, 255, 0, 255]
                });
            }
        }
        write_png(&dir.join("Resources/plate.png"), &rgba, w, h).unwrap();
        let doc = |keyed: bool| {
            format!(
                r#"{{"id":"P","name":"Key","createdAt":0,"state":"recorded","minReaderVersion":22,
                "trimStart":0,"trimEnd":2,"videoDuration":2,"subtitles":[],
                "compositionSettings":{{"canvasWidth":64,"canvasHeight":48,"backgroundColorHex":"1020C0"}},
                "resources":[{{"id":"I","kind":"image","filename":"plate.png","displayName":"plate","addedAt":0,
                  "pixelWidth":64,"pixelHeight":48,"imageCuts":[],"disabledAudioTrackIndices":[]}}],
                "layers":[{{"id":"L","name":"plate","sortIndex":0,"kind":"image","isEnabled":true,
                  "startTime":0,"duration":2,"resourceID":"I"{key},"keyframes":[]}}]}}"#,
                key = if keyed {
                    r#","chromaKey":{"colorHex":"00FF00","tolerance":0.25,"softness":0.05}"#
                } else {
                    ""
                }
            )
        };
        let render = |json: &str| -> Vec<u8> {
            std::fs::write(dir.join("metadata.json"), json).unwrap();
            let project = crate::project::Project::open(&dir).expect("project");
            let mut renderer = Renderer::new(&project, 64, 48).expect("renderer");
            renderer.frame_bgra(1.0).expect("frame")
        };
        let px = |frame: &[u8], x: usize, y: usize| -> [u8; 3] {
            let i = (y * 64 + x) * 4;
            [frame[i + 2], frame[i + 1], frame[i]] // r, g, b
        };
        let keyed = render(&doc(true));
        let plain = render(&doc(false));
        let corner_keyed = px(&keyed, 4, 4);
        let corner_plain = px(&plain, 4, 4);
        assert!(
            corner_plain[1] > 200 && corner_plain[0] < 40,
            "unkeyed shows the plate: {corner_plain:?}"
        );
        assert!(
            corner_keyed[2] > 150 && corner_keyed[1] < 60,
            "keyed shows the background through the plate: {corner_keyed:?}"
        );
        let centre = px(&keyed, 32, 24);
        assert!(
            centre[0] > 180 && centre[1] < 70,
            "the red subject survives the key: {centre:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Image effects (rung 24) through a real project: a blur softens the
    /// plate's hard edge, a keyframed blur ramps from sharp, a vignette
    /// darkens the plate's corner, a glow spills light past the bright
    /// half, grain speckles a flat field — and the twin without effects
    /// is untouched.
    #[test]
    fn image_effects_render_through_the_project() {
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-fx-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        // White on the left half, black on the right.
        let (w, h) = (64u32, 48u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let i = (y * w as usize + x) * 4;
                let v = if x < 32 { 255 } else { 0 };
                rgba[i..i + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        write_png(&dir.join("Resources/plate.png"), &rgba, w, h).unwrap();
        let doc = |effects: &str, keyframes: &str| {
            format!(
                r#"{{"id":"P","name":"Fx","createdAt":0,"state":"recorded","minReaderVersion":24,
                "trimStart":0,"trimEnd":3,"videoDuration":3,"subtitles":[],
                "compositionSettings":{{"canvasWidth":64,"canvasHeight":48,"backgroundColorHex":"000000"}},
                "resources":[{{"id":"I","kind":"image","filename":"plate.png","displayName":"plate","addedAt":0,
                  "pixelWidth":64,"pixelHeight":48,"imageCuts":[],"disabledAudioTrackIndices":[]}}],
                "layers":[{{"id":"L","name":"plate","sortIndex":0,"kind":"image","isEnabled":true,
                  "startTime":0,"duration":3,"resourceID":"I"{effects},"keyframes":[{keyframes}]}}]}}"#
            )
        };
        let render = |json: &str, at: f64| -> Vec<u8> {
            std::fs::write(dir.join("metadata.json"), json).unwrap();
            let project = crate::project::Project::open(&dir).expect("project");
            let mut renderer = Renderer::new(&project, 64, 48).expect("renderer");
            renderer.frame_bgra(at).expect("frame")
        };
        let lum = |frame: &[u8], x: usize, y: usize| -> u8 { frame[(y * 64 + x) * 4 + 1] };

        let plain = render(&doc("", ""), 1.0);
        assert!(
            lum(&plain, 30, 24) > 240 && lum(&plain, 33, 24) < 10,
            "the twin is a hard edge"
        );

        let blurred = render(&doc(r#","effects":{"blur":8}"#, ""), 1.0);
        assert!(
            lum(&blurred, 33, 24) > 25,
            "blur spills white past the edge: {}",
            lum(&blurred, 33, 24)
        );
        assert!(
            lum(&blurred, 30, 24) < 230,
            "and black into the white: {}",
            lum(&blurred, 30, 24)
        );
        assert!(lum(&blurred, 3, 24) > 240, "the far field is untouched");

        let ramp = render(
            &doc(
                "",
                r#"{"id":"K1","time":0,"blur":0,"transitionDuration":0},{"id":"K2","time":2,"blur":12,"transitionDuration":2}"#,
            ),
            0.0,
        );
        assert!(lum(&ramp, 33, 24) < 10, "keyframed blur starts sharp");
        let ramp_end = render(
            &doc(
                "",
                r#"{"id":"K1","time":0,"blur":0,"transitionDuration":0},{"id":"K2","time":2,"blur":12,"transitionDuration":2}"#,
            ),
            2.0,
        );
        assert!(
            lum(&ramp_end, 33, 24) > 25,
            "and arrives blurred: {}",
            lum(&ramp_end, 33, 24)
        );

        let vignetted = render(&doc(r#","effects":{"vignette":1.0}"#, ""), 1.0);
        assert!(
            lum(&vignetted, 2, 2) < 80,
            "the corner darkens: {}",
            lum(&vignetted, 2, 2)
        );
        assert!(
            lum(&vignetted, 20, 24) > 150,
            "nearer the centre it does not: {}",
            lum(&vignetted, 20, 24)
        );

        let glowing = render(
            &doc(
                r#","effects":{"glow":1.0,"glowRadius":10,"glowThreshold":0.5}"#,
                "",
            ),
            1.0,
        );
        assert!(
            lum(&glowing, 36, 24) > 20,
            "the glow spills past the bright half: {}",
            lum(&glowing, 36, 24)
        );

        let grainy = render(&doc(r#","effects":{"grain":1.0}"#, ""), 1.0);
        let samples: Vec<u8> = (0..16).map(|i| lum(&grainy, 2 + i, 10 + i)).collect();
        assert!(
            samples.iter().any(|&v| v < 250),
            "grain speckles the white: {samples:?}"
        );
        assert!(
            samples.iter().all(|&v| v >= 200),
            "within bounds: {samples:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A stage (rung 30): two cubes, red at depth +1 and blue at -1, seen
    /// straight on through the first member's camera — the near one is
    /// what the middle shows, and swapping the depths swaps it; a green
    /// picture member at +2.5 (clear of the near cube's front face at
    /// its depth plus half its side) stands in front of both.
    #[test]
    fn a_stage_orders_its_members_by_depth() {
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-stage-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        std::fs::write(
            dir.join("Resources").join("red.glb"),
            promo_engine::model::sample_cube_glb_with(0.5, "Body", [0.9, 0.1, 0.1, 1.0]),
        )
        .unwrap();
        std::fs::write(
            dir.join("Resources").join("blue.glb"),
            promo_engine::model::sample_cube_glb_with(0.5, "Body", [0.1, 0.1, 0.9, 1.0]),
        )
        .unwrap();
        let green = image::RgbaImage::from_pixel(64, 64, image::Rgba([20, 220, 60, 255]));
        green.save(dir.join("Resources").join("green.png")).unwrap();
        let doc = |red_depth: f64, blue_depth: f64, picture: &str| {
            format!(
                r#"{{"id":"P","name":"Stage","createdAt":0,"state":"recorded","minReaderVersion":30,
                "trimStart":0,"trimEnd":2,"videoDuration":2,"subtitles":[],
                "compositionSettings":{{"canvasWidth":320,"canvasHeight":320,"backgroundColorHex":"000000"}},
                "resources":[
                  {{"id":"R","kind":"model","filename":"red.glb","displayName":"Red","addedAt":0}},
                  {{"id":"B","kind":"model","filename":"blue.glb","displayName":"Blue","addedAt":0}},
                  {{"id":"G","kind":"image","filename":"green.png","displayName":"Green","addedAt":0,"pixelWidth":64,"pixelHeight":64}}],
                "layers":[
                  {{"id":"L1","name":"red","sortIndex":0,"kind":"model","isEnabled":true,"stage":"s",
                    "startTime":0,"duration":2,"resourceID":"R",
                    "keyframes":[{{"id":"K1","time":0,"camera":{{"yaw":0,"pitch":0,"distance":3.5}},"depth":{red_depth},"transitionDuration":0}}]}},
                  {{"id":"L2","name":"blue","sortIndex":1,"kind":"model","isEnabled":true,"stage":"s",
                    "startTime":0,"duration":2,"resourceID":"B",
                    "keyframes":[{{"id":"K2","time":0,"depth":{blue_depth},"transitionDuration":0}}]}}{picture}]}}"#
            )
        };
        let centre = |json: &str| -> (u8, u8, u8) {
            std::fs::write(dir.join("metadata.json"), json).unwrap();
            let project = crate::project::Project::open(&dir).expect("project");
            let mut renderer = Renderer::new(&project, 320, 320).expect("renderer");
            let frame = renderer.frame_bgra(0.5).expect("frame");
            let i = (160 * 320 + 160) * 4;
            (frame[i + 2], frame[i + 1], frame[i])
        };
        let (r, g, b) = centre(&doc(1.0, -1.0, ""));
        assert!(r > 100 && r > b + 60, "red in front: {r},{g},{b}");
        let (r, g, b) = centre(&doc(-1.0, 1.0, ""));
        assert!(
            b > 100 && b > r + 60,
            "blue in front after the swap: {r},{g},{b}"
        );
        let picture = r#",{"id":"L3","name":"green","sortIndex":2,"kind":"image","isEnabled":true,"stage":"s",
            "startTime":0,"duration":2,"resourceID":"G",
            "keyframes":[{"id":"K3","time":0,"zoom":0.6,"depth":2.5,"transitionDuration":0}]}"#;
        let (r, g, b) = centre(&doc(1.0, -1.0, picture));
        assert!(
            g > 120 && g > r + 60 && g > b + 60,
            "the picture stands in front of both: {r},{g},{b}"
        );
        // A caption member in front of everything: white glyph pixels reach
        // the middle band, and the red cube shows between its glyphs.
        let caption = r#",{"id":"L4","name":"words","sortIndex":3,"kind":"caption","isEnabled":true,"stage":"s",
            "startTime":0,"duration":2,"captionText":"IIIIIIII",
            "captionStyle":{"alignment":"center","subtitleFontSize":120,"subtitleColorHex":"FFFFFF","subtitleBackgroundOpacity":0},
            "keyframes":[{"id":"K4","time":0,"depth":3.0,"transitionDuration":0}]}"#;
        std::fs::write(dir.join("metadata.json"), doc(1.0, -1.0, caption)).unwrap();
        let project = crate::project::Project::open(&dir).expect("project");
        let mut renderer = Renderer::new(&project, 320, 320).expect("renderer");
        let frame = renderer.frame_bgra(0.5).expect("frame");
        let count = |keep: &dyn Fn(u8, u8, u8) -> bool| -> usize {
            (120..200)
                .flat_map(|y| (60..260).map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    let i = (y * 320 + x) * 4;
                    keep(frame[i + 2], frame[i + 1], frame[i])
                })
                .count()
        };
        let white = count(&|r, g, b| r > 200 && g > 200 && b > 200);
        assert!(
            white > 200,
            "the caption stands in front, white on the scene: {white} px"
        );
        let red = count(&|r, g, b| r > 120 && g < 80 && b < 80);
        assert!(red > 200, "and the cube shows between its glyphs: {red} px");
        // A drawing member — a filled green oval — in front the same way.
        let drawing = r#"{"id":"D","kind":"drawing","filename":"","displayName":"Oval","addedAt":0,
            "pixelWidth":320,"pixelHeight":320,
            "drawing":{"shapes":[{"id":"S","kind":"oval","points":[[60,60],[260,260]],
              "strokeColorHex":"10E040","strokeWidth":4,"fillColorHex":"10E040","fillOpacity":1,
              "arrowStart":false,"arrowEnd":false}]}},"#;
        let json = doc(1.0, -1.0, r#",{"id":"L5","name":"oval","sortIndex":4,"kind":"drawing","isEnabled":true,"stage":"s",
            "startTime":0,"duration":2,"resourceID":"D",
            "keyframes":[{"id":"K5","time":0,"depth":3.0,"transitionDuration":0}]}"#)
            .replacen(r#""resources":["#, &format!(r#""resources":[{drawing}"#), 1);
        std::fs::write(dir.join("metadata.json"), json).unwrap();
        let project = crate::project::Project::open(&dir).expect("project");
        let mut renderer = Renderer::new(&project, 320, 320).expect("renderer");
        let frame = renderer.frame_bgra(0.5).expect("frame");
        let i = (160 * 320 + 160) * 4;
        let (r, g, b) = (frame[i + 2], frame[i + 1], frame[i]);
        assert!(
            g > 120 && g > r + 60 && g > b + 60,
            "the drawing stands in front: {r},{g},{b}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Across the stage (rung 31): the red cube offset right and the other
    /// left at the same depth, the scene placed 300 wide — the right of the
    /// frame is red, the left the other cube's bound green, the middle
    /// the ground.
    #[test]
    fn a_stage_places_members_side_by_side() {
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-across-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        std::fs::write(
            dir.join("Resources").join("red.glb"),
            promo_engine::model::sample_cube_glb_with(0.5, "Body", [0.9, 0.1, 0.1, 1.0]),
        )
        .unwrap();
        std::fs::write(
            dir.join("Resources").join("blue.glb"),
            promo_engine::model::sample_cube_glb_with(0.5, "Body", [0.1, 0.1, 0.9, 1.0]),
        )
        .unwrap();
        let doc = r#"{"id":"P","name":"Across","createdAt":0,"state":"recorded","minReaderVersion":31,
            "trimStart":0,"trimEnd":2,"videoDuration":2,"subtitles":[],
            "compositionSettings":{"canvasWidth":320,"canvasHeight":320,"backgroundColorHex":"000000"},
            "resources":[
              {"id":"R","kind":"model","filename":"red.glb","displayName":"Red","addedAt":0},
              {"id":"B","kind":"model","filename":"blue.glb","displayName":"Blue","addedAt":0}],
            "layers":[
              {"id":"L1","name":"red","sortIndex":0,"kind":"model","isEnabled":true,"stage":"s",
                "startTime":0,"duration":2,"resourceID":"R",
                "keyframes":[{"id":"K1","time":0,"camera":{"yaw":0,"pitch":0,"distance":3.0},"stageOffset":[1.6,0],
                  "placement":{"width":300,"anchor":"center"},"transitionDuration":0}]},
              {"id":"L2","name":"green","sortIndex":1,"kind":"model","isEnabled":true,"stage":"s",
                "startTime":0,"duration":2,"resourceID":"B",
                "keyframes":[{"id":"K2","time":0,"stageOffset":[-1.6,0],"transitionDuration":0}]}]}"#
            .replace(
                r#""filename":"blue.glb","displayName":"Blue","addedAt":0}"#,
                r#""filename":"blue.glb","displayName":"Blue","addedAt":0,"materials":{"Body":"10E040"}}"#,
            );
        std::fs::write(dir.join("metadata.json"), &doc).unwrap();
        let project = crate::project::Project::open(&dir).expect("project");
        let mut renderer = Renderer::new(&project, 320, 320).expect("renderer");
        let frame = renderer.frame_bgra(0.5).expect("frame");
        let strip = |x0: usize, x1: usize| -> (u64, u64, u64) {
            let (mut r, mut b, mut n) = (0u64, 0u64, 0u64);
            for y in 120..200 {
                for x in x0..x1 {
                    let i = (y * 320 + x) * 4;
                    r += frame[i + 2] as u64;
                    b += frame[i] as u64;
                    n += 1;
                }
            }
            (r / n, b / n, n)
        };
        let (r, b, _) = strip(220, 300);
        assert!(r > 60 && r > b * 2, "the right is red: r {r} b {b}");
        let (r, b, _) = strip(20, 100);
        assert!(
            b < 60 && r < 60,
            "the left cube took its binding, not its file colour: r {r} b {b}"
        );
        let g = {
            let mut g = 0u64;
            let mut n = 0u64;
            for y in 120..200 {
                for x in 20..100 {
                    g += frame[(y * 320 + x) * 4 + 1] as u64;
                    n += 1;
                }
            }
            g / n
        };
        assert!(g > 60, "the left is the bound green: g {g}");
        // The left member keyed with its own camera yaw turns in place: two
        // faces, two shades on its side of the frame; the right stays one.
        let turned = doc.replace(
            r#""keyframes":[{"id":"K2","time":0,"stageOffset":[-1.6,0],"transitionDuration":0}]"#,
            r#""keyframes":[{"id":"K2","time":0,"stageOffset":[-1.6,0],"camera":{"yaw":40},"transitionDuration":0}]"#,
        );
        assert_ne!(turned, doc, "the member keyframe was rewritten");
        std::fs::write(dir.join("metadata.json"), &turned).unwrap();
        let project = crate::project::Project::open(&dir).expect("project");
        let mut renderer = Renderer::new(&project, 320, 320).expect("renderer");
        let frame = renderer.frame_bgra(0.5).expect("frame");
        let spread = |x0: usize, x1: usize| -> u32 {
            let mut lum: Vec<u32> = (120..200)
                .flat_map(|y| (x0..x1).map(move |x| (x, y)))
                .map(|(x, y)| {
                    let i = (y * 320 + x) * 4;
                    (frame[i + 2] as u32 * 299 + frame[i + 1] as u32 * 587 + frame[i] as u32 * 114)
                        / 1000
                })
                .filter(|&l| l > 30)
                .collect();
            lum.sort_unstable();
            if lum.len() < 20 {
                return 0;
            }
            lum[lum.len() * 9 / 10] - lum[lum.len() / 10]
        };
        assert!(
            spread(0, 130) > 25,
            "the turned member shows two shades: {}",
            spread(0, 130)
        );
        assert!(
            spread(190, 320) < 25,
            "the untouched member stays one: {}",
            spread(190, 320)
        );
        let (r, b, _) = strip(150, 170);
        assert!(r < 30 && b < 30, "the middle is the ground: r {r} b {b}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Placement measures the model, not its sphere: a cube placed 200
    /// tall on a 320 canvas, seen face-on, spans about 200 rows of lit
    /// pixels.
    #[test]
    fn a_model_placement_height_is_the_models_height() {
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-modelbox-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        std::fs::write(
            dir.join("Resources").join("cube.glb"),
            promo_engine::model::sample_cube_glb(),
        )
        .unwrap();
        let doc = r#"{"id":"P","name":"Box","createdAt":0,"state":"recorded","minReaderVersion":29,
            "trimStart":0,"trimEnd":2,"videoDuration":2,"subtitles":[],
            "compositionSettings":{"canvasWidth":320,"canvasHeight":320,"backgroundColorHex":"000000"},
            "resources":[{"id":"M","kind":"model","filename":"cube.glb","displayName":"Cube","addedAt":0}],
            "layers":[{"id":"L","name":"cube","sortIndex":0,"kind":"model","isEnabled":true,
              "startTime":0,"duration":2,"resourceID":"M",
              "keyframes":[{"id":"K0","time":0,"camera":{"yaw":0,"pitch":0,"distance":6},
                "placement":{"height":200,"anchor":"center"},"transitionDuration":0}]}]}"#;
        std::fs::write(dir.join("metadata.json"), doc).unwrap();
        let project = crate::project::Project::open(&dir).expect("project");
        let mut renderer = Renderer::new(&project, 320, 320).expect("renderer");
        let frame = renderer.frame_bgra(0.5).expect("frame");
        let lit_rows = (0..320)
            .filter(|&y| (0..320).any(|x| frame[(y * 320 + x) * 4 + 2] > 40))
            .count();
        assert!(
            (180..=222).contains(&lit_rows),
            "the cube stands about 200 tall as placed: {lit_rows} rows"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A video on a slot: the slab's Screen bound to the practice clip
    /// shows a picture that differs from the slot's own dark material and
    /// changes between two moments — the recording plays on the screen.
    #[test]
    fn a_video_bound_to_a_slot_plays_on_it() {
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            eprintln!("ffmpeg unavailable; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-slotvideo-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        std::fs::write(
            dir.join("Resources").join("slab.glb"),
            promo_engine::model::sample_slab_glb(),
        )
        .unwrap();
        // Two seconds of red then two of blue, so two moments differ for
        // certain wherever the screen is sampled.
        let clip = dir.join("Resources").join("talk.mp4");
        let made = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("color=c=red:size=320x200:rate=10:duration=2")
            .args(["-f", "lavfi", "-i"])
            .arg("color=c=blue:size=320x200:rate=10:duration=2")
            .args([
                "-filter_complex",
                "[0:v][1:v]concat=n=2:v=1:a=0",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&clip)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !made {
            eprintln!("ffmpeg could not write the test clip; skipping");
            return;
        }
        let doc = |materials: &str| {
            format!(
                r#"{{"id":"P","name":"SlotVideo","createdAt":0,"state":"recorded","minReaderVersion":29,
                "trimStart":0,"trimEnd":4,"videoDuration":4,"subtitles":[],
                "compositionSettings":{{"canvasWidth":320,"canvasHeight":320,"backgroundColorHex":"000000"}},
                "resources":[
                  {{"id":"V","kind":"video","filename":"talk.mp4","displayName":"Talk","addedAt":0,"duration":4,
                    "imageCuts":[],"disabledAudioTrackIndices":[]}},
                  {{"id":"M","kind":"model","filename":"slab.glb","displayName":"Slab","addedAt":0{materials}}}],
                "layers":[{{"id":"L","name":"slab","sortIndex":0,"kind":"model","isEnabled":true,
                  "startTime":0,"duration":4,"resourceID":"M",
                  "keyframes":[{{"id":"K0","time":0,"camera":{{"yaw":0,"pitch":0,"distance":3.0}},"transitionDuration":0}}]}}]}}"#
            )
        };
        let screen = |json: &str, time: f64| -> Vec<u8> {
            std::fs::write(dir.join("metadata.json"), json).unwrap();
            let project = crate::project::Project::open(&dir).expect("project");
            let mut renderer = Renderer::new(&project, 320, 320).expect("renderer");
            let frame = renderer.frame_bgra(time).expect("frame");
            // The middle 40×40 of the frame: inside the screen, seen straight on.
            (140..180)
                .flat_map(|y| (140..180).map(move |x| (x, y)))
                .flat_map(|(x, y)| frame[(y * 320 + x) * 4..(y * 320 + x) * 4 + 3].to_vec())
                .collect()
        };
        let dark = screen(&doc(""), 0.5);
        let bound = doc(r#","materials":{"Screen":{"resourceID":"V"}}"#);
        let early = screen(&bound, 0.5);
        let late = screen(&bound, 2.5);
        let differ = |a: &[u8], b: &[u8]| -> usize {
            a.iter()
                .zip(b)
                .filter(|(x, y)| (**x as i32 - **y as i32).abs() > 24)
                .count()
        };
        assert!(
            differ(&early, &dark) > early.len() / 4,
            "the recording shows on the screen"
        );
        assert!(
            differ(&early, &late) > early.len() / 20,
            "and it moves between moments"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A clip: the turning cube's `Turn` scrubbed to 0.5 s shows two
    /// faces where clip time 0 shows one; with no time keyed the clip runs
    /// on layer time, so the layer at 0.5 s shows the same two faces.
    #[test]
    fn a_clip_keyframe_poses_the_model() {
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-clip-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        std::fs::write(
            dir.join("Resources").join("cube.glb"),
            promo_engine::model::sample_turning_cube_glb(),
        )
        .unwrap();
        let doc = |clip: &str| {
            format!(
                r#"{{"id":"P","name":"Clip","createdAt":0,"state":"recorded","minReaderVersion":29,
                "trimStart":0,"trimEnd":2,"videoDuration":2,"subtitles":[],
                "compositionSettings":{{"canvasWidth":320,"canvasHeight":320,"backgroundColorHex":"000000"}},
                "resources":[{{"id":"M","kind":"model","filename":"cube.glb","displayName":"Cube","addedAt":0}}],
                "layers":[{{"id":"L","name":"cube","sortIndex":0,"kind":"model","isEnabled":true,
                  "startTime":0,"duration":2,"resourceID":"M",
                  "keyframes":[{{"id":"K0","time":0,"camera":{{"yaw":0,"pitch":0}}{clip},"transitionDuration":0}}]}}]}}"#
            )
        };
        let spread = |json: &str, time: f64| -> u32 {
            std::fs::write(dir.join("metadata.json"), json).unwrap();
            let project = crate::project::Project::open(&dir).expect("project");
            let mut renderer = Renderer::new(&project, 320, 320).expect("renderer");
            let frame = renderer.frame_bgra(time).expect("frame");
            let mut lum: Vec<u32> = frame
                .chunks_exact(4)
                .filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 40)
                .map(|p| (p[2] as u32 * 299 + p[1] as u32 * 587 + p[0] as u32 * 114) / 1000)
                .collect();
            lum.sort_unstable();
            let n = lum.len();
            assert!(n > 320 * 320 / 12, "the cube is there: {n}");
            lum[n * 9 / 10] - lum[n / 10]
        };
        let rest = spread(&doc(r#","clip":{"name":"Turn","time":0}"#), 0.5);
        assert!(rest < 30, "at clip time 0 the cube is face-on: {rest}");
        let turned = spread(&doc(r#","clip":{"name":"Turn","time":0.5}"#), 0.5);
        assert!(turned > 30, "at clip time 0.5 it shows two faces: {turned}");
        let running = spread(&doc(r#","clip":{"name":"Turn"}"#), 0.5);
        assert!(
            running > 30,
            "untimed, the clip runs on layer time: {running}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A resource on a slot: the generated slab's Screen bound to a green
    /// PNG shows green where the screen is, seen straight on; the same
    /// project with the binding dropped shows the slab's own dark screen.
    #[test]
    fn a_model_slot_bound_to_an_image_shows_the_picture() {
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-slot-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        std::fs::write(
            dir.join("Resources").join("phone.glb"),
            promo_engine::model::sample_slab_glb(),
        )
        .unwrap();
        let green = image::RgbaImage::from_pixel(64, 64, image::Rgba([20, 220, 60, 255]));
        green.save(dir.join("Resources").join("shot.png")).unwrap();
        let doc = |materials: &str| {
            format!(
                r#"{{"id":"P","name":"Slot","createdAt":0,"state":"recorded","minReaderVersion":29,
                "trimStart":0,"trimEnd":2,"videoDuration":2,"subtitles":[],
                "compositionSettings":{{"canvasWidth":320,"canvasHeight":320,"backgroundColorHex":"000000"}},
                "resources":[
                  {{"id":"S","kind":"image","filename":"shot.png","displayName":"Shot","addedAt":0,"pixelWidth":64,"pixelHeight":64}},
                  {{"id":"M","kind":"model","filename":"phone.glb","displayName":"Phone","addedAt":0{materials}}}],
                "layers":[{{"id":"L","name":"phone","sortIndex":0,"kind":"model","isEnabled":true,
                  "startTime":0,"duration":2,"resourceID":"M",
                  "keyframes":[{{"id":"K0","time":0,"camera":{{"yaw":0,"pitch":0,"distance":3.0}},"transitionDuration":0}}]}}]}}"#
            )
        };
        let centre = |json: &str| -> (u8, u8, u8) {
            std::fs::write(dir.join("metadata.json"), json).unwrap();
            let project = crate::project::Project::open(&dir).expect("project");
            let mut renderer = Renderer::new(&project, 320, 320).expect("renderer");
            let frame = renderer.frame_bgra(0.5).expect("frame");
            let i = (160 * 320 + 160) * 4;
            (frame[i + 2], frame[i + 1], frame[i])
        };
        let (r, g, b) = centre(&doc(r#","materials":{"Screen":{"resourceID":"S"}}"#));
        assert!(
            g > 120 && g > r + 60 && g > b + 60,
            "the screen shows the green shot: {r},{g},{b}"
        );
        let (r, g, b) = centre(&doc(""));
        assert!(
            g < 80 && (g as i32 - r as i32).abs() < 30,
            "unbound, the screen is its own dark material: {r},{g},{b}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Models (rung 29): a generated cube glb on a model layer renders lit
    /// — face-on it is one shade, from a keyed three-quarter camera it is
    /// several with the top face brightest — and a `@accent` material
    /// binding paints it the palette's red.
    #[test]
    fn a_model_layer_renders_lit_and_takes_the_palette() {
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-model-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        std::fs::write(
            dir.join("Resources").join("cube.glb"),
            promo_engine::model::sample_cube_glb_with(0.5, "Body", [0.75, 0.75, 0.75, 1.0]),
        )
        .unwrap();
        let doc = |materials: &str| {
            format!(
                r#"{{"id":"P","name":"Model","createdAt":0,"state":"recorded","minReaderVersion":29,
                "trimStart":0,"trimEnd":2,"videoDuration":2,"subtitles":[],
                "compositionSettings":{{"canvasWidth":320,"canvasHeight":320,"backgroundColorHex":"000000",
                  "palette":[{{"name":"accent","colorHex":"E02020"}}]}},
                "resources":[{{"id":"M","kind":"model","filename":"cube.glb","displayName":"Cube","addedAt":0{materials}}}],
                "layers":[{{"id":"L","name":"cube","sortIndex":0,"kind":"model","isEnabled":true,
                  "startTime":0,"duration":2,"resourceID":"M",
                  "keyframes":[
                    {{"id":"K0","time":0,"camera":{{"yaw":0,"pitch":0}},"transitionDuration":0}},
                    {{"id":"K1","time":1,"camera":{{"yaw":35,"pitch":25}},"transitionDuration":0}}]}}]}}"#
            )
        };
        let render = |json: &str, time: f64| -> Vec<u8> {
            std::fs::write(dir.join("metadata.json"), json).unwrap();
            let project = crate::project::Project::open(&dir).expect("project");
            let mut renderer = Renderer::new(&project, 320, 320).expect("renderer");
            renderer.frame_bgra(time).expect("frame")
        };
        // Luminance of the pixels that are not the black background, read
        // between the 10th and 90th percentiles so anti-aliased edges, the
        // rim light and the contact shadow do not decide the answer.
        let lit = |frame: &[u8]| -> (usize, u32, u32) {
            let mut lum: Vec<u32> = frame
                .chunks_exact(4)
                .filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 40)
                .map(|p| (p[2] as u32 * 299 + p[1] as u32 * 587 + p[0] as u32 * 114) / 1000)
                .collect();
            lum.sort_unstable();
            let n = lum.len();
            if n == 0 {
                return (0, 0, 0);
            }
            (n, lum[n / 10], lum[n * 9 / 10])
        };
        let (n, lo, hi) = lit(&render(&doc(""), 0.0));
        assert!(n > 320 * 320 / 8, "the cube is there: {n} lit");
        assert!(hi - lo < 30, "face-on is one shade: {lo}..{hi}");
        let (_, lo, hi) = lit(&render(&doc(""), 1.0));
        assert!(
            hi - lo > 30,
            "three-quarter view shows several shades: {lo}..{hi}"
        );

        let red = render(&doc(r#","materials":{"Body":"@accent"}"#), 1.0);
        let (mut r, mut b, mut n) = (0u64, 0u64, 0u64);
        for p in red.chunks_exact(4) {
            if p[0] as u32 + p[1] as u32 + p[2] as u32 > 30 {
                r += p[2] as u64;
                b += p[0] as u64;
                n += 1;
            }
        }
        assert!(
            n > 0 && r > b * 2,
            "painted with the palette's red: r {r} vs b {b} over {n}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Kinetic reveals (rung 28): flip, tumble and slide each render a
    /// mid-walk frame that is neither empty nor finished — the arriving
    /// units are on their way — and land on the same picture a wipe does.
    #[test]
    fn kinetic_reveals_arrive_mid_walk_and_land() {
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-kinetic-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        let doc = |mode: &str| {
            format!(
                r#"{{"id":"P","name":"Kinetic","createdAt":0,"state":"recorded","minReaderVersion":28,
                "trimStart":0,"trimEnd":2,"videoDuration":2,"subtitles":[],
                "compositionSettings":{{"canvasWidth":640,"canvasHeight":360,"backgroundColorHex":"000000",
                  "subtitleFontSize":72,"subtitleBold":true,"subtitleColorHex":"FFFFFF",
                  "subtitleBackgroundOpacity":0,"subtitleVerticalMargin":120}},
                "resources":[],
                "layers":[{{"id":"L","name":"words","sortIndex":0,"kind":"caption","isEnabled":true,
                  "startTime":0,"duration":2,"captionText":"ONE TWO THREE FOUR",
                  "captionStyle":{{"alignment":"center","reveal":{{"by":"word","mode":"{mode}","seconds":1.6}}}},
                  "keyframes":[]}}]}}"#
            )
        };
        let lit = |json: &str, time: f64| -> usize {
            std::fs::write(dir.join("metadata.json"), json).unwrap();
            let project = crate::project::Project::open(&dir).expect("project");
            let mut renderer = Renderer::new(&project, 640, 360).expect("renderer");
            let frame = renderer.frame_bgra(time).expect("frame");
            frame.chunks(4).filter(|p| p[2] > 128).count()
        };
        let landed = lit(&doc("wipe"), 1.9);
        assert!(
            landed > 2000,
            "the wipe lands with the words showing: {landed}"
        );
        for mode in ["flip", "tumble", "slide"] {
            let mid = lit(&doc(mode), 0.8);
            assert!(
                mid > landed / 20 && mid < landed * 19 / 20,
                "{mode} is on its way at mid-walk: {mid} of {landed}"
            );
            let end = lit(&doc(mode), 1.9);
            assert!(
                end > landed * 9 / 10 && end < landed * 11 / 10,
                "{mode} lands on the wipe's picture: {end} vs {landed}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Caption tilt: `tiltY` keyframes on a caption layer lean it in
    /// perspective — the lit width of a wide white word narrows against
    /// its flat twin, and its near edge stays the taller one.
    #[test]
    fn a_caption_with_tilt_keyframes_leans() {
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-tilt-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        let doc = |keyframes: &str| {
            format!(
                r#"{{"id":"P","name":"Tilt","createdAt":0,"state":"recorded",
                "trimStart":0,"trimEnd":2,"videoDuration":2,"subtitles":[],
                "compositionSettings":{{"canvasWidth":640,"canvasHeight":360,"backgroundColorHex":"000000",
                  "subtitleFontSize":120,"subtitleBold":true,"subtitleColorHex":"FFFFFF",
                  "subtitleBackgroundOpacity":0,"subtitleVerticalMargin":100}},
                "resources":[],
                "layers":[{{"id":"L","name":"word","sortIndex":0,"kind":"caption","isEnabled":true,
                  "startTime":0,"duration":2,"captionText":"WWWWWW",
                  "captionStyle":{{"alignment":"center"}},"keyframes":[{keyframes}]}}]}}"#
            )
        };
        let lit = |json: &str| -> (usize, usize, usize) {
            std::fs::write(dir.join("metadata.json"), json).unwrap();
            let project = crate::project::Project::open(&dir).expect("project");
            let mut renderer = Renderer::new(&project, 640, 360).expect("renderer");
            let stride = renderer.width as usize;
            let frame = renderer.frame_bgra(1.0).expect("frame");
            let mut columns = vec![0usize; 640];
            for y in 0..360 {
                for (x, count) in columns.iter_mut().enumerate() {
                    if frame[(y * stride + x) * 4 + 2] > 128 {
                        *count += 1;
                    }
                }
            }
            let first = columns.iter().position(|&c| c > 0).unwrap_or(0);
            let last = columns.iter().rposition(|&c| c > 0).unwrap_or(0);
            // The outermost lit column of a glyph raster is a letter's own
            // edge, so the edges are read as the tallest column in each
            // outer third of the lit span.
            let third = (last.saturating_sub(first) / 3).max(1);
            let tallest =
                |range: std::ops::Range<usize>| columns[range].iter().copied().max().unwrap_or(0);
            (
                last.saturating_sub(first),
                tallest(first..(first + third).min(last + 1)),
                tallest(last.saturating_sub(third)..last + 1),
            )
        };
        let flat = lit(&doc(""));
        let leaning = lit(&doc(
            r#"{"id":"K","time":0,"tiltX":0,"tiltY":60,"transitionDuration":0}"#,
        ));
        assert!(flat.0 > 300, "the flat word is wide: {}", flat.0);
        assert!(
            leaning.0 < flat.0 * 3 / 4,
            "a tilted caption is narrower: {} vs {} flat",
            leaning.0,
            flat.0
        );
        assert!(
            leaning.1 > leaning.2,
            "its near edge is taller: {} vs {}",
            leaning.1,
            leaning.2
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Extruded type (rung 27): a big white word on black gains a side —
    /// grey pixels down-right of every stroke that the flat twin does not
    /// have — and the face stays white.
    #[test]
    fn extruded_type_has_a_side() {
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-depth-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        let doc = |depth: &str| {
            format!(
                r#"{{"id":"P","name":"Depth","createdAt":0,"state":"recorded","minReaderVersion":27,
                "trimStart":0,"trimEnd":2,"videoDuration":2,"subtitles":[],
                "compositionSettings":{{"canvasWidth":640,"canvasHeight":360,"backgroundColorHex":"000000",
                  "subtitleFontSize":160,"subtitleBold":true,"subtitleColorHex":"FFFFFF",
                  "subtitleBackgroundOpacity":0,"subtitleVerticalMargin":80}},
                "resources":[],
                "layers":[{{"id":"L","name":"word","sortIndex":0,"kind":"caption","isEnabled":true,
                  "startTime":0,"duration":2,"captionText":"IIII",
                  "captionStyle":{{"alignment":"center"{depth}}},"keyframes":[]}}]}}"#
            )
        };
        let render = |json: &str| -> (Vec<u8>, usize) {
            std::fs::write(dir.join("metadata.json"), json).unwrap();
            let project = crate::project::Project::open(&dir).expect("project");
            let mut renderer = Renderer::new(&project, 640, 360).expect("renderer");
            let stride = renderer.width as usize;
            (renderer.frame_bgra(1.0).expect("frame"), stride)
        };
        let count = |frame: &[u8], stride: usize, keep: &dyn Fn(u8, u8, u8) -> bool| -> usize {
            (0..360)
                .flat_map(|y| (0..640).map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    let i = (y * stride + x) * 4;
                    keep(frame[i + 2], frame[i + 1], frame[i])
                })
                .count()
        };
        let grey = |r: u8, g: u8, b: u8| r == g && g == b && r > 40 && r < 200;
        let white = |r: u8, g: u8, _b: u8| r > 250 && g > 250;
        let (flat, stride) = render(&doc(""));
        let (deep, _) = render(&doc(r#","depth":{"count":6,"offset":[3,3],"shade":0.6}"#));
        let (flat_greys, deep_greys) = (count(&flat, stride, &grey), count(&deep, stride, &grey));
        assert!(
            deep_greys > flat_greys + 400,
            "the side is grey where the flat twin is black: {deep_greys} vs {flat_greys}"
        );
        assert!(
            count(&deep, stride, &white) > 2000 && count(&flat, stride, &white) > 2000,
            "the face stays white"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Follow the pointer (rung 26): a synthetic track on the plate — the
    /// pointer on the white left half, then jumping to the black right
    /// half — and a layer that follows it at zoom 2: the window is where
    /// the pointer settled, and a click draws its ring for half a second.
    #[test]
    fn a_layer_follows_the_pointer_and_rings_its_clicks() {
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-follow-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        let (w, h) = (64u32, 48u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let i = (y * w as usize + x) * 4;
                let v = if x < 32 { 255 } else { 0 };
                rgba[i..i + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        write_png(&dir.join("Resources/plate.png"), &rgba, w, h).unwrap();
        // A canvas big enough for a ring to have a size: rings scale with it.
        let json = r#"{"id":"P","name":"Follow","createdAt":0,"state":"recorded","minReaderVersion":26,
            "trimStart":0,"trimEnd":4,"videoDuration":4,"subtitles":[],
            "compositionSettings":{"canvasWidth":640,"canvasHeight":480,"backgroundColorHex":"1020C0"},
            "resources":[{"id":"I","kind":"image","filename":"plate.png","displayName":"plate","addedAt":0,
              "pixelWidth":64,"pixelHeight":48,"imageCuts":[],"disabledAudioTrackIndices":[],
              "pointer":{"samples":[[0,0.25,0.5],[1.5,0.75,0.5]],"clicks":[[1.0,0.25,0.5]]}}],
            "layers":[{"id":"L","name":"plate","sortIndex":0,"kind":"image","isEnabled":true,
              "startTime":0,"duration":4,"resourceID":"I",
              "follow":{"zoom":2,"smoothing":0.2,"clickColorHex":"FF6A00"},"keyframes":[]}]}"#;
        std::fs::write(dir.join("metadata.json"), json).unwrap();
        let project = crate::project::Project::open(&dir).expect("project");
        let mut renderer = Renderer::new(&project, 640, 480).expect("renderer");
        // Rows are strided by the renderer's own width, whatever it settled on.
        let stride = renderer.width as usize;
        let px = move |frame: &[u8], x: usize, y: usize| -> [u8; 3] {
            let i = (y * stride + x) * 4;
            [frame[i + 2], frame[i + 1], frame[i]]
        };
        let early = renderer.frame_bgra(0.5).expect("frame");
        assert!(
            px(&early, 320, 240)[1] > 200,
            "on the white half at first: {:?} (stride {stride})",
            px(&early, 320, 240)
        );
        let late = renderer.frame_bgra(3.5).expect("frame");
        assert!(
            px(&late, 320, 240)[1] < 20,
            "settled on the black half: {:?}",
            px(&late, 320, 240)
        );
        // The ring: an antialiased orange stroke over white — red stays full
        // and blue drops well below it — somewhere around the click's place,
        // and nowhere before the click or half a second after it.
        let orange = |frame: &[u8]| {
            (160..480).any(|x| {
                (80..400).any(|y| {
                    let p = px(frame, x, y);
                    p[0] > 200 && p[0] as i32 > p[2] as i32 + 60
                })
            })
        };
        let ringed = renderer.frame_bgra(1.1).expect("frame");
        assert!(orange(&ringed), "a click ring at 1.1 s (stride {stride})");
        assert!(!orange(&early), "no ring before the click");
        let gone = renderer.frame_bgra(2.0).expect("frame");
        assert!(!orange(&gone), "the ring is gone half a second later");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The five newer transition kinds (rung 25), mid-way through a
    /// layer's own entry, each against a plain fade at the same instant:
    /// the blur dissolve softens the plate's edge, the zoom moves it, the
    /// flash lifts the black half, the glitch pulls the channels apart,
    /// and the dip is still dark where the fade already shows.
    #[test]
    fn the_newer_transitions_read_mid_way() {
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-trans-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        // White for the left quarter, black beyond: an edge at x = 16.
        let (w, h) = (64u32, 48u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let i = (y * w as usize + x) * 4;
                let v = if x < 16 { 255 } else { 0 };
                rgba[i..i + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        write_png(&dir.join("Resources/plate.png"), &rgba, w, h).unwrap();
        let doc = |kind: &str| {
            format!(
                r#"{{"id":"P","name":"Tr","createdAt":0,"state":"recorded","minReaderVersion":25,
                "trimStart":0,"trimEnd":3,"videoDuration":3,"subtitles":[],
                "compositionSettings":{{"canvasWidth":64,"canvasHeight":48,"backgroundColorHex":"000000"}},
                "resources":[{{"id":"I","kind":"image","filename":"plate.png","displayName":"plate","addedAt":0,
                  "pixelWidth":64,"pixelHeight":48,"imageCuts":[],"disabledAudioTrackIndices":[]}}],
                "layers":[{{"id":"L","name":"plate","sortIndex":0,"kind":"image","isEnabled":true,
                  "startTime":0,"duration":3,"resourceID":"I",
                  "transitionIn":{{"kind":"{kind}","duration":1.0}},"keyframes":[]}}]}}"#
            )
        };
        let render = |json: &str, at: f64| -> Vec<u8> {
            std::fs::write(dir.join("metadata.json"), json).unwrap();
            let project = crate::project::Project::open(&dir).expect("project");
            let mut renderer = Renderer::new(&project, 64, 48).expect("renderer");
            renderer.frame_bgra(at).expect("frame")
        };
        let px = |frame: &[u8], x: usize, y: usize| -> [u8; 3] {
            let i = (y * 64 + x) * 4;
            [frame[i + 2], frame[i + 1], frame[i]]
        };
        let fade = render(&doc("fade"), 0.5);
        assert!(
            px(&fade, 14, 24)[1] > 100 && px(&fade, 18, 24)[1] < 10,
            "the fade twin: a hard edge at half opacity — {:?} {:?} (row: {:?})",
            px(&fade, 14, 24),
            px(&fade, 18, 24),
            (0..64)
                .step_by(4)
                .map(|x| px(&fade, x, 24)[1])
                .collect::<Vec<_>>()
        );

        // The softness scales with the canvas: 28 px at 900 tall is 1.5 px
        // here, so the tell is the texel beside the edge, not two away.
        let soft = render(&doc("blurDissolve"), 0.5);
        assert!(
            px(&soft, 16, 24)[1] > px(&fade, 16, 24)[1] + 8,
            "blur dissolve softens the edge: {:?} vs fade {:?}",
            px(&soft, 16, 24),
            px(&fade, 16, 24)
        );

        let zoom = render(&doc("zoom"), 0.5);
        // 17.5% larger about the centre: the edge at 16 moves to about 13.
        assert!(
            px(&zoom, 15, 24)[1] < 60,
            "zoom moved the edge inward: {:?}",
            px(&zoom, 15, 24)
        );

        let flash = render(&doc("flash"), 0.5);
        assert!(
            px(&flash, 40, 24)[1] > 60,
            "flash lifts the black half: {:?}",
            px(&flash, 40, 24)
        );

        let glitch = render(&doc("glitch"), 0.5);
        let split = (0..48).any(|y| {
            let p = px(&glitch, 15, y);
            (p[0] as i32 - p[2] as i32).abs() > 40
        });
        assert!(split, "glitch pulls the channels apart at the edge");

        let dip = render(&doc("dip"), 0.25);
        assert!(
            px(&dip, 8, 24)[1] < 5,
            "dip is still dark at a quarter: {:?}",
            px(&dip, 8, 24)
        );
        let dip_late = render(&doc("dip"), 0.75);
        assert!(
            px(&dip_late, 8, 24)[1] > 100,
            "and half way in by three quarters"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// B3.4 (LUT): an inverting cube on the plate turns red into cyan and
    /// green into magenta; the unlutted twin keeps its colours; amount 0
    /// is the twin.
    #[test]
    fn a_lut_grades_the_layer_through_the_cube() {
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-lut-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        let (w, h) = (64u32, 48u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let i = (y * w as usize + x) * 4;
                let red = (20..44).contains(&x) && (12..36).contains(&y);
                rgba[i..i + 4].copy_from_slice(if red {
                    &[255, 0, 0, 255]
                } else {
                    &[0, 255, 0, 255]
                });
            }
        }
        write_png(&dir.join("Resources/plate.png"), &rgba, w, h).unwrap();
        // Size-2 inverting cube: every corner maps to its opposite.
        std::fs::write(
            dir.join("Resources/invert.cube"),
            "LUT_3D_SIZE 2\n1 1 1\n0 1 1\n1 0 1\n0 0 1\n1 1 0\n0 1 0\n1 0 0\n0 0 0\n",
        )
        .unwrap();
        let doc = |amount: Option<f64>| {
            let adjustments = match amount {
                Some(a) => format!(r#","adjustments":{{"lutResourceID":"K","lutAmount":{a}}}"#),
                None => String::new(),
            };
            // A plain neighbour on the right: the strip must never land on
            // it (texture slots are per quad; the strip joins after them).
            let neighbour = r#",{"id":"N","name":"plain","sortIndex":1,"kind":"image","isEnabled":true,
                  "startTime":0,"duration":2,"resourceID":"I",
                  "keyframes":[{"id":"nk","time":0,"placement":{"width":16,"anchor":"topRight"},"transitionDuration":0}]}"#;
            format!(
                r#"{{"id":"P","name":"Lut","createdAt":0,"state":"recorded","minReaderVersion":23,
                "trimStart":0,"trimEnd":2,"videoDuration":2,"subtitles":[],
                "compositionSettings":{{"canvasWidth":64,"canvasHeight":48,"backgroundColorHex":"101014"}},
                "resources":[{{"id":"I","kind":"image","filename":"plate.png","displayName":"plate","addedAt":0,
                  "pixelWidth":64,"pixelHeight":48,"imageCuts":[],"disabledAudioTrackIndices":[]}},
                  {{"id":"K","kind":"lut","filename":"invert.cube","displayName":"invert","addedAt":0,
                  "imageCuts":[],"disabledAudioTrackIndices":[]}}],
                "layers":[{{"id":"L","name":"plate","sortIndex":0,"kind":"image","isEnabled":true,
                  "startTime":0,"duration":2,"resourceID":"I"{adjustments},"keyframes":[]}}{neighbour}]}}"#
            )
        };
        let render = |json: &str| -> Vec<u8> {
            std::fs::write(dir.join("metadata.json"), json).unwrap();
            let project = crate::project::Project::open(&dir).expect("project");
            let mut renderer = Renderer::new(&project, 64, 48).expect("renderer");
            renderer.frame_bgra(1.0).expect("frame")
        };
        let px = |frame: &[u8], x: usize, y: usize| -> [u8; 3] {
            let i = (y * 64 + x) * 4;
            [frame[i + 2], frame[i + 1], frame[i]]
        };
        let graded = render(&doc(Some(1.0)));
        // The 16-px neighbour in the top-right corner is the plain plate.
        let corner = px(&graded, 62, 1);
        assert!(
            corner[1] > 200 && corner[0] < 40 && corner[2] < 40,
            "the neighbour stays green: {corner:?}"
        );
        let plain = render(&doc(None));
        let zero = render(&doc(Some(0.0)));
        assert_eq!(
            px(&plain, 32, 24),
            px(&zero, 32, 24),
            "amount 0 is the plain picture"
        );
        let subject = px(&graded, 32, 24);
        assert!(
            subject[0] < 30 && subject[1] > 225 && subject[2] > 225,
            "red inverts to cyan: {subject:?}"
        );
        let plate = px(&graded, 4, 4);
        assert!(
            plate[0] > 225 && plate[1] < 30 && plate[2] > 225,
            "green inverts to magenta: {plate:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// B3.5: an alpha export renders over nothing — the container says
    /// yuva, an empty corner decodes fully transparent, and the drawn
    /// subject keeps its colour and coverage.
    #[test]
    fn an_alpha_export_keeps_transparency_and_the_subject() {
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        if std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| !s.success())
            .unwrap_or(true)
        {
            eprintln!("ffmpeg unavailable; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-alpha-cli-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A red disc drawing in the middle, nothing else; the settings
        // colour must not paint under an alpha export.
        std::fs::write(
            dir.join("metadata.json"),
            r#"{"id":"P","name":"Alpha","createdAt":0,"state":"recorded","minReaderVersion":18,
            "trimStart":0,"trimEnd":1,"videoDuration":1,"subtitles":[],
            "compositionSettings":{"canvasWidth":64,"canvasHeight":48,"backgroundColorHex":"1020C0"},
            "resources":[{"id":"D","kind":"drawing","filename":"","displayName":"disc","addedAt":0,"imageCuts":[],
              "disabledAudioTrackIndices":[],"drawing":{"shapes":[{"id":"S","kind":"oval","points":[[16,8],[48,40]],
              "strokeColorHex":"FF0000","strokeWidth":1,"fillColorHex":"FF0000","arrowStart":false,"arrowEnd":false}]}}],
            "layers":[{"id":"L","name":"disc","sortIndex":0,"kind":"drawing","isEnabled":true,
              "startTime":0,"duration":1,"resourceID":"D","keyframes":[]}]}"#,
        )
        .unwrap();
        let project = crate::project::Project::open(&dir).expect("project");
        let out = dir.join("alpha.mov");
        let settings = ExportSettings {
            width: 64,
            height: 48,
            codec: promo_media::VideoCodec::ProRes4444,
            alpha: true,
            start: 0.0,
            end: 0.5,
            fps: 30.0,
            overlay: None,
        };
        export_video(
            &project,
            &out,
            &settings,
            &mut |_, _| true,
            ProxyPolicy::Off,
        )
        .expect("export");
        use promo_media::VideoDecoder;
        let mut decoder = promo_media::ffmpeg::FfmpegDecoder::open(&out).expect("decoder");
        assert!(decoder.info().has_alpha, "{:?}", decoder.info());
        let Some(promo_gpu::GpuSurface::CpuPixels { data, width, .. }) =
            decoder.frame_at(0.2).expect("frame")
        else {
            panic!("cpu pixels expected");
        };
        let px = |x: usize, y: usize| -> [u8; 4] {
            let i = (y * width as usize + x) * 4;
            [data[i], data[i + 1], data[i + 2], data[i + 3]]
        };
        let corner = px(2, 2);
        assert!(
            corner[3] < 8,
            "the empty corner is transparent, not the settings colour: {corner:?}"
        );
        let centre = px(32, 24);
        assert!(
            centre[3] > 240 && centre[2] > 200 && centre[1] < 60,
            "the disc is opaque red: {centre:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The nesting oracle: a composition placed as a clip must render
    /// pixel-identical to the same layers flattened into the parent with
    /// their times offset — the recursion adds a texture round trip and
    /// nothing else. An independent oracle: both documents go through the
    /// CLI's own renderer, not a twin of it.
    #[test]
    fn a_nested_composition_renders_as_its_layers_flattened() {
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-nest-oracle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shape = r#"{"id":"S","kind":"oval","points":[[20,10],[140,80]],"strokeColorHex":"FFFFFF",
            "strokeWidth":2,"fillColorHex":"5B8CFF","arrowStart":false,"arrowEnd":false}"#;
        let drawing = format!(
            r#"{{"id":"D","kind":"drawing","filename":"","displayName":"Blob","addedAt":0,"imageCuts":[],
            "disabledAudioTrackIndices":[],"drawing":{{"shapes":[{}]}}}}"#,
            shape
        );
        // The composition's layers: a plate and the drawing, on the
        // composition's own clock (0…3). The flattened twin carries the same
        // two layers offset by the card's start and NO plate of its own: one
        // background layer paints per document level, and outside the card's
        // window both documents show the settings colour.
        let inner = |offset: f64| {
            format!(
                r#"{{"id":"IB","name":"plate","sortIndex":0,"kind":"background","isEnabled":true,
                  "startTime":{o},"duration":3,"keyframes":[
                    {{"id":"IK","time":0,"colorHex":"203040","transitionDuration":0}}]}},
                  {{"id":"ID","name":"blob","sortIndex":1,"kind":"drawing","isEnabled":true,
                  "startTime":{o1},"duration":2,"resourceID":"D","keyframes":[]}}"#,
                o = offset,
                o1 = offset + 0.5
            )
        };
        let nested = format!(
            r#"{{"id":"P","name":"Nested","createdAt":0,"state":"recorded","minReaderVersion":19,
            "trimStart":0,"trimEnd":5,"videoDuration":5,"subtitles":[],
            "compositionSettings":{{"canvasWidth":160,"canvasHeight":90,"backgroundColorHex":"101014"}},
            "resources":[{drawing},
              {{"id":"C","kind":"composition","filename":"","displayName":"Card","addedAt":0,
               "duration":3,"pixelWidth":160,"pixelHeight":90,"imageCuts":[],
               "composition":{{"canvasWidth":160,"canvasHeight":90,"layers":[{inner}]}}}}],
            "layers":[
              {{"id":"BG","name":"bg","sortIndex":0,"kind":"background","isEnabled":true,
               "startTime":0,"duration":5,"keyframes":[]}},
              {{"id":"L","name":"card","sortIndex":1,"kind":"video","isEnabled":true,
               "startTime":1,"duration":3,"resourceID":"C","keyframes":[]}}]}}"#,
            drawing = drawing,
            inner = inner(0.0)
        );
        let flattened = format!(
            r#"{{"id":"P","name":"Flat","createdAt":0,"state":"recorded","minReaderVersion":18,
            "trimStart":0,"trimEnd":5,"videoDuration":5,"subtitles":[],
            "compositionSettings":{{"canvasWidth":160,"canvasHeight":90,"backgroundColorHex":"101014"}},
            "resources":[{drawing}],
            "layers":[{inner}]}}"#,
            drawing = drawing,
            inner = inner(1.0)
                .replace(
                    r#""sortIndex":0,"kind":"background""#,
                    r#""sortIndex":1,"kind":"background""#
                )
                .replace(
                    r#""sortIndex":1,"kind":"drawing""#,
                    r#""sortIndex":2,"kind":"drawing""#
                )
        );
        let render = |json: &str, at: f64| -> Vec<u8> {
            std::fs::write(dir.join("metadata.json"), json).unwrap();
            let project = crate::project::Project::open(&dir).expect("project");
            let mut renderer = Renderer::new(&project, 160, 90).expect("renderer");
            renderer.frame_bgra(at).expect("frame")
        };
        for at in [0.5, 1.2, 2.0, 3.5] {
            let a = render(&nested, at);
            let b = render(&flattened, at);
            assert_eq!(a.len(), b.len());
            let worst = a
                .iter()
                .zip(&b)
                .map(|(x, y)| (*x as i32 - *y as i32).abs())
                .max()
                .unwrap();
            let differing = a.iter().zip(&b).filter(|(x, y)| x != y).count();
            assert!(
                worst <= 3,
                "t={at}: nested and flattened differ by up to {worst} ({differing} bytes); \
                 first px nested={:?} flat={:?}, centre nested={:?} flat={:?}",
                &a[0..4],
                &b[0..4],
                &a[(45 * 160 + 80) * 4..(45 * 160 + 80) * 4 + 4],
                &b[(45 * 160 + 80) * 4..(45 * 160 + 80) * 4 + 4]
            );
            if at == 2.0 {
                // The picture is not trivially empty: the blob (visible from
                // 0.5 on the card's clock, 1.5 on the parent's) is there.
                let blue = a.chunks(4).filter(|px| px[0] > 200 && px[2] < 140).count();
                assert!(
                    blue > 500,
                    "t={at}: the nested blob is drawn ({blue} blue pixels)"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Decoders must follow the playhead, not the composition.
    ///
    /// Each open decoder is a live ffmpeg process. Holding one per clip for
    /// the whole render is fine for five clips and a scaling cliff at fifty,
    /// so a clip's decoder opens when its window arrives and closes once it
    /// has passed.
    #[test]
    fn decoders_open_and_close_with_the_playhead() {
        let dir = std::env::temp_dir().join("promo-cli-lifetime");
        let _ = std::fs::remove_dir_all(&dir);
        let Some(project) = three_clip_project(&dir) else {
            eprintln!("ffmpeg unavailable; skipping");
            return;
        };
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let mut renderer = Renderer::new(&project, 160, 120).expect("renderer");
        assert_eq!(
            renderer.open_decoders(),
            0,
            "nothing is open before the first frame"
        );

        renderer.frame_bgra(0.5).expect("first clip");
        assert_eq!(renderer.open_decoders(), 1, "only the clip on screen");

        renderer.frame_bgra(5.0).expect("third clip");
        assert_eq!(
            renderer.open_decoders(),
            1,
            "the earlier clips' decoders were closed, not accumulated"
        );
    }

    /// A finish on a binding (rung 32) changes the shading: the same grey
    /// cube reads differently as chrome than as the file's own half-metal
    /// — and the object form with a colour alone renders exactly as the
    /// bare colour string does.
    #[test]
    fn a_finish_on_a_binding_changes_the_shading() {
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-finish-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        std::fs::write(
            dir.join("Resources").join("grey.glb"),
            promo_engine::model::sample_cube_glb_with(0.5, "Body", [0.7, 0.7, 0.7, 1.0]),
        )
        .unwrap();
        let doc = |materials: &str| {
            format!(
                r#"{{"id":"P","name":"Finish","createdAt":0,"state":"recorded","minReaderVersion":32,
                "trimStart":0,"trimEnd":2,"videoDuration":2,"subtitles":[],
                "compositionSettings":{{"canvasWidth":320,"canvasHeight":320,"backgroundColorHex":"000000"}},
                "resources":[
                  {{"id":"M","kind":"model","filename":"grey.glb","displayName":"Grey","addedAt":0,
                    "materials":{materials}}}],
                "layers":[{{"id":"L","name":"grey","sortIndex":0,"kind":"model","isEnabled":true,
                  "startTime":0,"duration":2,"resourceID":"M",
                  "keyframes":[{{"id":"K0","time":0,"camera":{{"yaw":25,"pitch":20,"distance":3.0}},"transitionDuration":0}}]}}]}}"#
            )
        };
        let render = |json: &str| -> Vec<u8> {
            std::fs::write(dir.join("metadata.json"), json).unwrap();
            let project = crate::project::Project::open(&dir).expect("project");
            let mut renderer = Renderer::new(&project, 320, 320).expect("renderer");
            let frame = renderer.frame_bgra(0.5).expect("frame");
            // The middle of the frame: on the cube, whatever faces show.
            (100..220)
                .flat_map(|y| (100..220).map(move |x| (x, y)))
                .flat_map(|(x, y)| frame[(y * 320 + x) * 4..(y * 320 + x) * 4 + 3].to_vec())
                .collect()
        };
        let bare = render(&doc(r#"{"Body":"B0B0B0"}"#));
        let object = render(&doc(r#"{"Body":{"colorHex":"B0B0B0"}}"#));
        assert_eq!(bare, object, "the object form with a colour alone is the bare colour");
        let chrome = render(&doc(r#"{"Body":{"colorHex":"B0B0B0","metallic":1,"roughness":0.1}}"#));
        let matte = render(&doc(r#"{"Body":{"colorHex":"B0B0B0","metallic":0,"roughness":1}}"#));
        let differ = |a: &[u8], b: &[u8]| -> usize {
            a.iter()
                .zip(b)
                .filter(|(x, y)| (**x as i32 - **y as i32).abs() > 12)
                .count()
        };
        assert!(
            differ(&chrome, &bare) > bare.len() / 5,
            "chrome shades differently from the file's finish: {} of {}",
            differ(&chrome, &bare),
            bare.len()
        );
        assert!(
            differ(&chrome, &matte) > bare.len() / 5,
            "chrome and matte differ: {} of {}",
            differ(&chrome, &matte),
            bare.len()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A stage as one layer (rung 33) draws exactly what its flat form
    /// draws: the same two cubes across a stage, once as members sharing a
    /// stage name with the first carrying the camera and placement, once
    /// as a stage layer holding both — pixel for pixel.
    #[test]
    fn a_stage_layer_renders_as_its_flat_form() {
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-stagelayer-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        std::fs::write(
            dir.join("Resources").join("red.glb"),
            promo_engine::model::sample_cube_glb_with(0.5, "Body", [0.9, 0.1, 0.1, 1.0]),
        )
        .unwrap();
        std::fs::write(
            dir.join("Resources").join("blue.glb"),
            promo_engine::model::sample_cube_glb_with(0.5, "Body", [0.1, 0.1, 0.9, 1.0]),
        )
        .unwrap();
        let head = r#"{"id":"P","name":"Across","createdAt":0,"state":"recorded",
            "trimStart":0,"trimEnd":2,"videoDuration":2,"subtitles":[],
            "compositionSettings":{"canvasWidth":320,"canvasHeight":320,"backgroundColorHex":"000000"},
            "resources":[
              {"id":"R","kind":"model","filename":"red.glb","displayName":"Red","addedAt":0},
              {"id":"B","kind":"model","filename":"blue.glb","displayName":"Blue","addedAt":0,"materials":{"Body":"10E040"}}],"#;
        let flat = format!(
            r#"{head}"minReaderVersion":31,"layers":[
              {{"id":"L1","name":"red","sortIndex":0,"kind":"model","isEnabled":true,"stage":"s",
                "startTime":0,"duration":2,"resourceID":"R",
                "keyframes":[{{"id":"K1","time":0,"camera":{{"yaw":0,"pitch":0,"distance":3.0}},"stageOffset":[1.6,0],
                  "placement":{{"width":300,"anchor":"center"}},"transitionDuration":0}}]}},
              {{"id":"L2","name":"green","sortIndex":1,"kind":"model","isEnabled":true,"stage":"s",
                "startTime":0,"duration":2,"resourceID":"B",
                "keyframes":[{{"id":"K2","time":0,"stageOffset":[-1.6,0],"transitionDuration":0}}]}}]}}"#
        );
        let nested = format!(
            r#"{head}"minReaderVersion":33,"layers":[
              {{"id":"S","name":"bench","sortIndex":0,"kind":"stage","isEnabled":true,
                "startTime":0,"duration":2,
                "keyframes":[{{"id":"K0","time":0,"camera":{{"yaw":0,"pitch":0,"distance":3.0}},
                  "placement":{{"width":300,"anchor":"center"}},"transitionDuration":0}}],
                "members":[
                  {{"id":"L1","name":"red","sortIndex":0,"kind":"model","isEnabled":true,
                    "startTime":0,"duration":2,"resourceID":"R",
                    "keyframes":[{{"id":"K1","time":0,"stageOffset":[1.6,0],"transitionDuration":0}}]}},
                  {{"id":"L2","name":"green","sortIndex":1,"kind":"model","isEnabled":true,
                    "startTime":0,"duration":2,"resourceID":"B",
                    "keyframes":[{{"id":"K2","time":0,"stageOffset":[-1.6,0],"transitionDuration":0}}]}}]}}]}}"#
        );
        let render = |json: &str| -> Vec<u8> {
            std::fs::write(dir.join("metadata.json"), json).unwrap();
            let project = crate::project::Project::open(&dir).expect("project");
            let mut renderer = Renderer::new(&project, 320, 320).expect("renderer");
            renderer.frame_bgra(0.5).expect("frame")
        };
        let a = render(&flat);
        let b = render(&nested);
        let strip = |frame: &[u8], x0: usize, x1: usize| -> (u64, u64) {
            let (mut r, mut g, mut n) = (0u64, 0u64, 0u64);
            for y in 120..200 {
                for x in x0..x1 {
                    let i = (y * 320 + x) * 4;
                    r += frame[i + 2] as u64;
                    g += frame[i + 1] as u64;
                    n += 1;
                }
            }
            (r / n, g / n)
        };
        let (r, _) = strip(&b, 220, 300);
        let (_, g) = strip(&b, 20, 100);
        assert!(r > 60 && g > 60, "the stage layer draws both cubes: r {r} g {g}");
        let differing = a
            .iter()
            .zip(&b)
            .filter(|(x, y)| (**x as i32 - **y as i32).abs() > 2)
            .count();
        assert_eq!(differing, 0, "the stage layer is its flat form, pixel for pixel");
        // And the LIFT of the flat document — what the app and promo_apply
        // write back — draws the same picture again.
        let lifted = promo_model::ProjectMetadata::from_json(&flat)
            .expect("flat")
            .lifted();
        assert_eq!(
            lifted.layers.as_deref().unwrap().len(),
            1,
            "one stage layer stands for the two members"
        );
        let c = render(&lifted.to_json().expect("encode"));
        let differing = a
            .iter()
            .zip(&c)
            .filter(|(x, y)| (**x as i32 - **y as i32).abs() > 2)
            .count();
        assert_eq!(differing, 0, "the lifted document is the flat one, pixel for pixel");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A scene environment (rung 35) is what a metal mirrors: the same
    /// chrome cube reads brighter and differently under the studio than
    /// under the synthetic sky, and turning the environment turns the
    /// reflection.
    #[test]
    fn a_scene_environment_lights_a_metal() {
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-env-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        // A sphere: its reflections sweep every direction, so a turned
        // environment must show — a cube's flat faces each mirror one
        // direction and can land on the same radiance twice.
        std::fs::write(
            dir.join("Resources").join("grey.glb"),
            promo_engine::model::shape_glb(promo_engine::model::ShapeKind::Sphere),
        )
        .unwrap();
        let doc = |environment: &str| {
            format!(
                r#"{{"id":"P","name":"Env","createdAt":0,"state":"recorded","minReaderVersion":35,
                "trimStart":0,"trimEnd":2,"videoDuration":2,"subtitles":[],
                "compositionSettings":{{"canvasWidth":320,"canvasHeight":320,"backgroundColorHex":"000000"{environment}}},
                "resources":[
                  {{"id":"M","kind":"model","filename":"grey.glb","displayName":"Grey","addedAt":0,
                    "materials":{{"Body":{{"colorHex":"C8C8C8","metallic":1,"roughness":0.1}}}}}}],
                "layers":[{{"id":"L","name":"grey","sortIndex":0,"kind":"model","isEnabled":true,
                  "startTime":0,"duration":2,"resourceID":"M",
                  "keyframes":[{{"id":"K0","time":0,"camera":{{"yaw":25,"pitch":20,"distance":3.0}},"transitionDuration":0}}]}}]}}"#
            )
        };
        let render = |json: &str| -> Vec<u8> {
            std::fs::write(dir.join("metadata.json"), json).unwrap();
            let project = crate::project::Project::open(&dir).expect("project");
            let mut renderer = Renderer::new(&project, 320, 320).expect("renderer");
            let frame = renderer.frame_bgra(0.5).expect("frame");
            (100..220)
                .flat_map(|y| (100..220).map(move |x| (x, y)))
                .flat_map(|(x, y)| frame[(y * 320 + x) * 4..(y * 320 + x) * 4 + 3].to_vec())
                .collect()
        };
        let plain = render(&doc(""));
        let studio = render(&doc(r#","environment":{"preset":"studio"}"#));
        let turned = render(&doc(r#","environment":{"preset":"studio","rotation":180}"#));
        let mean = |a: &[u8]| a.iter().map(|v| *v as u64).sum::<u64>() / a.len() as u64;
        let differ = |a: &[u8], b: &[u8]| -> usize {
            a.iter()
                .zip(b)
                .filter(|(x, y)| (**x as i32 - **y as i32).abs() > 12)
                .count()
        };
        assert!(
            differ(&studio, &plain) > plain.len() / 5,
            "the studio changes a chrome cube: {} of {}",
            differ(&studio, &plain),
            plain.len()
        );
        assert!(
            mean(&studio) > mean(&plain),
            "the studio is brighter than the synthetic sky: {} vs {}",
            mean(&studio),
            mean(&plain)
        );
        assert!(
            differ(&turned, &studio) > plain.len() / 20,
            "turning the environment turns the reflection: {} of {}",
            differ(&turned, &studio),
            plain.len()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A particle system (rung 36) draws through a drawing layer: a
    /// confetti burst puts colour on a black canvas, the same instant
    /// renders the same bytes twice, and a later instant differs.
    #[test]
    fn a_particle_burst_draws_and_is_deterministic() {
        if GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-particles-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        let doc = r#"{"id":"P","name":"Confetti","createdAt":0,"state":"recorded","minReaderVersion":36,
            "trimStart":0,"trimEnd":3,"videoDuration":3,"subtitles":[],
            "compositionSettings":{"canvasWidth":320,"canvasHeight":320,"backgroundColorHex":"000000"},
            "resources":[{"id":"C","kind":"particles","filename":"","displayName":"Confetti","addedAt":0,
              "particles":{"burst":200,"rate":0,"anchor":[0.5,0.6],"speed":[0.6,1.0],"spread":60,
                           "size":[0.03,0.05],"life":[2,3],"colors":["FF4040","40FF40","4080FF"],"shape":"square"}}],
            "layers":[{"id":"L","name":"confetti","sortIndex":0,"kind":"drawing","isEnabled":true,
              "startTime":0,"duration":3,"resourceID":"C","keyframes":[]}]}"#;
        std::fs::write(dir.join("metadata.json"), doc).unwrap();
        let project = crate::project::Project::open(&dir).expect("project");
        let mut renderer = Renderer::new(&project, 320, 320).expect("renderer");
        let lit = |frame: &[u8]| frame.chunks(4).filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 60).count();
        let a = renderer.frame_bgra(0.4).expect("frame");
        let b = renderer.frame_bgra(0.4).expect("frame");
        let c = renderer.frame_bgra(1.2).expect("frame");
        assert!(lit(&a) > 200, "confetti lights the canvas: {} pixels", lit(&a));
        assert_eq!(a, b, "the same instant is the same picture");
        let differ = a.iter().zip(&c).filter(|(x, y)| (**x as i32 - **y as i32).abs() > 12).count();
        assert!(differ > 200, "a later instant differs: {differ}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
