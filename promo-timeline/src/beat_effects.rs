//! Beat generators (docs/BEAT-SYNC.md §4): `pulse` and `shake` on a layer,
//! `flash` on the composition, expanded during resolution into ordinary
//! keyframes and one ordinary drawing layer — so the preview and both
//! export paths draw them with no new engine input, the apps' lanes show
//! them, and validate and explain see them.
//!
//! Ownership is Auto zoom's: a generated keyframe carries `auto` =
//! `beat:<signature of its values>`, a generated layer or resource
//! `generatedBy` = `flash:<n>:<signature>`. Unedited, they are stripped on
//! save and made again on the next load — which is why a new grid re-times
//! them. One a person edited no longer matches its signature: it stays as
//! theirs, its tag dropped, and the expansion leaves its grid point alone.

use promo_model::musical::{
    self, BeatSection, Flash, OneOrMany, Pulse, Shake, TimeValue, BEAT_TAG,
};
use promo_model::{ProjectLayer, ProjectLayerKeyframe, ProjectMetadata, ProjectResource};
use serde_json::{json, Value};

use crate::beats::Grid;
use crate::musical::TimingProblem;

/// Before resolution: generated content as it was written goes (it is made
/// again below), edited content becomes the person's, and an edited or
/// deleted flash layer retires its generator — the save's fold rule
/// (`musical::strip_generated`), applied up front.
pub(crate) fn settle(meta: &mut ProjectMetadata) {
    if !carries_generated(meta) {
        return;
    }
    let Ok(mut value) = serde_json::to_value(&*meta) else {
        return;
    };
    if musical::strip_generated(&mut value) {
        if let Ok(settled) = serde_json::from_value::<ProjectMetadata>(value) {
            *meta = settled;
        }
    }
}

fn carries_generated(meta: &ProjectMetadata) -> bool {
    promo_model::nesting::all_layers(meta).iter().any(|l| {
        l.generated_by.is_some()
            || l.keyframes
                .iter()
                .any(|k| k.auto.as_deref().is_some_and(|a| a.starts_with(BEAT_TAG)))
    }) || meta
        .resources
        .iter()
        .flatten()
        .any(|r| r.generated_by.is_some())
}

/// What an expansion needs to know about where it is.
pub(crate) struct Context<'a> {
    pub grid: Grid,
    pub fps: f64,
    /// The clock's song's sections, when an analysis gave them.
    pub sections: Option<&'a [BeatSection]>,
    /// Where the composition ends: an open-ended layer's end.
    pub end: f64,
}

fn list<T: Clone>(field: &Option<OneOrMany<T>>) -> Vec<T> {
    match field {
        None => Vec::new(),
        Some(OneOrMany::One(one)) => vec![one.clone()],
        Some(OneOrMany::Many(many)) => many.clone(),
    }
}

/// Beat indices on the grid a trigger names, inside `[from, to)`.
fn points(trigger: Option<&Value>, grid: &Grid, from: f64, to: f64) -> Result<Vec<f64>, String> {
    let step = match trigger {
        None => 1.0,
        Some(Value::String(word)) => match word.as_str() {
            "beat" => 1.0,
            "downbeat" | "bar" => f64::from(grid.meter),
            other => {
                return Err(format!(
                    "\"{other}\" is no grid point — beat, downbeat, bar or {{\"beats\": n}}"
                ))
            }
        },
        Some(Value::Object(object)) => match object.get("beats").and_then(Value::as_f64) {
            Some(n) if n > 0.0 => n,
            _ => return Err("every {\"beats\": n} needs n above 0".into()),
        },
        Some(other) => return Err(format!("{other} is no grid point")),
    };
    if to.partial_cmp(&from) != Some(std::cmp::Ordering::Greater) {
        return Ok(Vec::new());
    }
    let mut m = (grid.index_at(from) / step - 1e-9).ceil();
    let mut out = Vec::new();
    while out.len() < 100_000 {
        let index = m * step;
        let t = grid.time_at(index);
        if t >= to {
            break;
        }
        if t >= from - 1e-9 {
            out.push(index);
        }
        m += 1.0;
    }
    Ok(out)
}

