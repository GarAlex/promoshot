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
use std::sync::Mutex;

/// What the host can answer with.
///
/// Images are decoded once up front — the engine caches textures itself, so
/// this only avoids re-decoding a PNG for every frame of a slideshow. Video
/// keeps an open decoder per layer, because a clip is far too big to hold as
/// frames and the engine asks for one source time at a time.
struct HostState {
    /// layer id → BGRA pixels + size
    frames: HashMap<String, (Vec<u8>, u32, u32)>,
    /// layer id → decoder, plus the last frame it produced.
    videos: HashMap<String, VideoLayer>,
}

struct VideoLayer {
    decoder: Box<dyn VideoDecoder>,
    /// The decoded frame, held so the provider can hand out a pointer to it.
    /// The engine copies during the call, but the buffer must outlive the
    /// call itself.
    last: Option<(Vec<u8>, u32, u32)>,
}

extern "C" fn provider(
    user: *mut c_void,
    layer_id: *const c_char,
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

    // A still image: the same pixels whatever the time.
    if let Some((pixels, width, height)) = state.frames.get(&id) {
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
    match video.decoder.frame_at(source_time.max(0.0)) {
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
            // Video keeps a live decoder; only stills are preloaded.
            if layer.kind == promo_model::ProjectLayerKind::Video {
                let decoder = registry
                    .open_decoder(&path)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                videos.insert(
                    layer.id.clone(),
                    VideoLayer {
                        decoder,
                        last: None,
                    },
                );
                continue;
            }
            let decoded = image::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let rgba = decoded.to_rgba8();
            let (w, h) = rgba.dimensions();
            // The compositor samples BGRA, matching what every platform
            // surface hands over.
            let mut bgra = rgba.into_raw();
            for px in bgra.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
            frames.insert(layer.id.clone(), (bgra, w, h));
        }

        let state = Box::new(Mutex::new(HostState { frames, videos }));
        let user = &*state as *const Mutex<HostState> as *mut c_void;
        let engine = PreviewEngine::new(project.meta.clone(), provider, user, 512 << 20)
            .map_err(|e| format!("engine: {e:?}"))?;

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

    /// Renders one frame as BGRA rows — what a raw-video pipe wants.
    pub fn frame_bgra(&mut self, time: f64) -> Result<Vec<u8>, String> {
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
