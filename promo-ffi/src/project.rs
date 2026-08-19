//! Project handle + timeline query surface (Phase 1).
//!
//! Two traffic classes (RUST-CORE-PLAN §3):
//! - Cold/bulk: JSON in, JSON out (`promo_project_parse`,
//!   `promo_project_to_json`, `promo_timeline_eval`). Used by the Swift↔Rust
//!   parity harness and one-shot conversions.
//! - Hot: plain C calls against a parsed handle
//!   (`promo_layer_transform`, `promo_resource_source_time`,
//!   `promo_resource_video_segment`, `promo_layer_gain`) — the per-frame
//!   queries the Swift app delegates behind its feature flag.

use promo_model::ProjectMetadata;
use promo_timeline as tl;
use serde_json::json;
use std::ffi::{c_char, c_double, c_float, c_int, CStr, CString};

/// Validates a `metadata.json` payload. Returns NULL when it is valid, or a
/// human-readable message describing the first problem — free it with
/// `promo_string_free`.
///
/// `promo_project_parse` only says yes or no, which is useless in an editor:
/// "invalid" without "line 42: unknown field `canvasWith`" is not a message a
/// person can act on.
///
/// Safety contract (C ABI): `json` is a valid NUL-terminated string.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_project_validate(json: *const c_char) -> *mut c_char {
    crate::ffi_guard_else(
        || to_c_string("internal error: the validator panicked"),
        move || {
            if json.is_null() {
                return to_c_string("no JSON supplied");
            }
            let Ok(text) = (unsafe { CStr::from_ptr(json) }).to_str() else {
                return to_c_string("JSON is not valid UTF-8");
            };
            match ProjectMetadata::from_json(text) {
                Ok(_) => std::ptr::null_mut(),
                Err(e) => to_c_string(&e.to_string()),
            }
        },
    )
}

/// What a project's media inventory looks like against a real folder:
/// `{"resources": [{"id", "filename", "displayName", "kind", "origin"}],
///   "missingLayers": ["id", …]}` — free with `promo_string_free`.
///
/// `filenames` is the `Resources/` listing as a JSON array of names; the
/// caller does the directory read (it owns the sandbox/security scope), the
/// core owns the RULE, so the app, the CLI and `promo_validate` cannot drift
/// on what "the project has" means.
///
/// Safety contract (C ABI): both arguments are valid NUL-terminated strings.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_project_inventory(
    json: *const c_char,
    filenames_json: *const c_char,
) -> *mut c_char {
    if json.is_null() || filenames_json.is_null() {
        return std::ptr::null_mut();
    }
    let (Ok(text), Ok(names_text)) = (
        unsafe { CStr::from_ptr(json) }.to_str(),
        unsafe { CStr::from_ptr(filenames_json) }.to_str(),
    ) else {
        return std::ptr::null_mut();
    };
    let Ok(meta) = ProjectMetadata::from_json(text) else {
        return std::ptr::null_mut();
    };
    let names: Vec<String> = serde_json::from_str(names_text).unwrap_or_default();
    let resolved = promo_model::effective_resources(&meta, &names);
    let resources: Vec<serde_json::Value> = resolved
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.resource.id,
                "filename": r.resource.filename,
                "displayName": r.resource.display_name,
                "kind": r.resource.kind.as_str(),
                "origin": match r.origin {
                    promo_model::ResourceOrigin::Declared => "declared",
                    promo_model::ResourceOrigin::Derived => "derived",
                    promo_model::ResourceOrigin::Missing => "missing",
                },
            })
        })
        .collect();
    let out = serde_json::json!({
        "resources": resources,
        "missingLayers": promo_model::layers_with_missing_media(&meta, &names),
    });
    match serde_json::to_string(&out) {
        Ok(text) => to_c_string(&text),
        Err(_) => std::ptr::null_mut(),
    }
}