/// The span a `range` names, clipped to `[from, to)`.
fn span(
    range: Option<&Value>,
    grid: &Grid,
    sections: Option<&[BeatSection]>,
    from: f64,
    to: f64,
) -> Result<(f64, f64), String> {
    let Some(range) = range else {
        return Ok((from, to));
    };
    let number = |key: &str| range.get(key).and_then(Value::as_f64);
    let value_time = |key: &str| -> Result<Option<f64>, String> {
        let Some(raw) = range.get(key) else {
            return Ok(None);
        };
        let value: TimeValue =
            serde_json::from_value(raw.clone()).map_err(|e| format!("range.{key}: {e}"))?;
        Ok(Some(match value {
            TimeValue::Seconds(s) => s,
            TimeValue::Musical(m) => grid.time_at(grid.position(&m)),
        }))
    };
    let (a, b) = if let Some(bar) = number("bar") {
        let first = grid.bars(bar - 1.0);
        let end = number("bars").map_or(f64::INFINITY, |n| grid.time_at(first + grid.bars(n)));
        (grid.time_at(first), end)
    } else if let Some(beat) = number("beat") {
        let first = beat - 1.0;
        let end = number("beats").map_or(f64::INFINITY, |n| grid.time_at(first + n));
        (grid.time_at(first), end)
    } else if let Some(section) = range.get("section") {
        let sections = sections.ok_or_else(|| {
            "range names a section, but the clock's song has no analysis to have sections"
                .to_string()
        })?;
        let found = sections
            .iter()
            .find(|s| match section {
                Value::Number(n) => n.as_u64() == Some(u64::from(s.index)),
                Value::String(label) => s.label == *label,
                _ => false,
            })
            .ok_or_else(|| format!("range names section {section}, which the song has not"))?;
        let first = grid.bars(f64::from(found.start_bar) - 1.0);
        (
            grid.time_at(first),
            grid.time_at(first + grid.bars(f64::from(found.bars))),
        )
    } else if range.get("from").is_some() || range.get("to").is_some() {
        (
            value_time("from")?.unwrap_or(f64::NEG_INFINITY),
            value_time("to")?.unwrap_or(f64::INFINITY),
        )
    } else {
        return Err(format!(
            "range {range} — use {{\"bar\", \"bars\"}}, {{\"beat\", \"beats\"}}, \
             {{\"section\"}} or {{\"from\", \"to\"}}"
        ));
    };
    Ok((a.max(from), b.min(to)))
}

/// A length as seconds measured forward from `t`.
fn length_from(value: Option<TimeValue>, default_beats: f64, grid: &Grid, t: f64) -> f64 {
    match value {
        Some(TimeValue::Seconds(s)) => s.max(0.0),
        Some(TimeValue::Musical(m)) => match grid.length(&m) {
            Some(beats) => grid.time_at(grid.index_at(t) + beats) - t,
            None => grid.time_at(grid.index_at(t) + default_beats) - t,
        },
        None => grid.time_at(grid.index_at(t) + default_beats) - t,
    }
}

fn keyframe(id: String, time: f64, ramp: f64) -> ProjectLayerKeyframe {
    serde_json::from_value(json!({ "id": id, "time": time, "transitionDuration": ramp }))
        .expect("a keyframe needs only an id, a time and a ramp")
}

/// Tags a generated keyframe with the signature of its values.
fn tagged(mut key: ProjectLayerKeyframe) -> ProjectLayerKeyframe {
    key.auto = None;
    let value = serde_json::to_value(&key).unwrap_or(Value::Null);
    key.auto = Some(format!("{BEAT_TAG}{}", musical::signature(&value)));
    key
}

/// Which track a property keys, and whether a person's keyframe keys it.
fn keys_track(key: &ProjectLayerKeyframe, property: &str) -> bool {
    match property {
        "viewport" => key.viewport.is_some(),
        "opacity" => key.opacity.is_some(),
        "position" => key.horizontal_shift.is_some() || key.vertical_shift.is_some(),
        _ => key.zoom.is_some() || key.placement.is_some(),
    }
}

/// The last keyframe on a track at or before a local time.
fn previous_on<'a>(
    layer: &'a ProjectLayer,
    property: &str,
    local: f64,
) -> Option<&'a ProjectLayerKeyframe> {
    layer
        .keyframes
        .iter()
        .filter(|k| keys_track(k, property) && k.time <= local + 1e-9)
        .max_by(|a, b| a.time.total_cmp(&b.time))
}

