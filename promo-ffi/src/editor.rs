//! Editor-layer FFI (`promo-editor`).
//!
//! Editor calls are rare and small — a selection change, a repack, a viewport
//! move — so they cross as JSON, unlike the per-frame export scene which stays
//! flat binary. One boundary serves every non-Rust front end: Swift today,
//! a Windows app later. Rust front ends depend on `promo-editor` directly and
//! never come through here.

use promo_editor::TimelineViewport;
use promo_model::ProjectLayer;
use std::ffi::{c_char, CStr, CString};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ViewportParams {
    center: f64,
    /// A non-finite or absent span means "fit".
    span: Option<f64>,
    total: f64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackParams {
    layers: Vec<ProjectLayer>,
    total_duration: f64,
    #[serde(default)]
    gutter: f64,
    #[serde(default)]
    viewport: Option<ViewportParams>,
    #[serde(default)]
    always_include: Vec<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LaneOut {
    // Explicit: serde's camelCase gives `rowId`/`layerIds`, but every other
    // key this project exchanges with Swift capitalises ID (`resourceID`,
    // `imageCutID`). A mismatch here is silent — the decoder just yields
    // nothing — which is what the parity test caught.
    #[serde(rename = "rowID")]
    row_id: String,
    kind: String,
    index_within_kind: usize,
    #[serde(rename = "layerIDs")]
    layer_ids: Vec<String>,
}

/// Packs layers into lanes. Input JSON:
/// `{"layers": [...], "totalDuration": s, "gutter": s,
///   "viewport": {"center": s, "span": s|null, "total": s}|null,
///   "alwaysInclude": ["id", …]}`
///
/// Returns `[{"rowID", "kind", "indexWithinKind", "layerIDs"}]` as JSON; free
/// with `promo_string_free`. NULL on malformed input.
///
/// Safety contract (C ABI): `params_json` is a valid NUL-terminated string.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_lanes_pack(params_json: *const c_char) -> *mut c_char {
    if params_json.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(text) = unsafe { CStr::from_ptr(params_json) }.to_str() else {
        return std::ptr::null_mut();
    };
    let Ok(params) = serde_json::from_str::<PackParams>(text) else {
        return std::ptr::null_mut();
    };

    let viewport = params.viewport.map(|v| TimelineViewport {
        center: v.center,
        span: v.span.unwrap_or(f64::INFINITY),
        total: v.total,
    });
    let lanes = promo_editor::pack(
        &params.layers,
        params.total_duration,
        params.gutter,
        viewport.as_ref(),
        &params.always_include,
    );

    let out: Vec<LaneOut> = lanes
        .iter()
        .map(|lane| LaneOut {
            row_id: lane.row_id(),
            kind: lane.kind.as_str().to_string(),
            index_within_kind: lane.index_within_kind,
            layer_ids: lane.layers.iter().map(|l| l.id.clone()).collect(),
        })
        .collect();

    match serde_json::to_string(&out)
        .ok()
        .and_then(|s| CString::new(s).ok())
    {
        Some(c) => c.into_raw(),
        None => std::ptr::null_mut(),
    }
}

/// The row holding `layer_id` after packing the same input, or NULL when the
/// layer is not in the packed set (focus can drop it). Free with
/// `promo_string_free`.
///
/// Safety contract (C ABI): both arguments are valid NUL-terminated strings.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_lanes_row_id(
    params_json: *const c_char,
    layer_id: *const c_char,
) -> *mut c_char {
    if params_json.is_null() || layer_id.is_null() {
        return std::ptr::null_mut();
    }
    let (Ok(text), Ok(id)) = (
        unsafe { CStr::from_ptr(params_json) }.to_str(),
        unsafe { CStr::from_ptr(layer_id) }.to_str(),
    ) else {
        return std::ptr::null_mut();
    };
    let Ok(params) = serde_json::from_str::<PackParams>(text) else {
        return std::ptr::null_mut();
    };
    let viewport = params.viewport.map(|v| TimelineViewport {
        center: v.center,
        span: v.span.unwrap_or(f64::INFINITY),
        total: v.total,
    });
    let lanes = promo_editor::pack(
        &params.layers,
        params.total_duration,
        params.gutter,
        viewport.as_ref(),
        &params.always_include,
    );
    match promo_editor::row_id_containing(id, &lanes).and_then(|s| CString::new(s).ok()) {
        Some(c) => c.into_raw(),
        None => std::ptr::null_mut(),
    }
}

/// Whether lanes are worth offering at this timeline width, and whether their
/// labels collapse to icons — so every front end draws the same conclusion
/// from the same measurement instead of hardcoding its own breakpoints.
#[no_mangle]
pub extern "C" fn promo_lanes_fit(timeline_width: f64) -> i32 {
    promo_editor::TimelineLanePolicy::lanes_fit(timeline_width) as i32
}

#[no_mangle]
pub extern "C" fn promo_lanes_compact_labels(timeline_width: f64) -> i32 {
    promo_editor::TimelineLanePolicy::uses_compact_labels(timeline_width) as i32
}