/// The route a layer takes between two of its keyframes, as points on the
/// canvas — what the editor draws so a person can SEE the path before
/// committing to it.
///
/// Input `{"layerID": "…", "samples": 64}`; returns
/// `{"segments": [{"fromTime", "toTime", "hasPath", "points": [[x, y], …]}]}`
/// — one entry per keyframe pair the layer actually MOVES across, whether or
/// not it follows a path. A pathed segment is the fitted curve; a plain one
/// is the straight line it really travels, given as its two endpoints.
/// Segments where the layer does not move are omitted: nothing to draw, and
/// an arrowhead there would have no direction. Free with
/// `promo_string_free`.
///
/// Sampled rather than handed over raw: the fitted curve is what the layer
/// actually flies along, so the overlay cannot disagree with the render the
/// way a separately-drawn approximation would.
///
/// Safety contract (C ABI): both arguments are valid NUL-terminated strings.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_layer_motion_paths(
    handle: *const ProjectHandle,
    params_json: *const c_char,
) -> *mut c_char {
    let Some(handle) = (unsafe { handle.as_ref() }) else {
        return std::ptr::null_mut();
    };
    if params_json.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(text) = (unsafe { CStr::from_ptr(params_json) }).to_str() else {
        return std::ptr::null_mut();
    };
    let Ok(params) = serde_json::from_str::<serde_json::Value>(text) else {
        return std::ptr::null_mut();
    };
    let layer_id = params["layerID"].as_str().unwrap_or_default();
    let samples = params["samples"].as_u64().unwrap_or(64).clamp(2, 512) as usize;

    let empty: Vec<promo_model::ProjectLayer> = Vec::new();
    let Some(layer) = handle
        .meta
        .layers
        .as_deref()
        .unwrap_or(&empty)
        .iter()
        .find(|l| l.id == layer_id)
    else {
        return to_c_string("{\"segments\":[]}");
    };
    let resources = handle.meta.resources.as_deref().unwrap_or(&[]);

    let mut keyframes: Vec<&promo_model::ProjectLayerKeyframe> = layer
        .keyframes
        .iter()
        .filter(|k| k.zoom.is_some() || k.vertical_shift.is_some() || k.horizontal_shift.is_some())
        .collect();
    keyframes.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap_or(std::cmp::Ordering::Equal));

    let mut segments: Vec<serde_json::Value> = Vec::new();
    for pair in keyframes.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let from = promo_model::Point(
            a.horizontal_shift.unwrap_or(0.0),
            a.vertical_shift.unwrap_or(0.0),
        );
        let to = promo_model::Point(
            b.horizontal_shift.unwrap_or(0.0),
            b.vertical_shift.unwrap_or(0.0),
        );
        // A segment with a path follows it; one without is still a route —
        // a straight line the layer really travels — and the editor draws
        // both, so a move is visible before anyone decides to bend it.
        // `hasPath` lets the overlay style the two differently without
        // deciding for itself what the shape should be.
        let resolved = b
            .motion_path
            .as_ref()
            .and_then(|path| promo_timeline::path_polyline(resources, path).map(|line| (path, line)));
        let points: Vec<serde_json::Value> = match resolved.as_ref() {
            Some((path, polyline)) => {
                let flipped = path.flipped.unwrap_or(false);
                let (start_at, end_at) =
                    (path.start_at.unwrap_or(0.0), path.end_at.unwrap_or(1.0));
                (0..samples)
                    .map(|step| {
                        let progress = step as f64 / (samples - 1) as f64;
                        let point = promo_timeline::point_along_range(
                            &polyline, from, to, flipped, start_at, end_at, progress,
                        );
                        serde_json::json!([point.x(), point.y()])
                    })
                    .collect()
            }
            // Two points are the whole of a straight line; sampling it
            // further would only invite the overlay to smooth something
            // that is not curved.
            None => {
                // A layer that does not move between these keyframes has no
                // route to draw, and an arrowhead would have no direction.
                let still = (to.x() - from.x()).abs() < 1e-9 && (to.y() - from.y()).abs() < 1e-9;
                if still {
                    continue;
                }
                vec![
                    serde_json::json!([from.x(), from.y()]),
                    serde_json::json!([to.x(), to.y()]),
                ]
            }
        };
        segments.push(serde_json::json!({
            "fromTime": a.time,
            "toTime": b.time,
            "hasPath": resolved.is_some(),
            "points": points,
        }));
    }
    match serde_json::to_string(&serde_json::json!({ "segments": segments })) {
        Ok(text) => to_c_string(&text),
        Err(_) => std::ptr::null_mut(),
    }
}

/// How a path WOULD look applied between two points — the hint an editor
/// draws while someone is choosing one, before anything is committed.
///
/// Params: `{"pathResourceID": "…", "from": [x, y], "to": [x, y],
///           "flipped": false, "startAt": 0, "endAt": 1, "samples": 64}`.
/// Returns `{"points": [[x, y], …]}`; free with `promo_string_free`, NULL
/// when the path cannot be resolved or is not a usable route.
///
/// A pure function of (path, from, to): nothing is written to the project, so
/// hovering a list of paths costs no edits and no undo entries. It runs the
/// SAME fit the renderer runs, which is the point — a hint drawn any other
/// way would promise something the render then does not deliver.
///
/// Safety contract (C ABI): both arguments are valid NUL-terminated strings.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_path_preview(
    handle: *const ProjectHandle,
    params_json: *const c_char,
) -> *mut c_char {
    let Some(handle) = (unsafe { handle.as_ref() }) else {
        return std::ptr::null_mut();
    };
    if params_json.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(text) = (unsafe { CStr::from_ptr(params_json) }).to_str() else {
        return std::ptr::null_mut();
    };
    let Ok(params) = serde_json::from_str::<serde_json::Value>(text) else {
        return std::ptr::null_mut();
    };
    let point_of = |value: &serde_json::Value| -> Option<promo_model::Point> {
        let array = value.as_array()?;
        Some(promo_model::Point(
            array.first()?.as_f64()?,
            array.get(1)?.as_f64()?,
        ))
    };
    let (Some(from), Some(to)) = (point_of(&params["from"]), point_of(&params["to"])) else {
        return std::ptr::null_mut();
    };
    let path = promo_model::MotionPath {
        path_resource_id: params["pathResourceID"].as_str().unwrap_or_default().to_string(),
        flipped: params["flipped"].as_bool(),
        start_at: params["startAt"].as_f64(),
        end_at: params["endAt"].as_f64(),
    };
    let Some(polyline) = promo_timeline::path_polyline(
        handle.meta.resources.as_deref().unwrap_or(&[]),
        &path,
    ) else {
        return std::ptr::null_mut();
    };
    let samples = params["samples"].as_u64().unwrap_or(64).clamp(2, 512) as usize;
    let flipped = path.flipped.unwrap_or(false);
    let (start_at, end_at) = (path.start_at.unwrap_or(0.0), path.end_at.unwrap_or(1.0));
    let points: Vec<serde_json::Value> = (0..samples)
        .map(|step| {
            let progress = step as f64 / (samples - 1) as f64;
            let point = promo_timeline::point_along_range(
                &polyline, from, to, flipped, start_at, end_at, progress,
            );
            serde_json::json!([point.x(), point.y()])
        })
        .collect();
    match serde_json::to_string(&serde_json::json!({ "points": points })) {
        Ok(text) => to_c_string(&text),
        Err(_) => std::ptr::null_mut(),
    }
}

