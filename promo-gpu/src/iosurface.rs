//! macOS IOSurface interop: create/look at IOSurfaces and wrap them as Metal
//! textures. This is the import module behind `GpuSurface::IoSurface`.
//!
//! The framework C API is declared directly (it is tiny and ABI-stable) so we
//! control locking and base-address access precisely — that CPU visibility is
//! how the spike PROVES the render was zero-copy: wgpu draws into the texture,
//! and the same bytes appear through the IOSurface mapping.

#![allow(non_snake_case)]

use core_foundation::base::TCFType;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use std::ffi::c_void;

pub type IOSurfaceRef = *mut c_void;

#[link(name = "IOSurface", kind = "framework")]
extern "C" {
    fn IOSurfaceCreate(properties: *const c_void) -> IOSurfaceRef;
    fn IOSurfaceLock(buffer: IOSurfaceRef, options: u32, seed: *mut u32) -> i32;
    fn IOSurfaceUnlock(buffer: IOSurfaceRef, options: u32, seed: *mut u32) -> i32;
    fn IOSurfaceGetBaseAddress(buffer: IOSurfaceRef) -> *mut c_void;
    fn IOSurfaceGetBytesPerRow(buffer: IOSurfaceRef) -> usize;
    fn IOSurfaceGetWidth(buffer: IOSurfaceRef) -> usize;
    fn IOSurfaceGetHeight(buffer: IOSurfaceRef) -> usize;
    /// Stable per-surface identity; never recycled while a retain is held.
    pub fn IOSurfaceGetID(buffer: IOSurfaceRef) -> u32;
}

const K_IOSURFACE_LOCK_READ_ONLY: u32 = 1;

/// An owned BGRA8 IOSurface (test/spike helper; production surfaces arrive
/// from VideoToolbox via the FFI instead).
pub struct OwnedIoSurface {
    raw: IOSurfaceRef,
    pub width: usize,
    pub height: usize,
}

// An IOSurface is a kernel object; the ref is thread-agnostic.
unsafe impl Send for OwnedIoSurface {}

impl OwnedIoSurface {
    /// Creates a BGRA8 IOSurface of the given size.
    pub fn new_bgra(width: usize, height: usize) -> Result<Self, super::GpuError> {
        let bytes_per_element = 4i64;
        let props = CFDictionary::from_CFType_pairs(&[
            (
                CFString::from_static_string("IOSurfaceWidth").as_CFType(),
                CFNumber::from(width as i64).as_CFType(),
            ),
            (
                CFString::from_static_string("IOSurfaceHeight").as_CFType(),
                CFNumber::from(height as i64).as_CFType(),
            ),
            (
                CFString::from_static_string("IOSurfaceBytesPerElement").as_CFType(),
                CFNumber::from(bytes_per_element).as_CFType(),
            ),
            (
                // 'BGRA' fourcc.
                CFString::from_static_string("IOSurfacePixelFormat").as_CFType(),
                CFNumber::from(0x42475241i64).as_CFType(),
            ),
        ]);
        let raw = unsafe { IOSurfaceCreate(props.as_concrete_TypeRef() as *const c_void) };
        if raw.is_null() {
            return Err(super::GpuError::Import(
                "IOSurfaceCreate returned null".into(),
            ));
        }
        Ok(Self { raw, width, height })
    }

    pub fn raw(&self) -> IOSurfaceRef {
        self.raw
    }

    /// Fills the surface from tightly-packed BGRA bytes through the CPU
    /// mapping (host-upload path for still-image layer content and tests).
    pub fn write_pixels(&self, pixels: &[u8]) -> Result<(), super::GpuError> {
        if pixels.len() != self.width * self.height * 4 {
            return Err(super::GpuError::Import(format!(
                "write_pixels: got {} bytes, want {}",
                pixels.len(),
                self.width * self.height * 4
            )));
        }
        unsafe {
            let mut seed = 0u32;
            if IOSurfaceLock(self.raw, 0, &mut seed) != 0 {
                return Err(super::GpuError::Import("IOSurfaceLock failed".into()));
            }
            let base = IOSurfaceGetBaseAddress(self.raw) as *mut u8;
            let stride = IOSurfaceGetBytesPerRow(self.raw);
            for row in 0..self.height {
                let src = &pixels[row * self.width * 4..(row + 1) * self.width * 4];
                std::ptr::copy_nonoverlapping(src.as_ptr(), base.add(row * stride), src.len());
            }
            IOSurfaceUnlock(self.raw, 0, &mut seed);
            Ok(())
        }
    }

    /// Reads the surface's pixels through the CPU mapping (read-only lock).
    pub fn read_pixels(&self) -> Result<Vec<u8>, super::GpuError> {
        unsafe {
            let mut seed = 0u32;
            if IOSurfaceLock(self.raw, K_IOSURFACE_LOCK_READ_ONLY, &mut seed) != 0 {
                return Err(super::GpuError::Readback("IOSurfaceLock failed".into()));
            }
            let base = IOSurfaceGetBaseAddress(self.raw) as *const u8;
            let stride = IOSurfaceGetBytesPerRow(self.raw);
            let width = IOSurfaceGetWidth(self.raw);
            let height = IOSurfaceGetHeight(self.raw);
            let mut out = Vec::with_capacity(width * height * 4);
            for row in 0..height {
                let row_ptr = base.add(row * stride);
                out.extend_from_slice(std::slice::from_raw_parts(row_ptr, width * 4));
            }
            IOSurfaceUnlock(self.raw, K_IOSURFACE_LOCK_READ_ONLY, &mut seed);
            Ok(out)
        }
    }
}

impl Drop for OwnedIoSurface {
    fn drop(&mut self) {
        unsafe {
            core_foundation::base::CFRelease(self.raw as _);
        }
    }
}

/// Wraps an IOSurface as a Metal texture (`newTextureWithDescriptor:iosurface:plane:`).
/// metal-rs doesn't surface this initializer, so it is sent directly.
pub fn metal_texture_from_iosurface(
    device: &metal::Device,
    surface: IOSurfaceRef,
    width: usize,
    height: usize,
) -> Result<metal::Texture, super::GpuError> {
    use metal::foreign_types::ForeignType;
    use objc::{msg_send, sel, sel_impl};

    let descriptor = metal::TextureDescriptor::new();
    descriptor.set_texture_type(metal::MTLTextureType::D2);
    descriptor.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);
    descriptor.set_width(width as u64);
    descriptor.set_height(height as u64);
    descriptor.set_storage_mode(metal::MTLStorageMode::Shared);
    descriptor.set_usage(
        metal::MTLTextureUsage::RenderTarget
            | metal::MTLTextureUsage::ShaderRead
            | metal::MTLTextureUsage::ShaderWrite,
    );

    let raw_texture: *mut objc::runtime::Object = unsafe {
        msg_send![
            device.as_ptr() as *mut objc::runtime::Object,
            newTextureWithDescriptor: descriptor.as_ptr() as *mut objc::runtime::Object
            iosurface: surface
            plane: 0usize
        ]
    };
    if raw_texture.is_null() {
        return Err(super::GpuError::Import(
            "newTextureWithDescriptor:iosurface:plane: returned nil".into(),
        ));
    }
    Ok(unsafe { metal::Texture::from_ptr(raw_texture as *mut metal::MTLTexture) })
}
