//! promo-gpu: the wgpu compositor and per-platform GPU-surface import.
//!
//! P0 scope: device bring-up, the `GpuSurface` abstraction all codec backends
//! fill, and the IOSurface↔wgpu interop spike (macOS) proving the zero-copy
//! path: a VideoToolbox-style IOSurface imported as a wgpu texture, rendered
//! to by wgpu, and the result visible through the IOSurface's CPU mapping
//! without any pixel copy.

pub mod compositor;
mod surface;
pub use surface::{GpuSurface, ImportedFrame};
/// Re-exported so dependents use exactly this wgpu, not a second copy.
pub use wgpu;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod iosurface;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod spike;
pub mod vector;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum GpuError {
    #[error("no suitable GPU adapter found")]
    NoAdapter,
    #[error("device request failed: {0}")]
    Device(String),
    #[error("surface import failed: {0}")]
    Import(String),
    #[error("readback failed: {0}")]
    Readback(String),
}

/// The core's GPU context: one device/queue shared by compositor and caches.
pub struct GpuContext {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl GpuContext {
    /// Brings up the default high-performance adapter (Metal on Apple).
    pub fn new() -> Result<Self, GpuError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });
        let adapter = pollster_block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok_or(GpuError::NoAdapter)?;
        let (device, queue) = pollster_block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("promo-core"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
            },
            None,
        ))
        .map_err(|e| GpuError::Device(e.to_string()))?;
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
        })
    }

    /// The process-wide shared context. Multiple wgpu devices on one process
    /// are legal on macOS but panic inside Metal on the iOS simulator (fence
    /// creation for a second device) — and one device is cheaper anyway, so
    /// every production consumer (compositor FFI, preview engine) shares this.
    pub fn shared() -> Option<&'static GpuContext> {
        static SHARED: std::sync::OnceLock<Option<GpuContext>> = std::sync::OnceLock::new();
        SHARED.get_or_init(|| GpuContext::new().ok()).as_ref()
    }

    /// Human-readable adapter description (backend + name), for logs/gates.
    pub fn adapter_summary(&self) -> String {
        let info = self.adapter.get_info();
        format!("{:?} / {}", info.backend, info.name)
    }
}

/// Minimal single-future executor — avoids pulling an async runtime into the
/// core for what are one-shot setup calls.
fn pollster_block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

    struct ThreadWaker(std::thread::Thread);
    impl Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

    let mut fut = std::pin::pin!(fut);
    let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(out) => return out,
            Poll::Pending => std::thread::park(),
        }
    }
}