fn to_c_string(message: &str) -> *mut c_char {
    match CString::new(message) {
        Ok(c) => c.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Opaque to C. One fully parsed project.
pub struct ProjectHandle {
    meta: ProjectMetadata,
}

/// Parses a `metadata.json` payload (NUL-terminated UTF-8). Returns an owned
/// handle, or null when the payload does not parse. Free with
/// `promo_project_free`.
///
/// Safety contract (C ABI): `json` must be a valid NUL-terminated string.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_project_parse(json: *const c_char) -> *mut ProjectHandle {
    crate::ffi_guard(std::ptr::null_mut(), move || {
        if json.is_null() {
            return std::ptr::null_mut();
        }
        let raw = unsafe { CStr::from_ptr(json) };
        let Ok(text) = raw.to_str() else {
            return std::ptr::null_mut();
        };
        match ProjectMetadata::from_json(text) {
            Ok(meta) => Box::into_raw(Box::new(ProjectHandle { meta })),
            Err(_) => std::ptr::null_mut(),
        }
    })
}

/// Frees a handle returned by `promo_project_parse`. Null is a no-op.
///
/// Safety contract (C ABI): `handle` must be a pointer this library returned,
/// freed at most once.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_project_free(handle: *mut ProjectHandle) {
    crate::ffi_guard((), move || {
        if !handle.is_null() {
            drop(unsafe { Box::from_raw(handle) });
        }
    })
}

/// Re-encodes the handle's project as JSON. Caller frees the returned string
/// with `promo_string_free`. Null on encode failure or null handle.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_project_to_json(handle: *const ProjectHandle) -> *mut c_char {
    crate::ffi_guard(std::ptr::null_mut(), move || {
        let Some(handle) = (unsafe { handle.as_ref() }) else {
            return std::ptr::null_mut();
        };
        match handle.meta.to_json() {
            Ok(s) => CString::new(s)
                .map(CString::into_raw)
                .unwrap_or(std::ptr::null_mut()),
            Err(_) => std::ptr::null_mut(),
        }
    })
}

/// Frees a string returned by this library. Null is a no-op.
///
/// Safety contract (C ABI): `s` must be a string this library returned,
/// freed at most once.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_string_free(s: *mut c_char) {
    crate::ffi_guard((), move || {
        if !s.is_null() {
            drop(unsafe { CString::from_raw(s) });
        }
    })
}

/// Evaluates every timeline query at each of `times` (seconds, global
/// timeline) and returns one JSON document — the parity harness diffs this
/// against the Swift implementation's answers. Caller frees the string with
/// `promo_string_free`.
///
/// Safety contract (C ABI): `times` must point to `times_len` readable
/// doubles (or be null with `times_len` 0).
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_timeline_eval(
    handle: *const ProjectHandle,
    times: *const c_double,
    times_len: usize,
) -> *mut c_char {
    crate::ffi_guard(std::ptr::null_mut(), move || {
        let Some(handle) = (unsafe { handle.as_ref() }) else {
            return std::ptr::null_mut();
        };
        let times: &[f64] = if times.is_null() || times_len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(times, times_len) }
        };
        let doc = eval(&handle.meta, times);
        match serde_json::to_string(&doc) {
            Ok(s) => CString::new(s)
                .map(CString::into_raw)
                .unwrap_or(std::ptr::null_mut()),
            Err(_) => std::ptr::null_mut(),
        }
    })
}

