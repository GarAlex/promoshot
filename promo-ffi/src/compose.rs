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
    #[serde(default)]
    color709: bool,
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
        match handle
            .compositor
            .import_iosurface_cached(handle.ctx, surface, w as u32, h as u32)
        {
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
                color_709: q.color709,
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

/// Doubles per quad in `promo_compose_frame_raw`'s flat layout.
pub const QUAD_DOUBLES: usize = 18;
/// Doubles in the frame header.
pub const HEADER_DOUBLES: usize = 12;

/// Binary-scene variant of `promo_compose_frame` — the hot export path calls
/// this 30× per output second, and JSON encode (host) + parse (core) was
/// measurable per-frame work that scales with layer count.
///
/// `header`: 12 doubles — canvasW, canvasH, backgroundRGBA[4], outputW,
/// outputH, barsRGBA[4].
/// `quads`: `quad_count` × 18 doubles — textureIndex (-1 = solid fill),
/// rect[4], rotation, cornerRadius, borderWidth, borderRGBA[4],
/// solidRGBA[4], opacity, color709 (non-zero: the texture is BT.709-encoded
/// video and the shader converts to sRGB after sampling).
/// Surfaces/output and return codes as in `promo_compose_frame`.
///
/// # Safety
/// Pointers must be valid for the stated lengths; surfaces must be BGRA
/// IOSurfaceRefs (contract documented in promo_core.h).
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)] // documented FFI contract, mirrors promo_compose_frame
pub extern "C" fn promo_compose_frame_raw(
    handle: *mut CompositorHandle,
    header: *const f64,
    quads: *const f64,
    quad_count: usize,
    surfaces: *const *mut c_void,
    surface_widths: *const c_int,
    surface_heights: *const c_int,
    surface_count: usize,
    output_surface: *mut c_void,
) -> c_int {
    let Some(handle) = (unsafe { handle.as_mut() }) else {
        return -1;
    };
    if header.is_null() || output_surface.is_null() || (quad_count > 0 && quads.is_null()) {
        return -1;
    }
    if surface_count > 0
        && (surfaces.is_null() || surface_widths.is_null() || surface_heights.is_null())
    {
        return -1;
    }
    let h = unsafe { std::slice::from_raw_parts(header, HEADER_DOUBLES) };
    let q = unsafe { std::slice::from_raw_parts(quads, quad_count * QUAD_DOUBLES) };

    let mut textures: Vec<InputTexture> = Vec::with_capacity(surface_count);
    for i in 0..surface_count {
        let (surface, w, hh) = unsafe {
            (
                *surfaces.add(i),
                *surface_widths.add(i),
                *surface_heights.add(i),
            )
        };
        if surface.is_null() || w <= 0 || hh <= 0 {
            return -3;
        }
        match handle
            .compositor
            .import_iosurface_cached(handle.ctx, surface, w as u32, hh as u32)
        {
            Ok(t) => textures.push(t),
            Err(_) => return -3,
        }
    }

    let scene = Scene {
        canvas_width: h[0],
        canvas_height: h[1],
        background_rgba: [h[2] as f32, h[3] as f32, h[4] as f32, h[5] as f32],
        output_width: h[6] as u32,
        output_height: h[7] as u32,
        bars_rgba: [h[8] as f32, h[9] as f32, h[10] as f32, h[11] as f32],
        quads: q
            .chunks_exact(QUAD_DOUBLES)
            .map(|c| SceneQuad {
                texture: if c[0] < 0.0 { None } else { Some(c[0] as usize) },
                rect: [c[1], c[2], c[3], c[4]],
                rotation_deg: c[5],
                corner_radius: c[6],
                border_width: c[7],
                border_rgba: [c[8] as f32, c[9] as f32, c[10] as f32, c[11] as f32],
                solid_rgba: [c[12] as f32, c[13] as f32, c[14] as f32, c[15] as f32],
                opacity: c[16] as f32,
                color_709: c[17] != 0.0,
            })
            .collect(),
    };
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

    /// The binary scene path must render exactly what the JSON path renders.
    #[test]
    fn compose_raw_matches_json() {
        let handle = promo_compositor_new();
        assert!(!handle.is_null(), "compositor");

        let input = OwnedIoSurface::new_bgra(4, 4).expect("input");
        input.write_pixels(&[0u8, 0, 255, 255].repeat(16)).unwrap(); // red
        let output = OwnedIoSurface::new_bgra(20, 10).expect("output");

        // Same scene as compose_through_ffi, flat-encoded.
        let header: [f64; HEADER_DOUBLES] = [
            10.0, 10.0, // canvas
            0.0, 1.0, 0.0, 1.0, // background green
            20.0, 10.0, // output
            0.0, 0.0, 1.0, 1.0, // bars blue
        ];
        let mut quad = [0.0f64; QUAD_DOUBLES];
        quad[0] = 0.0; // texture 0
        quad[1..5].copy_from_slice(&[2.5, 2.5, 5.0, 5.0]);
        quad[16] = 1.0; // opacity

        let surfaces = [input.raw()];
        let widths = [4i32];
        let heights = [4i32];
        let rc = promo_compose_frame_raw(
            handle,
            header.as_ptr(),
            quad.as_ptr(),
            1,
            surfaces.as_ptr(),
            widths.as_ptr(),
            heights.as_ptr(),
            1,
            output.raw(),
        );
        assert_eq!(rc, 0, "raw compose rc");

        let px = output.read_pixels().unwrap();
        let at = |x: usize, y: usize| {
            let i = (y * 20 + x) * 4;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };
        assert_eq!(at(1, 5), [255, 0, 0, 255], "left bar blue");
        assert_eq!(at(6, 1), [0, 255, 0, 255], "canvas green");
        assert_eq!(at(10, 5), [0, 0, 255, 255], "quad red");

        // Solid quad (-1 texture) over the top-left corner.
        let mut solid = [0.0f64; QUAD_DOUBLES];
        solid[0] = -1.0;
        solid[1..5].copy_from_slice(&[0.0, 0.0, 2.0, 2.0]);
        solid[12..16].copy_from_slice(&[1.0, 1.0, 1.0, 1.0]); // white
        solid[16] = 1.0;
        let rc = promo_compose_frame_raw(
            handle,
            header.as_ptr(),
            solid.as_ptr(),
            1,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            output.raw(),
        );
        assert_eq!(rc, 0, "solid raw compose rc");
        let px = output.read_pixels().unwrap();
        let i = (1 * 20 + 6) * 4;
        assert_eq!(&px[i..i + 4], &[255, 255, 255, 255], "solid white quad");

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
