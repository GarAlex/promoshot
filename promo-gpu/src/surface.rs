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
    ///
    /// **BGRA with PREMULTIPLIED alpha.** The compositor blends premultiplied
    /// (`One` / `OneMinusSrcAlpha`), so straight-alpha pixels make every
    /// partially transparent edge saturate: half-covered white becomes fully
    /// white, and antialiased text comes out with hard binary edges. Opaque
    /// content is unaffected either way, which is why this went unnoticed
    /// until the first texture with a soft edge arrived.
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

/// A `GpuSurface` after import: the wgpu texture plus whatever must stay alive
/// for that texture to remain valid.
///
/// Zero-copy adoption means the texture is a *view onto someone else's memory*
/// — on Apple the IOSurface must outlive it, so the retain is held here and
/// released on drop. CPU uploads own their pixels outright and keep nothing.
/// Callers hold an `ImportedFrame` and never think about it again.
pub struct ImportedFrame {
    pub texture: crate::compositor::InputTexture,
    pub width: u32,
    pub height: u32,
    /// Never read — it exists so its `Drop` runs when the frame dies.
    #[allow(dead_code)]
    keep_alive: KeepAlive,
}

impl ImportedFrame {
    pub(crate) fn owning(
        texture: crate::compositor::InputTexture,
        width: u32,
        height: u32,
        keep_alive: KeepAlive,
    ) -> Self {
        Self {
            texture,
            width,
            height,
            keep_alive,
        }
    }

    /// Bytes this frame occupies, for the memory governor.
    pub fn byte_size(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }
}

impl std::fmt::Debug for ImportedFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportedFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

/// What an imported frame must keep alive. One variant per surface kind that
/// has an ownership rule; everything else keeps nothing.
pub(crate) enum KeepAlive {
    /// Uploaded pixels — the texture owns its memory.
    Nothing,
    /// An adopted IOSurface, retained until this drops.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    IoSurface(*mut std::ffi::c_void),
}

impl Drop for KeepAlive {
    fn drop(&mut self) {
        match self {
            KeepAlive::Nothing => {}
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            KeepAlive::IoSurface(raw) => unsafe { crate::iosurface::release(*raw) },
        }
    }
}

// Same reasoning as GpuSurface: the handle is a number until an import module
// touches it, and the frame is used from one thread at a time.
unsafe impl Send for ImportedFrame {}