fn eval(meta: &ProjectMetadata, times: &[f64]) -> serde_json::Value {
    let settings = &meta.composition_settings;
    let empty_layers = Vec::new();
    let empty_resources = Vec::new();
    let layers = meta.layers.as_ref().unwrap_or(&empty_layers);
    let resources = meta.resources.as_ref().unwrap_or(&empty_resources);

    let layer_docs: Vec<_> = layers
        .iter()
        .map(|layer| {
            let default_gain = layer
                .resource_id
                .as_ref()
                .and_then(|rid| resources.iter().find(|r| &r.id == rid))
                .map(|r| r.effective_volume())
                .unwrap_or(1.0);
            let transforms: Vec<_> = times
                .iter()
                .map(|&t| {
                    let tr = tl::layer_transform_along_paths(
                        layer, t, settings, meta.resources.as_deref().unwrap_or(&[]));
                    json!([tr.zoom, tr.vertical_shift, tr.horizontal_shift])
                })
                .collect();
            let rotations: Vec<_> = times
                .iter()
                .map(|&t| tl::layer_rotation(layer, t))
                .collect();
            let tilts: Vec<_> = times
                .iter()
                .map(|&t| match tl::layer_tilt_offset(layer, t) {
                    Some((x, y)) => json!([x, y]),
                    None => serde_json::Value::Null,
                })
                .collect();
            let bg: Vec<_> = times
                .iter()
                .map(|&t| tl::layer_background_color_hex(layer, t, settings))
                .collect();
            let gains: Vec<_> = times
                .iter()
                .map(|&t| tl::layer_gain(layer, tl::layer_local_time(layer, t), default_gain))
                .collect();
            let visible: Vec<_> = times
                .iter()
                .map(|&t| tl::layer_is_visible(layer, t))
                .collect();
            let ramps: Vec<_> = tl::gain_ramp_segments(layer, default_gain)
                .iter()
                .map(|s| json!([s.start_time, s.end_time, s.start_value, s.end_value]))
                .collect();
            json!({
                "id": layer.id,
                "transform": transforms,
                "rotation": rotations,
                "tilt": tilts,
                "bgColor": bg,
                "gain": gains,
                "visible": visible,
                "gainRamps": ramps,
            })
        })
        .collect();

    let resource_docs: Vec<_> = resources
        .iter()
        .map(|res| {
            let ranges: Vec<_> = tl::video_trim_ranges(res)
                .iter()
                .map(|r| json!([r.start, r.end]))
                .collect();
            let pauses: Vec<_> = tl::extended_video_pauses(res)
                .iter()
                .map(|p| json!([p.start_time, p.duration]))
                .collect();
            // Timeline-clock views (speed applied) — what the Swift resource
            // extension answers, and what the parity harness diffs against.
            let source_times: Vec<_> = times
                .iter()
                .map(|&t| tl::timeline_source_time(res, t))
                .collect();
            let segments: Vec<_> = times
                .iter()
                .map(|&t| {
                    let s = tl::timeline_video_segment(res, t);
                    json!([s.source_start, s.local_start, s.local_end, s.rate])
                })
                .collect();
            let output_times: Vec<_> = times
                .iter()
                .map(|&t| match tl::timeline_output_time_for_source(res, t) {
                    Some(v) => json!(v),
                    None => serde_json::Value::Null,
                })
                .collect();
            json!({
                "id": res.id,
                "trimRanges": ranges,
                "pauses": pauses,
                "loopPeriod": tl::timeline_loop_period(res),
                "sourceTime": source_times,
                "segment": segments,
                "outputTime": output_times,
            })
        })
        .collect();

    let settings_values: Vec<_> = times
        .iter()
        .map(|&t| {
            let v = tl::settings_interpolated_values(settings, t);
            json!([v.zoom, v.vertical_shift, v.horizontal_shift])
        })
        .collect();
    let settings_bg: Vec<_> = times
        .iter()
        .map(|&t| tl::settings_background_color_hex(settings, t))
        .collect();

    let focus: Vec<_> = tl::audio_focus_intervals(meta)
        .iter()
        .map(|(s, e)| json!([s, e]))
        .collect();

    json!({
        "layers": layer_docs,
        "resources": resource_docs,
        "settings": { "values": settings_values, "bgColor": settings_bg },
        "focusIntervals": focus,
    })
}

// ---------------------------------------------------------------------------
// Hot-path calls (plain C, no serialization)

/// Layer transform at a global time: fills `out[0..3]` = zoom, verticalShift,
/// horizontalShift. Returns 0, or -1 on bad handle/index/out pointer.
///
/// Safety contract (C ABI): `out` must point to 3 writable doubles.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_layer_transform(
    handle: *const ProjectHandle,
    layer_index: c_int,
    time: c_double,
    out: *mut c_double,
) -> c_int {
    crate::ffi_guard(-1, move || {
        let Some(handle) = (unsafe { handle.as_ref() }) else {
            return -1;
        };
        if out.is_null() || layer_index < 0 {
            return -1;
        }
        let Some(layer) = handle
            .meta
            .layers
            .as_ref()
            .and_then(|l| l.get(layer_index as usize))
        else {
            return -1;
        };
        let tr = tl::layer_transform_along_paths(
            layer, time, &handle.meta.composition_settings,
            handle.meta.resources.as_deref().unwrap_or(&[]));
        unsafe {
            *out = tr.zoom;
            *out.add(1) = tr.vertical_shift;
            *out.add(2) = tr.horizontal_shift;
        }
        0
    })
}

/// Source-media time for a resource-local TIMELINE time (trims + pauses +
/// looping + playback speed). Returns the mapped time, or -1.0 on bad
/// handle/index.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_resource_source_time(
    handle: *const ProjectHandle,
    resource_index: c_int,
    local_time: c_double,
) -> c_double {
    crate::ffi_guard(-1.0, move || {
        let Some(handle) = (unsafe { handle.as_ref() }) else {
            return -1.0;
        };
        if resource_index < 0 {
            return -1.0;
        }
        let Some(res) = handle
            .meta
            .resources
            .as_ref()
            .and_then(|r| r.get(resource_index as usize))
        else {
            return -1.0;
        };
        tl::timeline_source_time(res, local_time)
    })
}

/// Continuous playback segment containing a resource-local time: fills
/// `out[0..4]` = sourceStart, localStart, localEnd, rate. Returns 0, or -1
/// on bad handle/index/out pointer.
///
/// Safety contract (C ABI): `out` must point to 4 writable doubles.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_resource_video_segment(
    handle: *const ProjectHandle,
    resource_index: c_int,
    local_time: c_double,
    out: *mut c_double,
) -> c_int {
    crate::ffi_guard(-1, move || {
        let Some(handle) = (unsafe { handle.as_ref() }) else {
            return -1;
        };
        if out.is_null() || resource_index < 0 {
            return -1;
        }
        let Some(res) = handle
            .meta
            .resources
            .as_ref()
            .and_then(|r| r.get(resource_index as usize))
        else {
            return -1;
        };
        let seg = tl::timeline_video_segment(res, local_time);
        unsafe {
            *out = seg.source_start;
            *out.add(1) = seg.local_start;
            *out.add(2) = seg.local_end;
            *out.add(3) = seg.rate as f64;
        }
        0
    })
}

