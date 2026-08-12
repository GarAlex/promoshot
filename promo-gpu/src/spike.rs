//! P0 interop spike: prove the zero-copy pipeline shape on macOS.
//!
//!   IOSurface  ──wrap──▶  MTLTexture  ──adopt──▶  wgpu::Texture
//!        ▲                                              │
//!        └────────── CPU mapping sees the pixels ◀── render pass
//!
//! wgpu renders into the adopted texture; the SAME memory is then read back
//! through the IOSurface's CPU mapping — no copy anywhere. This is the exact
//! path VideoToolbox frames (in) and AVAssetWriter frames (out) will take.

use crate::iosurface::{metal_texture_from_iosurface, OwnedIoSurface};
use crate::{GpuContext, GpuError};

/// Timings for one spike run, microseconds.
#[derive(Debug, Clone, Copy)]
pub struct SpikeTimings {
    pub import_us: f64,
    pub render_us: f64,
    pub readback_us: f64,
}

/// Clear color written by wgpu, expected back through the IOSurface mapping
/// (BGRA8Unorm, no sRGB re-encode: bytes are exactly these).
const CLEAR_R: u8 = 51; // 0.2
const CLEAR_G: u8 = 102; // 0.4
const CLEAR_B: u8 = 204; // 0.8

/// Runs the full import→render→CPU-readback round trip once.
pub fn run(ctx: &GpuContext, width: usize, height: usize) -> Result<SpikeTimings, GpuError> {
    let surface = OwnedIoSurface::new_bgra(width, height)?;

    // 1. Wrap the IOSurface as a Metal texture and adopt it into wgpu.
    let t0 = std::time::Instant::now();
    let metal_device = unsafe {
        ctx.device
            .as_hal::<wgpu::hal::api::Metal, _, _>(|dev| dev.map(|d| d.raw_device().lock().clone()))
            .flatten()
            .ok_or_else(|| GpuError::Import("not a Metal device".into()))?
    };
    let mtl_texture = metal_texture_from_iosurface(&metal_device, surface.raw(), width, height)?;

    let hal_texture = unsafe {
        wgpu::hal::metal::Device::texture_from_raw(
            mtl_texture,
            wgpu::TextureFormat::Bgra8Unorm,
            metal::MTLTextureType::D2,
            1,
            1,
            wgpu::hal::CopyExtent {
                width: width as u32,
                height: height as u32,
                depth: 1,
            },
        )
    };
    let texture = unsafe {
        ctx.device.create_texture_from_hal::<wgpu::hal::api::Metal>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some("spike-iosurface"),
                size: wgpu::Extent3d {
                    width: width as u32,
                    height: height as u32,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            },
        )
    };
    let import_us = t0.elapsed().as_secs_f64() * 1e6;

    // 2. Render: clear the adopted texture through a wgpu render pass.
    let t1 = std::time::Instant::now();
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("spike"),
        });
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("spike-clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: CLEAR_R as f64 / 255.0,
                        g: CLEAR_G as f64 / 255.0,
                        b: CLEAR_B as f64 / 255.0,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
    }
    ctx.queue.submit([encoder.finish()]);
    ctx.device.poll(wgpu::Maintain::Wait);
    let render_us = t1.elapsed().as_secs_f64() * 1e6;

    // 3. Read the SAME memory back through the IOSurface CPU mapping.
    let t2 = std::time::Instant::now();
    let pixels = surface.read_pixels()?;
    let readback_us = t2.elapsed().as_secs_f64() * 1e6;

    // Verify every pixel carries the wgpu clear color (BGRA order).
    let mut checked = 0usize;
    for px in pixels.chunks_exact(4) {
        if px[0] != CLEAR_B || px[1] != CLEAR_G || px[2] != CLEAR_R {
            return Err(GpuError::Readback(format!(
                "pixel {} mismatch: got BGRA {:?}, want [{} {} {} 255]",
                checked,
                &px[..4],
                CLEAR_B,
                CLEAR_G,
                CLEAR_R
            )));
        }
        checked += 1;
    }
    if checked != width * height {
        return Err(GpuError::Readback(format!(
            "checked {} pixels, expected {}",
            checked,
            width * height
        )));
    }

    Ok(SpikeTimings {
        import_us,
        render_us,
        readback_us,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iosurface_wgpu_roundtrip() {
        let ctx = GpuContext::new().expect("gpu context");
        let timings = run(&ctx, 256, 256).expect("spike");
        // Sanity ceilings, not perf gates (those live in the bench):
        assert!(
            timings.import_us < 100_000.0,
            "import too slow: {timings:?}"
        );
        assert!(
            timings.render_us < 500_000.0,
            "render too slow: {timings:?}"
        );
    }

    #[test]
    fn spike_works_at_video_sizes() {
        let ctx = GpuContext::new().expect("gpu context");
        run(&ctx, 1920, 1080).expect("1080p spike");
    }
}
