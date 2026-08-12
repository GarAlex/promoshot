//! promo-engine: the conductor — preview engine (clock, prefetch, proxy
//! ladder, memory budgets) and export engine (streaming full-res pipeline).
//! P3 slice 1: MemoryGovernor + the host-provider preview engine (render the
//! composition at any time into an IOSurface, frames cached under budget).

pub mod governor;
pub mod mixer;
#[cfg(target_os = "macos")]
pub mod preview;

pub use governor::MemoryGovernor;
pub use mixer::{mix_chunk, MixInput};
#[cfg(target_os = "macos")]
pub use preview::{FrameProviderFn, PreviewEngine, PreviewStats, FLAG_PRE_FRAMED};
pub use promo_model::core_version;
