//! Vector-drawing FFI (macOS): the host hands over a `DrawingDocument` JSON
//! payload and a BGRA IOSurface; the core tessellates the shapes and renders
//! them on the GPU at that surface's resolution.
//!
//! This replaces the host's Core Graphics rasterization into a fixed-size
//! bitmap, so a drawing can be re-rendered crisply at whatever size the frame
//! needs instead of magnifying a pre-baked one.

#![cfg(any(target_os = "macos", target_os = "ios"))]

use promo_gpu::vector::{VectorRenderer, VectorShape, VectorShapeKind};
use promo_gpu::GpuContext;
use promo_model::{DrawingDocument, DrawingShapeKind};
use std::ffi::{c_char, c_int, c_void, CStr};

/// Opaque to C: GPU context + vector pipeline, reused across renders.
pub struct VectorHandle {
    ctx: &'static GpuContext,
    renderer: VectorRenderer,
}

/// Creates a vector renderer. Null when no GPU is available.
/// Free with `promo_vector_free`.
#[no_mangle]
pub extern "C" fn promo_vector_new() -> *mut VectorHandle {
    crate::ffi_guard(std::ptr::null_mut(), move || {
        let Some(ctx) = GpuContext::shared() else {
            return std::ptr::null_mut();
        };
        let Ok(renderer) = VectorRenderer::new(ctx) else {
            return std::ptr::null_mut();
        };
        Box::into_raw(Box::new(VectorHandle { ctx, renderer }))
    })
}

/// Frees a vector renderer. Null is a no-op.
///
/// Safety contract (C ABI): `handle` must be a pointer this library
/// returned, freed at most once.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_vector_free(handle: *mut VectorHandle) {
    crate::ffi_guard((), move || {
        if !handle.is_null() {
            drop(unsafe { Box::from_raw(handle) });
        }
    })
}

/// Renders a drawing into `output_surface` (BGRA IOSurface, width×height).
/// The drawing's content bounds are scaled to fill the surface — the same
/// mapping the host's CG rasterizer used. The surface is cleared to
/// transparent first; the drawing composites over the frame as a quad.
///
/// 0 ok, -1 bad input, -2 document parse failed, -4 render failed.
///
/// Safety contract (C ABI): `doc_json` is NUL-terminated UTF-8;
/// `output_surface` is a BGRA IOSurfaceRef of the stated size.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_vector_render(
    handle: *mut VectorHandle,
    doc_json: *const c_char,
    output_surface: *mut c_void,
    width: c_int,
    height: c_int,
) -> c_int {
    crate::ffi_guard(-4, move || {
        let Some(handle) = (unsafe { handle.as_mut() }) else {
            return -1;
        };
        if doc_json.is_null() || output_surface.is_null() || width <= 0 || height <= 0 {
            return -1;
        }
        let Ok(text) = unsafe { CStr::from_ptr(doc_json) }.to_str() else {
            return -1;
        };
        let Ok(doc) = serde_json::from_str::<DrawingDocument>(text) else {
            return -2;
        };
        let shapes = vector_shapes(&doc);
        match handle.renderer.render_to_iosurface(
            handle.ctx,
            &shapes,
            output_surface,
            width as u32,
            height as u32,
        ) {
            Ok(()) => 0,
            Err(_) => -4,
        }
    })
}

/// The drawing's natural size (content bounds), so the host can pick a
/// render resolution and place the quad. Writes `out[4]` = x, y, w, h.
/// 0 ok, -1 bad input, -2 parse failed.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_vector_content_bounds(doc_json: *const c_char, out: *mut f64) -> c_int {
    crate::ffi_guard(-1, move || {
        if doc_json.is_null() || out.is_null() {
            return -1;
        }
        let Ok(text) = unsafe { CStr::from_ptr(doc_json) }.to_str() else {
            return -1;
        };
        let Ok(doc) = serde_json::from_str::<DrawingDocument>(text) else {
            return -2;
        };
        let (x, y, w, h) = promo_gpu::vector::content_bounds(&vector_shapes(&doc));
        unsafe {
            *out = x;
            *out.add(1) = y;
            *out.add(2) = w;
            *out.add(3) = h;
        }
        0
    })
}