/// Resolved layer volume at a layer-local time. `default_gain` is the
/// baseline (the resource's Volume). Returns the gain, or `default_gain`
/// on bad handle/index.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_layer_gain(
    handle: *const ProjectHandle,
    layer_index: c_int,
    local_time: c_double,
    default_gain: c_float,
) -> c_float {
    crate::ffi_guard(default_gain, move || {
        let Some(handle) = (unsafe { handle.as_ref() }) else {
            return default_gain;
        };
        if layer_index < 0 {
            return default_gain;
        }
        let Some(layer) = handle
            .meta
            .layers
            .as_ref()
            .and_then(|l| l.get(layer_index as usize))
        else {
            return default_gain;
        };
        tl::layer_gain(layer, local_time, default_gain)
    })
}

/// Layer visibility at a composition time (enabled, started, not yet ended).
/// 1 visible, 0 hidden, 0 on bad handle/index.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_layer_is_visible(
    handle: *const ProjectHandle,
    layer_index: c_int,
    time: c_double,
) -> c_int {
    crate::ffi_guard(0, move || {
        let Some(layer) = (unsafe { layer_at(handle, layer_index) }) else {
            return 0;
        };
        tl::layer_is_visible(layer, time) as c_int
    })
}

/// Layout for a media layer: `out[4]` = x, y, width, height in canvas space.
/// Pure geometry — no project handle needed. 0 ok, -1 bad out pointer.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn promo_media_rect(
    source_width: c_double,
    source_height: c_double,
    canvas_width: c_double,
    canvas_height: c_double,
    zoom: c_double,
    horizontal_shift: c_double,
    vertical_shift: c_double,
    out: *mut c_double,
) -> c_int {
    crate::ffi_guard(-1, move || {
        if out.is_null() {
            return -1;
        }
        let rect = tl::media_rect(
            promo_model::Size::new(source_width, source_height),
            promo_model::Size::new(canvas_width, canvas_height),
            zoom,
            horizontal_shift,
            vertical_shift,
        );
        unsafe {
            *out = rect.x();
            *out.add(1) = rect.y();
            *out.add(2) = rect.width();
            *out.add(3) = rect.height();
        }
        0
    })
}

/// Layout for a drawing layer (content bounds aspect-fit into the canvas,
/// centered, then zoom + shift): `out[4]` = x, y, width, height.
/// Pure geometry — no project handle needed. 0 ok, -1 bad out pointer.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn promo_drawing_rect(
    natural_width: c_double,
    natural_height: c_double,
    canvas_width: c_double,
    canvas_height: c_double,
    zoom: c_double,
    horizontal_shift: c_double,
    vertical_shift: c_double,
    out: *mut c_double,
) -> c_int {
    crate::ffi_guard(-1, move || {
        if out.is_null() {
            return -1;
        }
        let rect = tl::drawing_rect(
            promo_model::Size::new(natural_width, natural_height),
            promo_model::Size::new(canvas_width, canvas_height),
            zoom,
            horizontal_shift,
            vertical_shift,
        );
        unsafe {
            *out = rect.x();
            *out.add(1) = rect.y();
            *out.add(2) = rect.width();
            *out.add(3) = rect.height();
        }
        0
    })
}

/// Clockwise rotation in degrees at a composition time (keyframed, holding
/// the first/last value outside the range). 0 on bad handle/index.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_layer_rotation(
    handle: *const ProjectHandle,
    layer_index: c_int,
    time: c_double,
) -> c_double {
    crate::ffi_guard(0.0, move || {
        let Some(layer) = (unsafe { layer_at(handle, layer_index) }) else {
            return 0.0;
        };
        tl::layer_rotation(layer, time)
    })
}

/// Layer opacity at `time` — 0…1, and 1 when the layer has no opacity
/// keyframes. Mirrors `promo_layer_rotation`.
///
/// Safety contract (C ABI): `handle` is a live project handle.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_layer_opacity(
    handle: *const ProjectHandle,
    layer_index: c_int,
    time: c_double,
) -> c_double {
    crate::ffi_guard(1.0, move || {
        let Some(layer) = (unsafe { layer_at(handle, layer_index) }) else {
            return 1.0;
        };
        tl::layer_opacity(layer, time)
    })
}

/// Device-frame 2.5D tilt at a composition time: `out[2]` = tiltX, tiltY in
/// degrees. Returns 0 when the layer has tilt keyframes, -1 otherwise (the
/// caller then uses the frame's static tilt) or on bad handle/index.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_layer_tilt(
    handle: *const ProjectHandle,
    layer_index: c_int,
    time: c_double,
    out: *mut c_double,
) -> c_int {
    crate::ffi_guard(-1, move || {
        if out.is_null() {
            return -1;
        }
        let Some(layer) = (unsafe { layer_at(handle, layer_index) }) else {
            return -1;
        };
        let Some((x, y)) = tl::layer_tilt_offset(layer, time) else {
            return -1;
        };
        unsafe {
            *out = x;
            *out.add(1) = y;
        }
        0
    })
}

