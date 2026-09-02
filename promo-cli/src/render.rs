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
        let ctx = GpuContext::shared().ok_or("no GPU adapter available")?;

        let registry = Registry::with_defaults();
        let (frames, cuts, videos) = Self::stage(project, &registry)?;
        let state = Box::new(Mutex::new(HostState {
            frames,
            cuts,
            videos,
        }));
        let user = &*state as *const Mutex<HostState> as *mut c_void;
        let mut engine = PreviewEngine::new(project.meta.clone(), provider, user, 512 << 20)
            .map_err(|e| format!("engine: {e:?}"))?;
        // Offline renderer: the export clock is monotonic and quality is
        // the point — per-time frames skip the cache, and a motion-blur
        // walk gets the export-grade sample cap rather than the preview's.
        engine.set_export_mode(true);

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
        let mut frames = HashMap::new();
        let mut cuts = HashMap::new();
        let mut videos = HashMap::new();
        for layer in promo_model::nesting::all_layers(&project.meta) {
            if project.unsupported(layer).is_some() {
                continue;
            }
            let mut candidates: Vec<String> = layer.resource_id.iter().cloned().collect();
            candidates.extend(layer.keyframes.iter().filter_map(|k| k.resource_id.clone()));
            candidates.dedup();
            // Only stills are preloaded; video opens while it is on screen.
            // Video keys on the BASE resource (swaps never target video).
            if layer.kind == promo_model::ProjectLayerKind::Video {
                let Some(resource) = candidates.first().and_then(|id| project.resource(id)) else {
                    continue;
                };
                let Some(path) = project.resource_path(resource) else {
                    continue;
                };
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
) -> Result<ExportOutcome, String> {
    let ExportSettings {
        width,
        height,
        start,
        end,
        fps,
        ref overlay,
    } = *settings;
    let count = (((end - start) * fps).round() as usize).max(1);

    let mut renderer = Renderer::new(project, width, height)?;
    if let Some((bgra, overlay_width, overlay_height)) = overlay {
        renderer.set_overlay(Some((bgra, *overlay_width, *overlay_height)))?;
    }
    let audio = build_soundtrack(project, end - start)?;
    let spec = promo_media::EncodeSpec {
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
        let bgra = renderer.frame_bgra(time)?;
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
    let (inputs, focus) = audio_inputs(&project.meta, &renderable);

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
            .read_tracks(&path, SAMPLE_RATE, CHANNELS, speed, &selection)
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
}
