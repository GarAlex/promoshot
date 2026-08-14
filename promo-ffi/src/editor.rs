//! Editor-layer FFI (`promo-editor`).
//!
//! Editor calls are rare and small — a selection change, a repack, a viewport
//! move — so they cross as JSON, unlike the per-frame export scene which stays
//! flat binary. One boundary serves every non-Rust front end: Swift today,
//! a Windows app later. Rust front ends depend on `promo-editor` directly and
//! never come through here.
//!
//! **Key casing**: serde's `camelCase` renders `row_id` as `rowId`, but every
//! key this project exchanges with Swift capitalises ID — `resourceID`,
//! `imageCutID`, `selectedID`. Any field ending in `_id`/`_ids` therefore
//! needs an explicit `#[serde(rename)]`. Getting it wrong is silent: a Swift
//! decoder yields nothing rather than failing, so the feature simply does
//! nothing. Both parity suites here exist partly to catch exactly that.

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

// --- selection, pinning, reveal (Stage 1 slice 1.2) ----------------------
//
// Stateless on purpose. Stage 1 does not move ownership: Swift still holds the
// selection in `@State`, and passes it in. The core owns the *rules*, not the
// state — which is the whole distinction Stage 1 rests on.

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RevealParams {
    #[serde(flatten)]
    pack: PackParams,
    /// The layer to scroll to.
    target: String,
    /// True when the lane view is showing (anchor is a row), false for the
    /// classic list (anchor is the layer itself).
    uses_lanes: bool,
}

/// Resolves what to scroll to when revealing a layer. Input is
/// [`promo_lanes_pack`]'s JSON plus `{"target": "id", "usesLanes": bool}`.
///
/// Returns `{"kind": "row"|"layer", "value": "…"}`, or NULL when focus has
/// dropped the layer and there is nothing to scroll to. Free with
/// `promo_string_free`.
///
/// Safety contract (C ABI): `params_json` is a valid NUL-terminated string.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_selection_reveal_anchor(params_json: *const c_char) -> *mut c_char {
    if params_json.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(text) = (unsafe { CStr::from_ptr(params_json) }).to_str() else {
        return std::ptr::null_mut();
    };
    let Ok(params) = serde_json::from_str::<RevealParams>(text) else {
        return std::ptr::null_mut();
    };

    let mut selection = promo_editor::Selection::new();
    selection.request_reveal(&params.target);

    let anchor = if params.uses_lanes {
        let viewport = params.pack.viewport.as_ref().map(|v| TimelineViewport {
            center: v.center,
            span: v.span.unwrap_or(f64::INFINITY),
            total: v.total,
        });
        let lanes = promo_editor::pack(
            &params.pack.layers,
            params.pack.total_duration,
            params.pack.gutter,
            viewport.as_ref(),
            &params.pack.always_include,
        );
        selection.take_reveal(Some(&lanes))
    } else {
        selection.take_reveal(None)
    };

    let (kind, value) = match anchor {
        Some(promo_editor::RevealAnchor::Row(row)) => ("row", row),
        Some(promo_editor::RevealAnchor::Layer(id)) => ("layer", id),
        None => return std::ptr::null_mut(),
    };
    match serde_json::to_string(&serde_json::json!({"kind": kind, "value": value}))
        .ok()
        .and_then(|s| CString::new(s).ok())
    {
        Some(c) => c.into_raw(),
        None => std::ptr::null_mut(),
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PinnedParams {
    layer: ProjectLayer,
    #[serde(rename = "selectedID")]
    selected_id: Option<String>,
    viewport: ViewportParams,
    window_active: bool,
}

/// Is this layer on screen only because it is the pinned selection? 1 = yes.
///
/// Input: `{"layer": {...}, "selectedID": "id"|null,
///          "viewport": {...}, "windowActive": bool}`.
///
/// Safety contract (C ABI): `params_json` is a valid NUL-terminated string.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_selection_is_pinned_outside_window(params_json: *const c_char) -> i32 {
    if params_json.is_null() {
        return 0;
    }
    let Ok(text) = (unsafe { CStr::from_ptr(params_json) }).to_str() else {
        return 0;
    };
    let Ok(params) = serde_json::from_str::<PinnedParams>(text) else {
        return 0;
    };
    let mut selection = promo_editor::Selection::new();
    if let Some(id) = &params.selected_id {
        selection.select(id);
    }
    let viewport = TimelineViewport {
        center: params.viewport.center,
        span: params.viewport.span.unwrap_or(f64::INFINITY),
        total: params.viewport.total,
    };
    selection.is_pinned_outside_window(&params.layer, &viewport, params.window_active) as i32
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReconcileParams {
    /// Layer ids in display order.
    #[serde(rename = "orderedIDs")]
    ordered_ids: Vec<String>,
    #[serde(rename = "selectedID")]
    selected_id: Option<String>,
}

/// The selection after the layer list changed: unchanged if still present,
/// otherwise the first layer, or NULL for an empty project. Free a non-NULL
/// result with `promo_string_free`.
///
/// Safety contract (C ABI): `params_json` is a valid NUL-terminated string.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_selection_reconcile(params_json: *const c_char) -> *mut c_char {
    if params_json.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(text) = (unsafe { CStr::from_ptr(params_json) }).to_str() else {
        return std::ptr::null_mut();
    };
    let Ok(params) = serde_json::from_str::<ReconcileParams>(text) else {
        return std::ptr::null_mut();
    };
    // reconcile() works on layers; ids are all it actually reads, so build the
    // thinnest thing that carries them.
    let ordered: Vec<ProjectLayer> = params
        .ordered_ids
        .iter()
        .enumerate()
        .filter_map(|(i, id)| {
            serde_json::from_value(serde_json::json!({
                "id": id, "name": id, "sortIndex": i, "kind": "image",
                "isEnabled": true, "startTime": 0.0, "keyframes": []
            }))
            .ok()
        })
        .collect();
    let mut selection = promo_editor::Selection::new();
    if let Some(id) = &params.selected_id {
        selection.select(id);
    }
    selection.reconcile(&ordered);
    match selection.selected().and_then(|s| CString::new(s).ok()) {
        Some(c) => c.into_raw(),
        None => std::ptr::null_mut(),
    }
}