/// The background layer's resolved color at a composition time as
/// straight (non-premultiplied) RGBA in 0…1: `out[4]`. Falls back to the
/// composition settings' background when the layer has no color keyframes.
/// 0 ok, -1 bad handle/index/out.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_layer_background_rgba(
    handle: *const ProjectHandle,
    layer_index: c_int,
    time: c_double,
    out: *mut c_double,
) -> c_int {
    crate::ffi_guard(-1, move || {
        if out.is_null() {
            return -1;
        }
        let Some(handle_ref) = (unsafe { handle.as_ref() }) else {
            return -1;
        };
        let Some(layer) = (unsafe { layer_at(handle, layer_index) }) else {
            return -1;
        };
        let settings = &handle_ref.meta.composition_settings;
        let hex = tl::layer_background_color_hex(layer, time, settings);
        // Resolved here too: this is what the canvas overlay asks, and an
        // overlay that disagrees with the render is the whole problem the
        // palette's single resolver exists to avoid.
        let rgba = rgba_from_hex(settings.resolve_color(&hex));
        unsafe {
            for (i, c) in rgba.iter().enumerate() {
                *out.add(i) = *c as f64;
            }
        }
        0
    })
}

/// Shared layer lookup for the hot-path queries above.
///
/// # Safety
/// `handle` must be a live project handle or null.
unsafe fn layer_at<'a>(
    handle: *const ProjectHandle,
    layer_index: c_int,
) -> Option<&'a promo_model::ProjectLayer> {
    if layer_index < 0 {
        return None;
    }
    handle
        .as_ref()?
        .meta
        .layers
        .as_ref()?
        .get(layer_index as usize)
}

/// `#RRGGBB` → straight RGBA 0…1 (opaque). Black on anything unparseable,
/// matching the host's CGColor fallback.
fn rgba_from_hex(hex: &str) -> [f32; 4] {
    let value = hex.trim().trim_start_matches('#').to_uppercase();
    if value.len() != 6 {
        return [0.0, 0.0, 0.0, 1.0];
    }
    let Ok(parsed) = u32::from_str_radix(&value, 16) else {
        return [0.0, 0.0, 0.0, 1.0];
    };
    [
        ((parsed >> 16) & 0xFF) as f32 / 255.0,
        ((parsed >> 8) & 0xFF) as f32 / 255.0,
        (parsed & 0xFF) as f32 / 255.0,
        1.0,
    ]
}

/// Convenience for hosts: index of the resource a layer references, or -1.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_layer_resource_index(
    handle: *const ProjectHandle,
    layer_index: c_int,
) -> c_int {
    crate::ffi_guard(-1, move || {
        let Some(handle) = (unsafe { handle.as_ref() }) else {
            return -1;
        };
        if layer_index < 0 {
            return -1;
        }
        let Some(rid) = handle
            .meta
            .layers
            .as_ref()
            .and_then(|l| l.get(layer_index as usize))
            .and_then(|l| l.resource_id.as_ref())
        else {
            return -1;
        };
        handle
            .meta
            .resources
            .as_ref()
            .and_then(|rs| rs.iter().position(|r| &r.id == rid))
            .map(|i| i as c_int)
            .unwrap_or(-1)
    })
}

