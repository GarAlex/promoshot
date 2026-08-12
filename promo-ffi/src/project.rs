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
}

/// Frees a handle returned by `promo_project_parse`. Null is a no-op.
///
/// Safety contract (C ABI): `handle` must be a pointer this library returned,
/// freed at most once.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_project_free(handle: *mut ProjectHandle) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle) });
    }
}

/// Re-encodes the handle's project as JSON. Caller frees the returned string
/// with `promo_string_free`. Null on encode failure or null handle.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_project_to_json(handle: *const ProjectHandle) -> *mut c_char {
    let Some(handle) = (unsafe { handle.as_ref() }) else {
        return std::ptr::null_mut();
    };
    match handle.meta.to_json() {
        Ok(s) => CString::new(s)
            .map(CString::into_raw)
            .unwrap_or(std::ptr::null_mut()),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Frees a string returned by this library. Null is a no-op.
///
/// Safety contract (C ABI): `s` must be a string this library returned,
/// freed at most once.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s) });
    }
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
                    let tr = tl::layer_transform(layer, t, settings);
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
            let source_times: Vec<_> = times
                .iter()
                .map(|&t| tl::source_time_for_local(res, t))
                .collect();
            let segments: Vec<_> = times
                .iter()
                .map(|&t| {
                    let s = tl::video_segment(res, t);
                    json!([s.source_start, s.local_start, s.local_end, s.rate])
                })
                .collect();
            let output_times: Vec<_> = times
                .iter()
                .map(|&t| match tl::output_time_for_source(res, t) {
                    Some(v) => json!(v),
                    None => serde_json::Value::Null,
                })
                .collect();
            json!({
                "id": res.id,
                "trimRanges": ranges,
                "pauses": pauses,
                "loopPeriod": tl::loop_period(res),
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
    let tr = tl::layer_transform(layer, time, &handle.meta.composition_settings);
    unsafe {
        *out = tr.zoom;
        *out.add(1) = tr.vertical_shift;
        *out.add(2) = tr.horizontal_shift;
    }
    0
}

/// Source-media time for a resource-local output time (trims + pauses +
/// looping). Returns the mapped time, or -1.0 on bad handle/index.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_resource_source_time(
    handle: *const ProjectHandle,
    resource_index: c_int,
    local_time: c_double,
) -> c_double {
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
    tl::source_time_for_local(res, local_time)
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
    let seg = tl::video_segment(res, local_time);
    unsafe {
        *out = seg.source_start;
        *out.add(1) = seg.local_start;
        *out.add(2) = seg.local_end;
        *out.add(3) = seg.rate as f64;
    }
    0
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
}

/// Convenience for hosts: index of the resource a layer references, or -1.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn promo_layer_resource_index(
    handle: *const ProjectHandle,
    layer_index: c_int,
) -> c_int {
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