// --- transport (Stage 1 slice 1.3) ---------------------------------------
//
// Stateless like the selection rules: the host holds the machine's state in
// its own `@State` and hands it over per event. The core owns the decisions.

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransportEvent {
    kind: String,
    #[serde(default)]
    time: Option<f64>,
    #[serde(default)]
    duration: Option<f64>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransportParams {
    state: String,
    time: f64,
    duration: f64,
    #[serde(default)]
    resume_after_scrub: bool,
    #[serde(default)]
    seek_generation: u64,
    event: TransportEvent,
}

/// Applies one event to the transport machine. Input JSON:
/// `{"state": "idle"|"playing"|"scrubbing", "time": s, "duration": s,
///   "resumeAfterScrub": bool, "seekGeneration": n,
///   "event": {"kind": "...", "time": s?, "duration": s?}}`
///
/// Event kinds: `play`, `pause`, `toggle`, `tick` (needs `time`),
/// `beginScrub`, `scrubTo` (needs `time`), `endScrub`, `seek` (needs `time`),
/// `setDuration` (needs `duration`).
///
/// Returns the next state plus the effects the host should carry out, in
/// order:
/// `{"state", "time", "duration", "resumeAfterScrub", "seekGeneration",
///   "effects": [{"kind": "seek", "time", "generation"} |
///               {"kind": "startPlayback", "at"} | {"kind": "stopPlayback"}]}`
/// Free with `promo_string_free`.
///
/// Safety contract (C ABI): `params_json` is a valid NUL-terminated string.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_transport_step(params_json: *const c_char) -> *mut c_char {
    if params_json.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(text) = (unsafe { CStr::from_ptr(params_json) }).to_str() else {
        return std::ptr::null_mut();
    };
    let Ok(params) = serde_json::from_str::<TransportParams>(text) else {
        return std::ptr::null_mut();
    };

    let mut transport = promo_editor::Transport::restore(
        promo_editor::TransportState::parse(&params.state),
        params.time,
        params.duration,
        params.resume_after_scrub,
        params.seek_generation,
    );

    let effects = match params.event.kind.as_str() {
        "play" => transport.play(),
        "pause" => transport.pause(),
        "toggle" => transport.toggle(),
        "tick" => transport.tick(params.event.time.unwrap_or(transport.time())),
        "beginScrub" => transport.begin_scrub(),
        "scrubTo" => {
            transport.scrub_to(params.event.time.unwrap_or(transport.time()));
            vec![]
        }
        "endScrub" => transport.end_scrub(),
        "seek" => transport.seek(params.event.time.unwrap_or(transport.time())),
        "setDuration" => {
            transport.set_duration(params.event.duration.unwrap_or(transport.duration()));
            vec![]
        }
        _ => return std::ptr::null_mut(),
    };

    let effects_json: Vec<serde_json::Value> = effects
        .iter()
        .map(|e| match e {
            promo_editor::Effect::Seek { time, generation } => {
                serde_json::json!({"kind": "seek", "time": time, "generation": generation})
            }
            promo_editor::Effect::StartPlayback { at } => {
                serde_json::json!({"kind": "startPlayback", "at": at})
            }
            promo_editor::Effect::StopPlayback => serde_json::json!({"kind": "stopPlayback"}),
        })
        .collect();

    let out = serde_json::json!({
        "state": transport.state().as_str(),
        "time": transport.time(),
        "duration": transport.duration(),
        "resumeAfterScrub": transport.resumes_after_scrub(),
        "seekGeneration": transport.seek_generation(),
        "effects": effects_json,
    });
    match serde_json::to_string(&out)
        .ok()
        .and_then(|s| CString::new(s).ok())
    {
        Some(c) => c.into_raw(),
        None => std::ptr::null_mut(),
    }
}

/// Is this seek completion the current one? 1 = yes.
///
/// Exists so hosts stop branching on the player's "finished" flag, which is
/// false both for a superseded seek and for one whose item was not ready —
/// the confusion that strands playback. The generation decides.
#[no_mangle]
pub extern "C" fn promo_transport_seek_is_current(generation: u64, current: u64) -> i32 {
    (generation == current) as i32
}