/// The base value of a property at absolute `t`, sampled on the person's
/// own keyframes, written into `key`.
fn write_base(
    person: &ProjectLayer,
    property: &str,
    t: f64,
    settings: &promo_model::CompositionSettings,
    key: &mut ProjectLayerKeyframe,
    punch: f64,
) {
    let local = crate::layer_local_time(person, t);
    match property {
        "viewport" => {
            let [x, y, w, h] = crate::layer_viewport(person, t).unwrap_or([0.0, 0.0, 1.0, 1.0]);
            let s = 1.0 / (1.0 + punch);
            let (nw, nh) = (w * s, h * s);
            key.viewport = Some([x + (w - nw) / 2.0, y + (h - nh) / 2.0, nw, nh]);
        }
        "opacity" => {
            let base =
                crate::interpolation::layer_interpolated_scalar(person, local, |k| k.opacity)
                    .unwrap_or(1.0);
            key.opacity = Some((base + punch).clamp(0.0, 1.0));
        }
        _ => {
            // A layer placed by a rule pulses its box; one placed by raw
            // zoom pulses the zoom.
            let rule = previous_on(person, "placement", local).and_then(|k| k.placement.clone());
            match rule.filter(|p| p.height.is_some() || p.width.is_some()) {
                Some(mut placement) => {
                    placement.height = placement.height.map(|h| h * (1.0 + punch));
                    placement.width = placement.width.map(|w| w * (1.0 + punch));
                    key.placement = Some(placement);
                }
                None => {
                    let zoom = crate::layer_transform(person, t, settings).zoom;
                    key.zoom = Some(zoom * (1.0 + punch));
                }
            }
        }
    }
}

