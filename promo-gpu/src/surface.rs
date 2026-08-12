//! The `GpuSurface` abstraction: how decoded frames arrive from a codec
//! backend. One variant per platform surface kind; `promo-gpu` owns one
//! import module per variant, and everything downstream sees only a wgpu
//! texture. Variants for platforms this build doesn't target still exist as
//! types (capability negotiation names them), only their import is gated.

/// A GPU-resident (or CPU-fallback) video frame handed over by a codec
/// backend. Raw handles are plain pointers/ids at this layer; ownership and
/// lifetime rules are documented per backend (the conformance suite tests
/// them).
#[derive(Debug)]
pub enum GpuSurface {
    /// Apple: an `IOSurfaceRef` (VideoToolbox decode output is IOSurface
    /// backed). Imported zero-copy as a Metal texture.
    IoSurface { raw: *mut std::ffi::c_void },
    /// Windows: an NT shared handle exported from the D3D11 decode texture,
    /// imported into DX12/wgpu.
    D3DSharedHandle { raw: *mut std::ffi::c_void },
    /// Linux: a DMA-BUF file descriptor (VAAPI), imported via Vulkan external
    /// memory.
    DmaBuf { fd: i32 },
    /// Universal fallback (software decode): tightly-packed pixels uploaded
    /// through the staging ring. The slow path — never used when a GPU
    /// surface is available.
    CpuPixels {
        data: Vec<u8>,
        width: u32,
        height: u32,
        bytes_per_row: u32,
    },
}

// The raw-handle variants are just numbers until an import module touches
// them on the right thread; moving the enum between threads is safe.
unsafe impl Send for GpuSurface {}