/// Model shapes → renderer shapes, resolving hex colors and opacities the
/// way the Swift rasterizer does (missing opacity = 1, unparseable stroke
/// color = white, no `fillColorHex` = no fill).
fn vector_shapes(doc: &DrawingDocument) -> Vec<VectorShape> {
    doc.shapes
        .iter()
        .map(|s| {
            let stroke_alpha = s.stroke_opacity.unwrap_or(1.0);
            let fill_alpha = s.fill_opacity.unwrap_or(1.0);
            VectorShape {
                kind: match s.kind {
                    DrawingShapeKind::Pen => VectorShapeKind::Pen,
                    DrawingShapeKind::Line => VectorShapeKind::Line,
                    DrawingShapeKind::Oval => VectorShapeKind::Oval,
                },
                points: s.points.iter().map(|p| (p.x(), p.y())).collect(),
                stroke_rgba: rgba_from_hex(&s.stroke_color_hex)
                    .map(|c| [c[0], c[1], c[2], stroke_alpha])
                    .unwrap_or([1.0, 1.0, 1.0, stroke_alpha]),
                stroke_width: s.stroke_width,
                fill_rgba: s
                    .fill_color_hex
                    .as_deref()
                    .and_then(rgba_from_hex)
                    .map(|c| [c[0], c[1], c[2], fill_alpha]),
                arrow_start: s.arrow_start,
                arrow_end: s.arrow_end,
                even_odd_fill: s.even_odd_fill.unwrap_or(false),
            }
        })
        .collect()
}

/// `#RRGGBB` / `RRGGBB` → linear-free sRGB components. `None` when the
/// string isn't a 6-digit hex color (mirrors Swift's optional CGColor).
fn rgba_from_hex(hex: &str) -> Option<[f32; 4]> {
    let value = hex.trim().trim_start_matches('#').to_uppercase();
    if value.len() != 6 {
        return None;
    }
    let parsed = u32::from_str_radix(&value, 16).ok()?;
    Some([
        ((parsed >> 16) & 0xFF) as f32 / 255.0,
        ((parsed >> 8) & 0xFF) as f32 / 255.0,
        (parsed & 0xFF) as f32 / 255.0,
        1.0,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use promo_gpu::iosurface::OwnedIoSurface;
    use std::ffi::CString;

    const DOC: &str = r#"{"shapes":[
        {"id":"S1","kind":"pen","points":[[0,0],[100,0],[100,100],[0,100],[0,0]],
         "strokeColorHex":"FF0000","strokeWidth":6,
         "fillColorHex":"0000FF","arrowStart":false,"arrowEnd":false}
    ]}"#;

    #[test]
    fn renders_a_filled_stroked_shape() {
        let handle = promo_vector_new();
        assert!(!handle.is_null(), "vector renderer");
        let output = OwnedIoSurface::new_bgra(100, 100).expect("surface");
        let json = CString::new(DOC).unwrap();
        let rc = promo_vector_render(handle, json.as_ptr(), output.raw(), 100, 100);
        assert_eq!(rc, 0, "render rc");

        let px = output.read_pixels().unwrap();
        let at = |x: usize, y: usize| {
            let i = (y * 100 + x) * 4;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };
        // Interior: blue fill (BGRA).
        assert_eq!(at(50, 50), [255, 0, 0, 255], "fill");
        // Edge: red stroke.
        assert_eq!(at(50, 1), [0, 0, 255, 255], "stroke");
        promo_vector_free(handle);
    }

    #[test]
    fn reports_content_bounds() {
        let json = CString::new(DOC).unwrap();
        let mut out = [0.0f64; 4];
        assert_eq!(
            promo_vector_content_bounds(json.as_ptr(), out.as_mut_ptr()),
            0
        );
        assert_eq!(out, [0.0, 0.0, 100.0, 100.0]);
    }

    #[test]
    fn bad_inputs_are_safe() {
        assert_eq!(
            promo_vector_render(
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
                0
            ),
            -1
        );
        assert_eq!(
            promo_vector_content_bounds(std::ptr::null(), std::ptr::null_mut()),
            -1
        );
        let handle = promo_vector_new();
        let output = OwnedIoSurface::new_bgra(4, 4).expect("surface");
        let bad = CString::new("{ not json").unwrap();
        assert_eq!(
            promo_vector_render(handle, bad.as_ptr(), output.raw(), 4, 4),
            -2
        );
        promo_vector_free(handle);
        promo_vector_free(std::ptr::null_mut());
    }
}