/// Audio mix-graph parity surface: computes the final per-track amplitude
/// breakpoints (Swift `AudioTimelineBuilder.levelPoints` twin). Input JSON:
/// `{"automation": [[t, v], …], "trackStart": s, "trackEnd": s,
///   "focus": [[a, b], …], "isFocused": bool, "duckFactor": f, "ramp": s}`.
/// Returns `[[t, v], …]` as JSON; free with `promo_string_free`.
///
/// Safety contract (C ABI): `params_json` is a valid NUL-terminated string.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_audio_level_points(params_json: *const c_char) -> *mut c_char {
    crate::ffi_guard(std::ptr::null_mut(), move || {
        if params_json.is_null() {
            return std::ptr::null_mut();
        }
        let Ok(text) = unsafe { CStr::from_ptr(params_json) }.to_str() else {
            return std::ptr::null_mut();
        };
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Params {
            automation: Vec<(f64, f32)>,
            track_start: f64,
            track_end: f64,
            #[serde(default)]
            focus: Vec<(f64, f64)>,
            #[serde(default)]
            is_focused: bool,
            duck_factor: f32,
            ramp: f64,
        }
        let Ok(params) = serde_json::from_str::<Params>(text) else {
            return std::ptr::null_mut();
        };
        let automation: Vec<tl::VolumePoint> = params
            .automation
            .iter()
            .map(|&(time, volume)| tl::VolumePoint { time, volume })
            .collect();
        let points = tl::level_points(
            &automation,
            params.track_start,
            params.track_end,
            &params.focus,
            params.is_focused,
            params.duck_factor,
            params.ramp,
        );
        let out: Vec<(f64, f32)> = points.iter().map(|p| (p.time, p.volume)).collect();
        match serde_json::to_string(&out) {
            Ok(s) => CString::new(s)
                .map(CString::into_raw)
                .unwrap_or(std::ptr::null_mut()),
            Err(_) => std::ptr::null_mut(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_cstring() -> CString {
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../fixtures/projects/project-4.json"
        ))
        .expect("fixture");
        CString::new(raw).unwrap()
    }

    #[test]
    fn parse_eval_free_round_trip() {
        let json = fixture_cstring();
        let handle = promo_project_parse(json.as_ptr());
        assert!(!handle.is_null());

        let times = [0.0, 2.5, 8.5, 14.5];
        let out = promo_timeline_eval(handle, times.as_ptr(), times.len());
        assert!(!out.is_null());
        let text = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_string();
        promo_string_free(out);
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(doc["layers"].as_array().unwrap().len(), 4);
        assert_eq!(doc["resources"].as_array().unwrap().len(), 4);
        assert_eq!(doc["focusIntervals"][0][0].as_f64(), Some(2.5));

        // Hot path spot checks against the timeline crate's fixture tests.
        assert_eq!(promo_resource_source_time(handle, 0, 14.0), 5.0);
        let mut seg = [0.0; 4];
        assert_eq!(
            promo_resource_video_segment(handle, 0, 12.0, seg.as_mut_ptr()),
            0
        );
        assert_eq!(seg, [0.0, 10.0, 13.0, 1.0]);
        let mut tr = [0.0; 3];
        assert_eq!(
            promo_layer_transform(handle, 1, 2.5 + 5.25, tr.as_mut_ptr()),
            0
        );
        assert!((tr[0] - 1.6).abs() < 1e-12);
        assert_eq!(promo_layer_resource_index(handle, 1), 0);
        assert_eq!(promo_layer_resource_index(handle, 0), -1);
        let g = promo_layer_gain(handle, 3, 2.5, 0.8);
        assert!((g - 0.6).abs() < 1e-6);

        promo_project_free(handle);
    }

    #[test]
    fn to_json_round_trips() {
        let json = fixture_cstring();
        let handle = promo_project_parse(json.as_ptr());
        let out = promo_project_to_json(handle);
        assert!(!out.is_null());
        let text = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_string();
        promo_string_free(out);
        let reparsed = promo_project_parse(CString::new(text).unwrap().as_ptr());
        assert!(!reparsed.is_null());
        promo_project_free(reparsed);
        promo_project_free(handle);
    }

    #[test]
    fn bad_inputs_are_safe() {
        assert!(promo_project_parse(std::ptr::null()).is_null());
        let bogus = CString::new("not json").unwrap();
        assert!(promo_project_parse(bogus.as_ptr()).is_null());
        promo_project_free(std::ptr::null_mut());
        promo_string_free(std::ptr::null_mut());
        assert!(promo_project_to_json(std::ptr::null()).is_null());
        assert_eq!(promo_resource_source_time(std::ptr::null(), 0, 1.0), -1.0);
    }
}

/// Resolves attached layers, returning the project with concrete
/// `startTime`/`duration` written in — free it with `promo_string_free`.
///
/// Swift calls this rather than reimplementing the walk. Two implementations
/// of one rule is how caption placement came to mean different things in the
/// app and the CLI; there is no reason to repeat that with timing.
///
/// Any problems are appended under a `timingProblems` key as plain sentences,
/// so the caller can show them without knowing the enum. A project that
/// cannot be resolved still comes back — resolution never refuses.
///
/// Safety contract (C ABI): `json` is a valid NUL-terminated string.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_project_resolve_timing(json: *const c_char) -> *mut c_char {
    crate::ffi_guard(std::ptr::null_mut(), move || {
        if json.is_null() {
            return std::ptr::null_mut();
        }
        let Ok(text) = (unsafe { CStr::from_ptr(json) }).to_str() else {
            return std::ptr::null_mut();
        };
        let Ok(mut meta) = ProjectMetadata::from_json(text) else {
            return std::ptr::null_mut();
        };
        let problems = promo_timeline::resolve_attachments(&mut meta);
        let Ok(serde_json::Value::Object(mut object)) = serde_json::to_value(&meta) else {
            return std::ptr::null_mut();
        };
        if !problems.is_empty() {
            object.insert(
                "timingProblems".to_string(),
                serde_json::Value::Array(
                    problems
                        .iter()
                        .map(|p| serde_json::Value::String(p.to_string()))
                        .collect(),
                ),
            );
        }
        match serde_json::to_string(&serde_json::Value::Object(object)) {
            Ok(out) => to_c_string(&out),
            Err(_) => std::ptr::null_mut(),
        }
    })
}