/// Every `pulse` and `shake` of one layer, as keyframes on it.
pub(crate) fn expand_layer(
    layer: &mut ProjectLayer,
    ctx: &Context,
    settings: &promo_model::CompositionSettings,
) -> Vec<TimingProblem> {
    let pulses = list(&layer.pulse);
    let shakes = list(&layer.shake);
    if pulses.is_empty() && shakes.is_empty() {
        return Vec::new();
    }
    let mut problems = Vec::new();
    let person = layer.clone();
    let start = layer.start_time;
    let end = layer.duration.map_or(ctx.end, |d| start + d);
    let attack = 1.0 / ctx.fps.max(1.0);
    let half_frame = attack / 2.0;
    let near_person = |property: &str, local: f64| {
        person
            .keyframes
            .iter()
            .any(|k| keys_track(k, property) && (k.time - local).abs() < half_frame)
    };
    let mut made: Vec<ProjectLayerKeyframe> = Vec::new();
    for (n, pulse) in pulses.iter().enumerate() {
        let Pulse {
            every,
            on,
            property,
            amount,
            decay,
            range,
            ..
        } = pulse;
        let property = property.clone().unwrap_or_else(|| "viewport".into());
        if !["viewport", "opacity", "placement"].contains(&property.as_str()) {
            problems.push(TimingProblem::Generator {
                layer: layer.name.clone(),
                why: format!("pulse property \"{property}\" — viewport, opacity or placement"),
            });
            continue;
        }
        let amount = amount.unwrap_or(0.04);
        let window = span(range.as_ref(), &ctx.grid, ctx.sections, start, end);
        let found =
            window.and_then(|(a, b)| points(every.as_ref().or(on.as_ref()), &ctx.grid, a, b));
        let indices = match found {
            Ok(indices) => indices,
            Err(why) => {
                problems.push(TimingProblem::Generator {
                    layer: layer.name.clone(),
                    why: format!("pulse: {why}"),
                });
                continue;
            }
        };
        let keyed = person.keyframes.iter().any(|k| keys_track(k, &property));
        for (k, index) in indices.iter().enumerate() {
            let t = ctx.grid.time_at(*index);
            let local = t - start;
            if near_person(&property, local) {
                continue;
            }
            let decay = length_from(*decay, 0.5, &ctx.grid, t);
            let id = |role: &str| format!("beat-{}-pulse{n}-{k}-{role}", layer.id);
            if keyed && local - attack > 0.0 {
                let previous =
                    previous_on(&person, &property, local - attack).map_or(0.0, |p| p.time);
                let mut pre = keyframe(
                    id("base"),
                    local - attack,
                    (local - attack - previous).max(0.0),
                );
                write_base(&person, &property, t - attack, settings, &mut pre, 0.0);
                made.push(tagged(pre));
            }
            let mut peak = keyframe(id("peak"), local, attack);
            write_base(&person, &property, t, settings, &mut peak, amount);
            made.push(tagged(peak));
            let mut rest = keyframe(id("rest"), local + decay, decay);
            rest.easing = Some(promo_model::Easing::EaseOut);
            write_base(&person, &property, t + decay, settings, &mut rest, 0.0);
            made.push(tagged(rest));
        }
    }
    for (n, shake) in shakes.iter().enumerate() {
        let Shake {
            every,
            on,
            amplitude,
            decay,
            range,
            ..
        } = shake;
        let amplitude = amplitude.unwrap_or(8.0);
        let window = span(range.as_ref(), &ctx.grid, ctx.sections, start, end);
        let found =
            window.and_then(|(a, b)| points(on.as_ref().or(every.as_ref()), &ctx.grid, a, b));
        let indices = match found {
            Ok(indices) => indices,
            Err(why) => {
                problems.push(TimingProblem::Generator {
                    layer: layer.name.clone(),
                    why: format!("shake: {why}"),
                });
                continue;
            }
        };
        for (k, index) in indices.iter().enumerate() {
            let t = ctx.grid.time_at(*index);
            let local = t - start;
            if near_person("position", local) {
                continue;
            }
            let decay = length_from(*decay, 0.25, &ctx.grid, t);
            let base = crate::layer_transform(&person, t, settings);
            let [dx, dy] = direction(&layer.id, *index);
            for (step, (role, scale)) in [
                ("hit", 1.0),
                ("back", -0.5),
                ("settle", 0.25),
                ("rest", 0.0),
            ]
            .into_iter()
            .enumerate()
            {
                let at = local + decay * step as f64 / 3.0;
                let ramp = if step == 0 { attack } else { decay / 3.0 };
                let mut key = keyframe(format!("beat-{}-shake{n}-{k}-{role}", layer.id), at, ramp);
                key.horizontal_shift = Some(base.horizontal_shift + amplitude * scale * dx);
                key.vertical_shift = Some(base.vertical_shift + amplitude * scale * dy);
                made.push(tagged(key));
            }
        }
    }
    layer.keyframes.extend(made);
    layer.keyframes.sort_by(|a, b| a.time.total_cmp(&b.time));
    problems
}

