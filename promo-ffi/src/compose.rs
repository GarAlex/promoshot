//! Compositor FFI (Phase 2, macOS): the host describes one frame as JSON
//! (canvas, letterbox output, colors, z-ordered quads) plus an array of BGRA
//! IOSurfaces; the core renders on the GPU into the output IOSurface. No
//! full-resolution pixels cross this boundary as CPU bytes — surfaces do.

#![cfg(any(target_os = "macos", target_os = "ios"))]

use promo_gpu::compositor::{Compositor, InputTexture, Scene, SceneQuad};
use promo_gpu::GpuContext;
use serde::Deserialize;
use std::ffi::{c_char, c_int, c_void, CStr};

/// Opaque to C: GPU context + pipeline, reused across frames.
pub struct CompositorHandle {
    ctx: &'static GpuContext,
    compositor: Compositor,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuadWire {
    #[serde(default)]
    texture: Option<usize>,
    rect: [f64; 4],
    #[serde(default)]
    rotation: f64,
    #[serde(default)]
    corner_radius: f64,
    #[serde(default)]
    border_width: f64,
    #[serde(default, rename = "borderRGBA")]
    border_rgba: [f32; 4],
    #[serde(default, rename = "solidRGBA")]
    solid_rgba: [f32; 4],
    #[serde(default = "one")]
    opacity: f32,
}

fn one() -> f32 {
    1.0
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SceneWire {
    canvas_width: f64,
    canvas_height: f64,
    #[serde(rename = "backgroundRGBA")]
    background_rgba: [f32; 4],
    output_width: u32,
    output_height: u32,
    #[serde(rename = "barsRGBA")]
    bars_rgba: [f32; 4],
    quads: Vec<QuadWire>,
}

/// Creates a compositor (GPU bring-up + pipeline). Null when no GPU is
/// available. Free with `promo_compositor_free`.
#[no_mangle]
pub extern "C" fn promo_compositor_new() -> *mut CompositorHandle {
    let Some(ctx) = GpuContext::shared() else {
        return std::ptr::null_mut();
    };
    let Ok(compositor) = Compositor::new(ctx) else {
        return std::ptr::null_mut();
    };
    Box::into_raw(Box::new(CompositorHandle { ctx, compositor }))
}

/// Frees a compositor. Null is a no-op.
///
/// Safety contract (C ABI): `handle` must be a pointer this library
/// returned, freed at most once.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_compositor_free(handle: *mut CompositorHandle) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle) });
    }
}

/// Renders one frame. `scene_json` (NUL-terminated) describes the frame;
/// quads reference `surfaces[i]` (BGRA IOSurfaceRefs with matching
/// `surface_widths`/`surface_heights`); `output_surface` must be a BGRA
/// IOSurface of outputWidth×outputHeight.
///
/// Returns 0 ok, -1 bad input, -2 scene parse failed, -3 import failed,
/// -4 render failed.
///
/// Safety contract (C ABI): pointers must be valid per the description;
/// the arrays hold `surface_count` elements.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_compose_frame(
    handle: *mut CompositorHandle,
    scene_json: *const c_char,
    surfaces: *const *mut c_void,
    surface_widths: *const c_int,
    surface_heights: *const c_int,
    surface_count: usize,
    output_surface: *mut c_void,
) -> c_int {
    let Some(handle) = (unsafe { handle.as_mut() }) else {
        return -1;
    };
    if scene_json.is_null() || output_surface.is_null() {
        return -1;
    }
    if surface_count > 0
        && (surfaces.is_null() || surface_widths.is_null() || surface_heights.is_null())
    {
        return -1;
    }
    let Ok(text) = unsafe { CStr::from_ptr(scene_json) }.to_str() else {
        return -1;
    };
    let Ok(wire) = serde_json::from_str::<SceneWire>(text) else {
        return -2;
    };

    let mut textures: Vec<InputTexture> = Vec::with_capacity(surface_count);
    for i in 0..surface_count {
        let (surface, w, h) = unsafe {
            (
                *surfaces.add(i),
                *surface_widths.add(i),
                *surface_heights.add(i),
            )
        };
        if surface.is_null() || w <= 0 || h <= 0 {
            return -3;
        }
        match Compositor::import_iosurface(handle.ctx, surface, w as u32, h as u32) {
            Ok(t) => textures.push(t),
            Err(_) => return -3,
        }
    }

    let scene = Scene {
        canvas_width: wire.canvas_width,
        canvas_height: wire.canvas_height,
        background_rgba: wire.background_rgba,
        output_width: wire.output_width,
        output_height: wire.output_height,
        bars_rgba: wire.bars_rgba,
        quads: wire
            .quads
            .iter()
            .map(|q| SceneQuad {
                texture: q.texture,
                rect: q.rect,
                rotation_deg: q.rotation,
                corner_radius: q.corner_radius,
                border_width: q.border_width,
                border_rgba: q.border_rgba,
                solid_rgba: q.solid_rgba,
                opacity: q.opacity,
            })
            .collect(),
    };
    // Out-of-range texture indices fail the render below with Import.
    match handle
        .compositor
        .compose_to_iosurface(handle.ctx, &scene, &textures, output_surface)
    {
        Ok(()) => 0,
        Err(promo_gpu::GpuError::Import(_)) => -3,
        Err(_) => -4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use promo_gpu::iosurface::OwnedIoSurface;
    use std::ffi::CString;

    #[test]
    fn compose_through_ffi() {
        let handle = promo_compositor_new();
        assert!(!handle.is_null(), "compositor");

        let input = OwnedIoSurface::new_bgra(4, 4).expect("input");
        input.write_pixels(&[0u8, 0, 255, 255].repeat(16)).unwrap(); // red
        let output = OwnedIoSurface::new_bgra(20, 10).expect("output");

        let scene = CString::new(
            r#"{
            "canvasWidth": 10, "canvasHeight": 10,
            "backgroundRGBA": [0, 1, 0, 1],
            "outputWidth": 20, "outputHeight": 10,
            "barsRGBA": [0, 0, 1, 1],
            "quads": [
              {"texture": 0, "rect": [2.5, 2.5, 5, 5]}
            ]}"#,
        )
        .unwrap();

        let surfaces = [input.raw()];
        let widths = [4i32];
        let heights = [4i32];
        let rc = promo_compose_frame(
            handle,
            scene.as_ptr(),
            surfaces.as_ptr(),
            widths.as_ptr(),
            heights.as_ptr(),
            1,
            output.raw(),
        );
        assert_eq!(rc, 0, "compose rc");

        let px = output.read_pixels().unwrap();
        let at = |x: usize, y: usize| {
            let i = (y * 20 + x) * 4;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };
        assert_eq!(at(1, 5), [255, 0, 0, 255], "left bar blue");
        assert_eq!(at(6, 1), [0, 255, 0, 255], "canvas green");
        assert_eq!(at(10, 5), [0, 0, 255, 255], "quad red");

        promo_compositor_free(handle);
    }

    #[test]
    fn bad_inputs_are_safe() {
        assert_eq!(
            promo_compose_frame(
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                std::ptr::null_mut()
            ),
            -1
        );
        promo_compositor_free(std::ptr::null_mut());
        let handle = promo_compositor_new();
        assert!(!handle.is_null());
        let bogus = CString::new("nope").unwrap();
        let out = OwnedIoSurface::new_bgra(4, 4).unwrap();
        assert_eq!(
            promo_compose_frame(
                handle,
                bogus.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                out.raw()
            ),
            -2
        );
        promo_compositor_free(handle);
    }
}
