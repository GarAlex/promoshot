//! Rendering frames from a project folder.
//!
//! The CLI is a *host* for `promo-engine`: it answers the engine's frame
//! provider with plain BGRA pixels loaded from disk, and takes the composited
//! result out of a wgpu texture. That is exactly the portable path R1 opened
//! — on Apple the same engine is fed zero-copy IOSurfaces instead.

use crate::project::Project;
use promo_engine::{HostSurface, PreviewEngine, SURFACE_CPU_PIXELS};
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
    /// layer id → BGRA pixels + size
    frames: HashMap<String, (Vec<u8>, u32, u32)>,
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
    if let Some((pixels, width, height)) = state.frames.get(&resource) {
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
        let mut frames = HashMap::new();
        let mut videos = HashMap::new();
        for layer in project.meta.layers.as_deref().unwrap_or(&[]) {
            if project.unsupported(layer).is_some() {
                continue;
            }
            // Every resource this layer can show: its own, plus anything a
            // keyframe swaps to. Loading only the first would leave the rest
            // of the layer blank.
            let mut candidates: Vec<String> = layer.resource_id.iter().cloned().collect();
            candidates.extend(layer.keyframes.iter().filter_map(|k| k.resource_id.clone()));
            candidates.dedup();
            let Some(resource) = candidates.first().and_then(|id| project.resource(id)) else {
                continue;
            };
            let Some(path) = project.resource_path(resource) else {
                continue;
            };
            // Only stills are preloaded; video opens while it is on screen.
            if layer.kind == promo_model::ProjectLayerKind::Video {
                // Open once now so a broken file fails here rather than
                // mid-render, then drop it: the render reopens what it needs.
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
                let Some(resource) = project.resource(candidate) else { continue };
                let Some(path) = project.resource_path(resource) else { continue };
                if frames.contains_key(candidate) {
                    continue;
                }
            let decoded = image::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let rgba = decoded.to_rgba8();
            let (w, h) = rgba.dimensions();
            // The compositor samples premultiplied BGRA; a PNG decodes to
            // straight RGBA, so convert both channel order and alpha or every
            // soft edge in the image saturates.
            let mut bgra = rgba.into_raw();
            for px in bgra.chunks_exact_mut(4) {
                px.swap(0, 2);
                let a = px[3] as u32;
                for channel in px.iter_mut().take(3) {
                    *channel = ((*channel as u32 * a + 127) / 255) as u8;
                }
            }
            frames.insert(candidate.clone(), (bgra, w, h));
            }
        }

        let state = Box::new(Mutex::new(HostState { frames, videos }));
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
        })
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
            .render_to_texture(time, &self.target, self.width, self.height)
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

/// The composition's soundtrack: every audio-bearing layer decoded, placed at
/// its position on the output timeline, and mixed.
///
/// Uses `promo_engine::mix_chunk`, the same summing the app's preview and
/// export use, so the CLI cannot drift into its own idea of a mix.
pub fn build_soundtrack(
    project: &Project,
    duration: f64,
) -> Result<Option<promo_media::AudioBuffer>, String> {
    use promo_engine::{mix_chunk, MixInput};

    const SAMPLE_RATE: u32 = 48_000;
    const CHANNELS: u16 = 2;

    if duration <= 0.0 {
        return Ok(None);
    }
    let registry = Registry::with_defaults();
    let reader = registry.audio_reader();

    // Decode first: a layer contributes only if its asset actually has audio,
    // which most screen recordings do not.
    let mut sources: Vec<(f64, promo_media::AudioBuffer)> = Vec::new();
    for layer in project.meta.layers.as_deref().unwrap_or(&[]) {
        if !layer.is_enabled {
            continue;
        }
        let kind = layer.kind;
        if kind != promo_model::ProjectLayerKind::Video
            && kind != promo_model::ProjectLayerKind::Audio
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
        let Some(path) = project.resource_path(resource) else {
            continue;
        };
        // Read through the cut's view so its speed (and trim) apply.
        let view = promo_timeline::resource_for_cut(resource, layer.media_cut_id.as_deref());
        let decoded = reader
            .read_at_speed(&path, SAMPLE_RATE, CHANNELS, view.speed.unwrap_or(1.0))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let Some(mut audio) = decoded else { continue };

        // The layer plays a window of the source; cut the PCM to match rather
        // than trusting the mixer to clip it. A named cut shadows the
        // resource's own trim, so audio cuts and video cuts mean the same
        // thing and are resolved by the same helper.
        // The stretched stream has its own clock: a trim of 4s into a source
        // played at 2x begins 2s into the stretched PCM.
        let speed = view.speed.unwrap_or(1.0).clamp(0.1, 10.0);
        let trim_start = view.trim_start.unwrap_or(0.0).max(0.0) / speed;
        let layer_len = layer
            .duration
            .unwrap_or_else(|| audio.duration_s() - trim_start)
            .max(0.0);
        let frame = CHANNELS as usize;
        let from = (trim_start * SAMPLE_RATE as f64) as usize * frame;
        let take = (layer_len * SAMPLE_RATE as f64) as usize * frame;
        if from >= audio.samples.len() {
            continue;
        }
        let end = (from + take).min(audio.samples.len());
        audio.samples = audio.samples[from..end].to_vec();
        if audio.samples.is_empty() {
            continue;
        }
        sources.push((layer.start_time.max(0.0), audio));
    }
    if sources.is_empty() {
        return Ok(None);
    }

    let frames = (duration * SAMPLE_RATE as f64).ceil() as usize;
    let mut output = vec![0.0f32; frames * CHANNELS as usize];
    let inputs: Vec<MixInput> = sources
        .iter()
        .map(|(start, audio)| MixInput {
            samples: &audio.samples,
            start_time: *start,
            points: &[],
        })
        .collect();
    mix_chunk(
        &mut output,
        CHANNELS as usize,
        SAMPLE_RATE as f64,
        0.0,
        &inputs,
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