/// The sprite frame showing `elapsed` seconds into a layer, for a UI that
/// wants to draw the animation itself — the resource preview.
///
/// `sheet_json` is a `SpriteSheet` object; `beyond_end` is `""`, `"hold"`,
/// `"loop"` or `"hide"`. Returns `{"frame": n, "uvRect": [u, v, w, h]}`, or
/// `{"frame": -1}` when a spent `hide` means nothing is drawn. NULL only when
/// the arguments cannot be read at all.
///
/// It exists so the preview and the render cannot disagree about which cell
/// is showing. A preview that computed its own frame would be a second
/// implementation of the rule, and the first thing anyone would notice is
/// that the loop looks right in the inspector and wrong in the export.
///
/// Safety contract (C ABI): both arguments are valid NUL-terminated strings.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_sprite_frame(
    sheet_json: *const c_char,
    elapsed: f64,
    beyond_end: *const c_char,
) -> *mut c_char {
    if sheet_json.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(text) = (unsafe { CStr::from_ptr(sheet_json) }).to_str() else {
        return std::ptr::null_mut();
    };
    let Ok(sheet) = serde_json::from_str::<promo_model::SpriteSheet>(text) else {
        return std::ptr::null_mut();
    };
    let mode = if beyond_end.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(beyond_end) }
            .to_string_lossy()
            .to_string()
    };
    // A layer is the only thing that carries `beyondEnd`, and the rule lives
    // on layers, so the caller's mode is expressed as the layer it would be.
    let clause = if mode.is_empty() {
        String::new()
    } else {
        format!(r#""beyondEnd": "{mode}","#)
    };
    let Ok(layer) = serde_json::from_str::<promo_model::ProjectLayer>(&format!(
        r#"{{"id": "S", "name": "S", "sortIndex": 0, "kind": "image",
             "isEnabled": true, "startTime": 0, {clause} "keyframes": []}}"#
    )) else {
        return std::ptr::null_mut();
    };
    // The uv rect is what it is regardless of pixel size; a unit sheet keeps
    // this call free of a size the caller would have to supply twice.
    let answer = match promo_timeline::sprite_frame_at(
        &sheet,
        &layer,
        elapsed,
        promo_model::Size::new(1.0, 1.0),
    ) {
        Some(frame) => serde_json::json!({
            "frame": frame.index,
            "uvRect": frame.uv_rect,
        }),
        None => serde_json::json!({ "frame": -1 }),
    };
    to_c_string(&answer.to_string())
}

/// Where a caption's box lands, for an editor that draws its own text.
///
/// Takes the project handle, a caption LAYER's id and a time — not a style —
/// so the app never has to know how a `SubtitleStyle` plus the composition
/// defaults become the text style the renderer uses. That mapping is exactly
/// where a preview drifts from a render.
///
/// Returns `{"width","height","x","y","textWidth","lines"}` in canvas px, or
/// NULL for an empty caption: nothing to draw is not a zero-sized box.
///
/// The app's editing canvas draws captions with the host's own text stack and
/// was measuring them at INFINITE width, so a headline long enough to wrap
/// showed as one overflowing line while the export wrapped it to two. One
/// layout now answers both.
///
/// Safety contract (C ABI): `layer_id` is a valid NUL-terminated string.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_caption_measure(
    handle: *const ProjectHandle,
    layer_id: *const c_char,
    time: f64,
) -> *mut c_char {
    let Some(handle) = (unsafe { handle.as_ref() }) else {
        return std::ptr::null_mut();
    };
    if layer_id.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(layer_id) = (unsafe { CStr::from_ptr(layer_id) }).to_str() else {
        return std::ptr::null_mut();
    };
    let empty: Vec<promo_model::ProjectLayer> = Vec::new();
    let Some(layer) = handle
        .meta
        .layers
        .as_deref()
        .unwrap_or(&empty)
        .iter()
        .find(|l| l.id == layer_id)
    else {
        return std::ptr::null_mut();
    };
    let settings = &handle.meta.composition_settings;
    let text = handle.meta.caption_text_for(layer).unwrap_or_default();
    let style = promo_engine::caption_style(
        handle.meta.caption_style_for(layer).as_ref(),
        settings,
    );
    // The keyframed values — size and margins animate on a caption, so
    // measuring at a TIME rather than statically is the difference between a
    // box that tracks the caption and one that lags it. No keyframes means
    // the base style stands.
    let base = tl::CaptionValues {
        font_size: style.font_size,
        vertical_margin: style.vertical_margin,
        left_margin: style.left_margin,
    };
    let values = tl::layer_caption_values(layer, time, base).unwrap_or(base);
    let style = promo_text::TextStyle {
        font_size: values.font_size,
        left_margin: values.left_margin,
        vertical_margin: values.vertical_margin,
        ..style
    };
    let Some(box_) = promo_text::measure(
        &text,
        settings.canvas_width,
        settings.canvas_height,
        &style,
    ) else {
        return std::ptr::null_mut();
    };
    to_c_string(
        &serde_json::json!({
            "width": box_.width, "height": box_.height,
            "x": box_.x, "y": box_.y,
            "textWidth": box_.text_width, "lines": box_.lines,
        })
        .to_string(),
    )
}

/// The layer's resolved viewport at `time` as JSON `[x, y, w, h]` (unit
/// source coordinates, clamped), or NULL when the layer has no viewport
/// keyframes — or is not the kind that honours one. The editor draws its
/// drag box from THIS rather than re-running the interpolation in Swift:
/// the resolver owns the hold-then-ramp rule and the clamping, and two
/// implementations of "where is the window right now" would drift exactly
/// the way caption wrapping once did.
///
/// Safety contract (C ABI): `layer_id` is a valid NUL-terminated string.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_layer_viewport(
    handle: *const ProjectHandle,
    layer_id: *const c_char,
    time: f64,
) -> *mut c_char {
    let Some(handle) = (unsafe { handle.as_ref() }) else {
        return std::ptr::null_mut();
    };
    if layer_id.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(layer_id) = (unsafe { CStr::from_ptr(layer_id) }).to_str() else {
        return std::ptr::null_mut();
    };
    let empty: Vec<promo_model::ProjectLayer> = Vec::new();
    let Some(layer) = handle
        .meta
        .layers
        .as_deref()
        .unwrap_or(&empty)
        .iter()
        .find(|l| l.id == layer_id)
    else {
        return std::ptr::null_mut();
    };
    let Some(vp) = tl::layer_viewport(layer, time) else {
        return std::ptr::null_mut();
    };
    to_c_string(&serde_json::json!(vp).to_string())
}