/// A deterministic direction for a layer's shake at a beat: one of eight,
/// from a splitmix64 of the layer's id and the beat — no RNG crate, the
/// same on every machine.
fn direction(id: &str, index: f64) -> [f64; 2] {
    let mut seed: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.bytes() {
        seed ^= u64::from(byte);
        seed = seed.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut z = seed ^ (index.round() as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    const D: f64 = std::f64::consts::FRAC_1_SQRT_2;
    [
        [1.0, 0.0],
        [D, D],
        [0.0, 1.0],
        [-D, D],
        [-1.0, 0.0],
        [-D, -D],
        [0.0, -1.0],
        [D, -D],
    ][(z % 8) as usize]
}

/// Every `flash` of the composition: one generated full-canvas drawing
/// resource and one drawing layer above everything per generator, its
/// opacity rising to the flash's on each grid point and falling back.
pub(crate) fn expand_flash(meta: &mut ProjectMetadata, ctx: &Context) -> Vec<TimingProblem> {
    let flashes = list(&meta.composition_settings.flash);
    if flashes.is_empty() {
        return Vec::new();
    }
    let mut problems = Vec::new();
    let (width, height) = (
        meta.composition_settings.canvas_width,
        meta.composition_settings.canvas_height,
    );
    let attack = 1.0 / ctx.fps.max(1.0);
    let top = meta
        .layers
        .iter()
        .flatten()
        .map(|l| l.sort_index)
        .max()
        .unwrap_or(0);
    for (n, flash) in flashes.iter().enumerate() {
        let Flash {
            every,
            on,
            color_hex,
            opacity,
            duration,
            blend_mode,
            range,
            ..
        } = flash;
        let trigger = on.as_ref().or(every.as_ref());
        let found = span(range.as_ref(), &ctx.grid, ctx.sections, 0.0, ctx.end)
            .and_then(|(a, b)| points(trigger, &ctx.grid, a, b));
        let indices = match found {
            Ok(indices) => indices,
            Err(why) => {
                problems.push(TimingProblem::Generator {
                    layer: format!("flash {n}"),
                    why,
                });
                continue;
            }
        };
        let colour = color_hex.clone().unwrap_or_else(|| "FFFFFF".into());
        let peak = opacity.unwrap_or(0.6).clamp(0.0, 1.0);
        let id = format!("beat-flash-{n}");
        let mut resource: ProjectResource = serde_json::from_value(json!({
            "id": id, "kind": "drawing", "filename": "", "displayName": format!("Flash {}", n + 1),
            "addedAt": 0,
            "drawing": { "shapes": [{ "id": format!("{id}-rect"), "kind": "rect",
                "points": [[0, 0], [width, height]], "strokeColorHex": colour,
                "strokeWidth": 0, "fillColorHex": colour,
                "arrowStart": false, "arrowEnd": false }] }
        }))
        .expect("a drawing resource");
        // Dark until the first grid point — unless that is the very start.
        let mut keys = Vec::new();
        if indices
            .first()
            .is_none_or(|&i| ctx.grid.time_at(i) > attack)
        {
            let mut first = keyframe(format!("{id}-start"), 0.0, 0.0);
            first.opacity = Some(0.0);
            keys.push(first);
        }
        for (k, index) in indices.iter().enumerate() {
            let t = ctx.grid.time_at(*index);
            let length = length_from(*duration, 0.25, &ctx.grid, t);
            if t - attack > 0.0 {
                let mut before = keyframe(format!("{id}-{k}-off"), t - attack, 0.0);
                before.opacity = Some(0.0);
                keys.push(before);
            }
            let mut on_beat = keyframe(format!("{id}-{k}-on"), t, attack.min(t));
            on_beat.opacity = Some(peak);
            keys.push(on_beat);
            let mut after = keyframe(format!("{id}-{k}-out"), t + length, length);
            after.opacity = Some(0.0);
            after.easing = Some(promo_model::Easing::EaseOut);
            keys.push(after);
        }
        keys.sort_by(|a, b| a.time.total_cmp(&b.time));
        let label = match trigger {
            Some(Value::String(word)) => word.clone(),
            Some(other) => other.to_string(),
            None => "beat".into(),
        };
        let mut layer: ProjectLayer = serde_json::from_value(json!({
            "id": id, "name": format!("Flash · on {label}"), "sortIndex": top + 1 + n as i64,
            "kind": "drawing", "isEnabled": true, "startTime": 0, "duration": ctx.end,
            "resourceID": id, "keyframes": []
        }))
        .expect("a drawing layer");
        layer.keyframes = keys;
        if let Some(mode) = blend_mode {
            match serde_json::from_value(json!(mode)) {
                Ok(mode) => layer.blend_mode = Some(mode),
                Err(_) => problems.push(TimingProblem::Generator {
                    layer: format!("flash {n}"),
                    why: format!("blendMode \"{mode}\" is no blend mode"),
                }),
            }
        }
        // A person's layer or resource already under this id keeps it.
        let taken = |id: &str| {
            meta.layers.iter().flatten().any(|l| l.id == id)
                || meta.resources.iter().flatten().any(|r| r.id == id)
        };
        if taken(&id) {
            continue;
        }
        let sign = |value: Value| musical::signature(&value);
        resource.generated_by = Some(format!(
            "flash:{n}:{}",
            sign(serde_json::to_value(&resource).unwrap_or(Value::Null))
        ));
        layer.generated_by = Some(format!(
            "flash:{n}:{}",
            sign(serde_json::to_value(&layer).unwrap_or(Value::Null))
        ));
        meta.resources.get_or_insert_with(Vec::new).push(resource);
        meta.layers.get_or_insert_with(Vec::new).push(layer);
    }
    problems
}
