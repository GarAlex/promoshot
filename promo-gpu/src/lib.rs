//! promo-gpu: the wgpu compositor and per-platform GPU-surface import.
//!
//! P0 scope: device bring-up, the `GpuSurface` abstraction all codec backends
//! fill, and the IOSurface↔wgpu interop spike (macOS) proving the zero-copy
//! path: a VideoToolbox-style IOSurface imported as a wgpu texture, rendered
//! to by wgpu, and the result visible through the IOSurface's CPU mapping
//! without any pixel copy.

pub mod adapter;
pub mod compositor;
pub mod model_pass;
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
    /// The whole report: every adapter source tried, then the fix.
    #[error("{0}")]
    NoAdapter(String),
    #[error("device request failed: {0}")]
    Device(String),
    #[error("surface import failed: {0}")]
    Import(String),
    #[error("readback failed: {0}")]
    Readback(String),
}

/// A test's GPU, or `None` to skip it — except under `PROMO_REQUIRE_GPU=1`,
/// where a missing adapter FAILS the test. CI sets it: the runners have
/// lavapipe, and a pixel test that returns early without an adapter passes
/// having drawn nothing — about eighty did (review 2026-09-27, P2-27).
///
/// It is the process's one shared context, and a test draws on it rather
/// than bringing up its own with `GpuContext::new()`: the apps run one
/// device, and a test binary that ran many crashed. On Ubuntu 26.04
/// (Vulkan loader 1.4.341, Mesa 26.0.8 lavapipe) the promo-gpu tests, each
/// making and dropping its own instance and device on parallel threads,
/// died with SIGSEGV inside the loader's vkSetDebugUtilsObjectNameEXT
/// while other threads created or destroyed devices — 15 runs in 20; with
/// one shared device, 50 in 50 passed. Bookworm's loader 1.3.239 and Mesa
/// 22.3 never crashed, and neither did one test thread.
pub fn gpu_for_test() -> Option<&'static GpuContext> {
    match GpuContext::shared() {
        Some(ctx) => Some(ctx),
        None if std::env::var("PROMO_REQUIRE_GPU").as_deref() == Ok("1") => {
            panic!("PROMO_REQUIRE_GPU=1, and there is no GPU adapter")
        }
        None => {
            eprintln!("no GPU adapter; skipping");
            None
        }
    }
}

/// The core's GPU context: one device/queue shared by compositor and caches.
pub struct GpuContext {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl GpuContext {
    /// Brings up the best adapter there is (Metal on Apple; on Linux a GPU,
    /// else lavapipe — the system's or the release's own — else
    /// SwiftShader with a warning). See [`adapter`].
    pub fn new() -> Result<Self, GpuError> {
        let adapter::Chosen {
            instance,
            adapter,
            kind,
            bundled,
        } = adapter::choose().map_err(GpuError::NoAdapter)?;
        // Said once per process, on stderr: which CPU path a render took
        // when it was not the system's own driver.
        static SAID: std::sync::Once = std::sync::Once::new();
        if kind == adapter::AdapterKind::SwiftShader {
            SAID.call_once(|| {
                eprintln!("{}", adapter::swiftshader_warning(&adapter.get_info().name));
            });
        } else if let Some(dir) = &bundled {
            SAID.call_once(|| {
                eprintln!(
                    "promo: no usable system Vulkan driver; rendering on the bundled lavapipe ({})",
                    dir.display()
                );
            });
        }
        // Adapter-specific format features unlock MSAA counts past the
        // spec-guaranteed [1, 4] — the vector rasterizer wants 8, whose
        // finer coverage the stills golden gate measures. QUERYING the
        // adapter is not enough: without this device feature, pipeline
        // creation refuses 8 even where the adapter reports it (it did,
        // on D3D12 — Metal happened to allow it and hid the gap).
        let wanted = wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
        let (device, queue) = pollster_block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("promo-core"),
                required_features: adapter.features() & wanted,
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
        Self::shared_or_error().ok()
    }

    /// [`shared`](Self::shared), keeping why there is none: the adapters
    /// tried and the fix, for an error a person reads.
    pub fn shared_or_error() -> Result<&'static GpuContext, &'static str> {
        static SHARED: std::sync::OnceLock<Result<GpuContext, String>> = std::sync::OnceLock::new();
        SHARED
            .get_or_init(|| GpuContext::new().map_err(|e| e.to_string()))
            .as_ref()
            .map_err(|e| e.as_str())
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
