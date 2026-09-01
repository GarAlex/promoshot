//! The CLI's host machinery as a library.
//!
//! `render::Renderer` is the proven portable host for `promo-engine` — it
//! answers the frame provider from `promo-media` and disk, and takes pixels
//! out of a wgpu texture. The Windows front end drives this same host over
//! the C ABI (`promo-ffi` depends on this crate for it) rather than growing
//! a second host in C#: decode and compositing stay in Rust, and "does the
//! app render this right?" stays answerable by diffing two callers of one
//! implementation.

pub mod project;
pub mod render;
