//! promo-engine: the conductor — preview engine (clock, prefetch, proxy
//! ladder, memory budgets) and export engine (streaming full-res pipeline).
//! MemoryGovernor + the host-provider preview engine: render the composition
//! at any time, frames cached under budget. The provider hands frames over as
//! a `HostSurface` (IOSurface, DMA-BUF, D3D handle or plain pixels), so the
//! engine names no platform and builds everywhere.

pub mod governor;
pub mod mixer;
pub mod preview;

pub use governor::MemoryGovernor;
pub use mixer::{mix_chunk, MixInput};
pub use preview::{
    FrameProviderFn, HostSurface, PreviewEngine, PreviewStats, FLAG_PRE_FRAMED, SURFACE_CPU_PIXELS,
    SURFACE_D3D_HANDLE, SURFACE_DMABUF, SURFACE_IOSURFACE, SURFACE_NONE,
};
pub use promo_model::core_version;

pub mod vector;
