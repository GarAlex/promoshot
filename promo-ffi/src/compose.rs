//! Compositor FFI (Phase 2, macOS): the host describes one frame as JSON
//! (canvas, letterbox output, colors, z-ordered quads) plus an array of BGRA
//! IOSurfaces; the core renders on the GPU into the output IOSurface. No
//! full-resolution pixels cross this boundary as CPU bytes — surfaces do.

#![cfg(any(target_os = "macos", target_os = "ios"))]

use promo_gpu::compositor::{Compositor, Fence, InputTexture, Scene, SceneQuad};
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
    crate::ffi_guard(std::ptr::null_mut(), move || {
        let Some(ctx) = GpuContext::shared() else {
            return std::ptr::null_mut();
        };
        let Ok(compositor) = Compositor::new(ctx) else {
            return std::ptr::null_mut();
        };
        Box::into_raw(Box::new(CompositorHandle { ctx, compositor }))
    })
}

/// Frees a compositor. Null is a no-op.
///
/// Safety contract (C ABI): `handle` must be a pointer this library
/// returned, freed at most once.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_compositor_free(handle: *mut CompositorHandle) {
    crate::ffi_guard((), move || {
        if !handle.is_null() {
            drop(unsafe { Box::from_raw(handle) });
        }
    })
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
    crate::ffi_guard(-4, move || {
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
    })
}

/// An in-flight GPU submission. Owned by the host between
/// `promo_compose_frame_raw_deferred` and `promo_submission_wait`; waiting
/// consumes it. Deliberately independent of the compositor handle so the
/// thread that waits never touches compositor state the producer is mutating.
pub struct SubmissionToken(Fence);

impl SubmissionToken {
    /// Crate-internal: the preview engine's deferred export render hands its
    /// fence out through the same token type, so the host has exactly one
    /// wait primitive (`promo_submission_wait`).
    pub(crate) fn new(fence: Fence) -> Self {
        Self(fence)
    }
}

/// Enables/disables deferred completion on a compositor. With it on,
/// `promo_compose_frame_raw_deferred` returns a token instead of blocking;
/// the caller must wait on that token before reading the output surface.
/// 0 ok, -1 bad handle.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_compositor_set_defer(handle: *mut CompositorHandle, defer: c_int) -> c_int {
    crate::ffi_guard(-1, move || {
        let Some(handle) = (unsafe { handle.as_mut() }) else {
            return -1;
        };
        handle.compositor.set_defer_completion(defer != 0);
        0
    })
}

/// Blocks until the submission completes, then frees the token. Passing NULL
/// is a no-op (a compose that produced no fence). 0 ok, -1 no GPU.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_submission_wait(token: *mut SubmissionToken) -> c_int {
    crate::ffi_guard(-1, move || {
        if token.is_null() {
            return 0;
        }
        let token = unsafe { Box::from_raw(token) };
        let Some(ctx) = GpuContext::shared() else {
            return -1;
        };
        token.0.wait(ctx);
        0
    })
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
/// `out_token` (optional): when the compositor has deferred completion on,
/// receives a `SubmissionToken*` the caller MUST pass to
/// `promo_submission_wait` before reading `output_surface` (NULL when the
/// compose already blocked).
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
    out_token: *mut *mut SubmissionToken,
) -> c_int {
    crate::ffi_guard(-4, move || {
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
        // A background-only frame has zero quads and may pass NULL — building a
        // slice from a null pointer is UB even for length 0.
        let q: &[f64] = if quad_count == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(quads, quad_count * QUAD_DOUBLES) }
        };

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
                    texture: if c[0] < 0.0 {
                        None
                    } else {
                        Some(c[0] as usize)
                    },
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
            Ok(()) => {
                if !out_token.is_null() {
                    let token = handle
                        .compositor
                        .take_fence()
                        .map(|f| Box::into_raw(Box::new(SubmissionToken(f))))
                        .unwrap_or(std::ptr::null_mut());
                    unsafe { *out_token = token };
                }
                0
            }
            Err(promo_gpu::GpuError::Import(_)) => -3,
            Err(_) => -4,
        }
    })
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
            std::ptr::null_mut(),
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
            std::ptr::null_mut(),
        );
        assert_eq!(rc, 0, "solid raw compose rc");
        let px = output.read_pixels().unwrap();
        let i = (20 + 6) * 4; // row 1, col 6
        assert_eq!(&px[i..i + 4], &[255, 255, 255, 255], "solid white quad");

        promo_compositor_free(handle);
    }

    /// Deferred compose must hand back a fence and, once waited on, produce
    /// exactly the pixels the blocking path produces.
    #[test]
    fn deferred_compose_matches_blocking_after_wait() {
        let handle = promo_compositor_new();
        assert!(!handle.is_null(), "compositor");

        let input = OwnedIoSurface::new_bgra(4, 4).expect("input");
        input.write_pixels(&[0u8, 0, 255, 255].repeat(16)).unwrap(); // red
        let header: [f64; HEADER_DOUBLES] = [
            10.0, 10.0, 0.0, 1.0, 0.0, 1.0, 20.0, 10.0, 0.0, 0.0, 1.0, 1.0,
        ];
        let mut quad = [0.0f64; QUAD_DOUBLES];
        quad[1..5].copy_from_slice(&[2.5, 2.5, 5.0, 5.0]);
        quad[16] = 1.0;
        let surfaces = [input.raw()];
        let widths = [4i32];
        let heights = [4i32];

        let blocking_out = OwnedIoSurface::new_bgra(20, 10).expect("out");
        let rc = promo_compose_frame_raw(
            handle,
            header.as_ptr(),
            quad.as_ptr(),
            1,
            surfaces.as_ptr(),
            widths.as_ptr(),
            heights.as_ptr(),
            1,
            blocking_out.raw(),
            std::ptr::null_mut(),
        );
        assert_eq!(rc, 0);
        let expected = blocking_out.read_pixels().unwrap();

        assert_eq!(promo_compositor_set_defer(handle, 1), 0);
        let deferred_out = OwnedIoSurface::new_bgra(20, 10).expect("out");
        let mut token: *mut SubmissionToken = std::ptr::null_mut();
        let rc = promo_compose_frame_raw(
            handle,
            header.as_ptr(),
            quad.as_ptr(),
            1,
            surfaces.as_ptr(),
            widths.as_ptr(),
            heights.as_ptr(),
            1,
            deferred_out.raw(),
            &mut token,
        );
        assert_eq!(rc, 0);
        assert!(!token.is_null(), "deferred compose must return a fence");
        assert_eq!(promo_submission_wait(token), 0);
        assert_eq!(
            deferred_out.read_pixels().unwrap(),
            expected,
            "deferred pixels must match the blocking render"
        );

        // Waiting on NULL (a blocking compose produced no fence) is a no-op.
        assert_eq!(promo_submission_wait(std::ptr::null_mut()), 0);
        assert_eq!(promo_compositor_set_defer(handle, 0), 0);
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
