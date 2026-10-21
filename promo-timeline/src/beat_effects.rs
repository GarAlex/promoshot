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
                .any(|k| k.auto.as_deref().is_some_and(musical::is_generator_tag))
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
    /// The scope's takes, for generators firing on words (§7.3).
    pub speech: Option<&'a crate::speech_time::Speech<'a>>,
}

/// Who a speech trigger or range is read for: its clock and its name.
#[derive(Clone, Copy)]
pub(crate) struct Owner<'a> {
    pub clock: Option<&'a promo_model::speech::SpeechClock>,
    pub name: &'a str,
}

/// Whether a trigger or range names a take's words or sentences.
pub(crate) fn is_speech(value: Option<&Value>) -> bool {
    value.and_then(Value::as_object).is_some_and(|o| {
        o.contains_key("words") || o.contains_key("sentences") || o.contains_key("sentence")
    })
}

/// The output times a speech trigger names, inside `[from, to)`:
/// `{"words": "all" | [12, 14] | ["Euclid"] | "emphasised", "of": …}` (with
/// `top` or `above` on the words' prominence) or `{"sentences": "start" |
/// "end", "of": …}`.
fn speech_times(
    trigger: &Value,
    ctx: &Context,
    owner: Owner,
    from: f64,
    to: f64,
) -> Result<Vec<f64>, String> {
    let speech = ctx
        .speech
        .ok_or("a speech trigger here has no take to count on")?;
    let of = trigger.get("of").and_then(Value::as_str);
    let (take, _) = speech
        .take_for(owner.clock, owner.name, of)
        .map_err(|f| f.why())?;
    let words = &take.timing.words;
    let mut source: Vec<f64> = Vec::new();
    if let Some(which) = trigger.get("words") {
        match which {
            Value::String(all) if all == "all" => source.extend(words.iter().map(|w| w.start)),
            Value::String(e) if e == "emphasised" || e == "emphasized" => {
                let scored: Vec<(usize, f64)> = words
                    .iter()
                    .enumerate()
                    .filter_map(|(i, w)| w.prominence.map(|p| (i, p)))
                    .collect();
                if scored.is_empty() {
                    return Err(format!(
                        "\"{}\" has no word prominence yet — the open measures it from the audio \
                         (`promo words`); name the words instead",
                        take.id
                    ));
                }
                let mut chosen: Vec<usize> =
                    if let Some(above) = trigger.get("above").and_then(Value::as_f64) {
                        scored
                            .iter()
                            .filter(|(_, p)| *p > above)
                            .map(|(i, _)| *i)
                            .collect()
                    } else {
                        let top = trigger.get("top").and_then(Value::as_u64).unwrap_or(3) as usize;
                        let mut ranked = scored.clone();
                        // Most prominent first; a tie goes to the earlier word.
                        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
                        ranked.into_iter().take(top).map(|(i, _)| i).collect()
                    };
                chosen.sort_unstable();
                source.extend(chosen.into_iter().map(|i| words[i].start));
            }
            Value::Array(list) => {
                for item in list {
                    match item {
                        Value::Number(n) => {
                            let i = n
                                .as_u64()
                                .and_then(|n| (n as usize).checked_sub(1))
                                .filter(|i| *i < words.len())
                                .ok_or_else(|| {
                                    format!("word {n} is past the end of \"{}\"", take.id)
                                })?;
                            source.push(words[i].start);
                        }
                        Value::String(text) => {
                            let wanted = promo_model::speech::fold_word(text);
                            let hits: Vec<f64> = words
                                .iter()
                                .filter(|w| promo_model::speech::fold_word(&w.text) == wanted)
                                .map(|w| w.start)
                                .collect();
                            if hits.is_empty() {
                                return Err(format!("no word \"{text}\" in \"{}\"", take.id));
                            }
                            source.extend(hits);
                        }
                        other => return Err(format!("{other} names no word")),
                    }
                }
            }
            other => {
                return Err(format!(
                    "\"words\": {other} — \"all\", \"emphasised\", or a list of indices or texts"
                ))
            }
        }
    } else if let Some(which) = trigger.get("sentences") {
        let sentences = &take.timing.sentences;
        match which.as_str() {
            Some("start") => source.extend(sentences.iter().map(|s| s.start)),
            Some("end") => source.extend(sentences.iter().map(|s| s.end)),
            _ => return Err("\"sentences\" is \"start\" or \"end\"".into()),
        }
    }
    let mut out: Vec<f64> = source
        .into_iter()
        .filter_map(|s| take.out(s))
        .filter(|t| *t >= from - 1e-9 && *t < to)
        .collect();
    out.sort_by(f64::total_cmp);
    out.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    Ok(out)
}

fn list<T: Clone>(field: &Option<OneOrMany<T>>) -> Vec<T> {
    match field {
        None => Vec::new(),
        Some(OneOrMany::One(one)) => vec![one.clone()],
        Some(OneOrMany::Many(many)) => many.clone(),
    }
}

/// Beat indices on the grid a trigger names, inside `[from, to)` — for a
/// speech trigger, the grid index of each word's (or sentence's) time.
fn points(
    trigger: Option<&Value>,
    ctx: &Context,
    owner: Owner,
    from: f64,
    to: f64,
) -> Result<Vec<f64>, String> {
    let grid = &ctx.grid;
    if is_speech(trigger) {
        let times = speech_times(trigger.expect("a trigger"), ctx, owner, from, to)?;
        return Ok(times.into_iter().map(|t| grid.index_at(t)).collect());
    }
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

/// The span a `range` names, clipped to `[from, to)` — a speech span is
/// `{"sentence": 2, "of": …}` or `{"words": [3, 9], "of": …}`.
fn span(
    range: Option<&Value>,
    ctx: &Context,
    owner: Owner,
    from: f64,
    to: f64,
) -> Result<(f64, f64), String> {
    let (grid, sections) = (&ctx.grid, ctx.sections);
    let Some(range) = range else {
        return Ok((from, to));
    };
    if is_speech(Some(range)) {
        let speech = ctx
            .speech
            .ok_or("a speech range here has no take to count on")?;
        let during: promo_model::speech::During =
            serde_json::from_value(range.clone()).map_err(|e| format!("range {range}: {e}"))?;
        let stand_in: ProjectLayer = serde_json::from_value(json!({
            "id": "", "name": owner.name, "sortIndex": 0, "kind": "image",
            "isEnabled": true, "startTime": 0, "keyframes": []}))
        .expect("a stand-in");
        let mut stand_in = stand_in;
        stand_in.speech_clock = owner.clock.cloned();
        let (a, b, _, _) = speech.during(&stand_in, &during).map_err(|f| f.why())?;
        return Ok((a.max(from), b.min(to)));
    }
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
        // From the song's own seconds, onto the nearest bar starts of the
        // grid that counts — the analysis's own downbeats, or a hand grid
        // whose bars sit a little elsewhere: sections are where the music
        // changes, whoever counts its tempo.
        let bar_at = |source: f64| {
            let t = grid.clock + source / grid.speed;
            grid.time_at(grid.snap(promo_model::musical::SnapUnit::Bar, t))
        };
        (bar_at(found.start), bar_at(found.end))
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

/// Tags a generated keyframe with the signature of its values, under the
/// generator's own tag (`beat:`, `speech:`).
fn tagged_as(mut key: ProjectLayerKeyframe, tag: &str) -> ProjectLayerKeyframe {
    key.auto = None;
    let value = serde_json::to_value(&key).unwrap_or(Value::Null);
    key.auto = Some(format!("{tag}{}", musical::signature(&value)));
    key
}

/// Which track a property keys, and whether a person's keyframe keys it.
fn keys_track(key: &ProjectLayerKeyframe, property: &str) -> bool {
    match property {
        "viewport" => key.viewport.is_some(),
        "opacity" => key.opacity.is_some(),
        "position" => key.horizontal_shift.is_some() || key.vertical_shift.is_some(),
        "captionPosition" => key.placement.is_some(),
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
    resources: &[ProjectResource],
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
        _ if person.kind == promo_model::ProjectLayerKind::Caption => {
            // A caption's size is its fontSize in points — its zoom field
            // is the legacy spelling of the same number, so a zoom of
            // 1.04 would draw 1-point text.
            let style = caption_text_style(person, t, settings, resources);
            key.font_size = Some(style.font_size * (1.0 + punch));
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
                    let zoom =
                        crate::layer_transform_along_paths(person, t, settings, resources).zoom;
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
    resources: &[ProjectResource],
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
        let owner = Owner {
            clock: person.speech_clock.as_ref(),
            name: &person.name,
        };
        let trigger = every.as_ref().or(on.as_ref());
        let (prefix, tag) = if is_speech(trigger) {
            ("speech", musical::SPEECH_TAG)
        } else {
            ("beat", BEAT_TAG)
        };
        let window = span(range.as_ref(), ctx, owner, start, end);
        let found = window.and_then(|(a, b)| points(trigger, ctx, owner, a, b));
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
        if keyed {
            problems.push(TimingProblem::PulseOverKeys {
                layer: layer.name.clone(),
                property: property.clone(),
            });
        }
        for (k, index) in indices.iter().enumerate() {
            let t = ctx.grid.time_at(*index);
            let local = t - start;
            if near_person(&property, local) {
                continue;
            }
            let decay = length_from(*decay, 0.5, &ctx.grid, t);
            let id = |role: &str| format!("{prefix}-{}-pulse{n}-{k}-{role}", layer.id);
            // Held at its base until the first peak (a track holds its
            // first key's value before it), and before every peak over a
            // person's keys.
            if (keyed || k == 0) && local - attack > 1e-9 {
                let previous =
                    previous_on(&person, &property, local - attack).map_or(0.0, |p| p.time);
                let mut pre = keyframe(
                    id("base"),
                    local - attack,
                    (local - attack - previous).max(0.0),
                );
                write_base(
                    &person,
                    &property,
                    t - attack,
                    settings,
                    resources,
                    &mut pre,
                    0.0,
                );
                made.push(tagged_as(pre, tag));
            }
            let mut peak = keyframe(id("peak"), local, attack);
            write_base(
                &person, &property, t, settings, resources, &mut peak, amount,
            );
            made.push(tagged_as(peak, tag));
            let mut rest = keyframe(id("rest"), local + decay, decay);
            rest.easing = Some(promo_model::Easing::EaseOut);
            write_base(
                &person,
                &property,
                t + decay,
                settings,
                resources,
                &mut rest,
                0.0,
            );
            made.push(tagged_as(rest, tag));
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
        let owner = Owner {
            clock: person.speech_clock.as_ref(),
            name: &person.name,
        };
        let trigger = on.as_ref().or(every.as_ref());
        let (prefix, tag) = if is_speech(trigger) {
            ("speech", musical::SPEECH_TAG)
        } else {
            ("beat", BEAT_TAG)
        };
        let window = span(range.as_ref(), ctx, owner, start, end);
        let found = window.and_then(|(a, b)| points(trigger, ctx, owner, a, b));
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
        let caption = person.kind == promo_model::ProjectLayerKind::Caption;
        let track = if caption {
            "captionPosition"
        } else {
            "position"
        };
        let keyed = person.keyframes.iter().any(|key| keys_track(key, track));
        for (k, index) in indices.iter().enumerate() {
            let t = ctx.grid.time_at(*index);
            let local = t - start;
            if near_person(track, local) {
                continue;
            }
            let decay = length_from(*decay, 0.25, &ctx.grid, t);
            let [dx, dy] = direction(&layer.id, *index);
            // Where the layer sits, in canvas px, at each step's instant:
            // a caption by the placement rule that hangs its box (its
            // keyed one, its style's, or the one its margins amount to);
            // anything else by the top-left its own track resolves to,
            // the resource's real aspect included — a placement rule
            // resolved against a square stand-in put the picture ~200 px
            // off on every hit.
            let at_rest = |at: f64| -> Position {
                if caption {
                    Position::Rule(caption_rule(&person, at, settings, resources))
                } else {
                    let base = crate::layer_transform_along_paths(&person, at, settings, resources);
                    Position::TopLeft(base.horizontal_shift, base.vertical_shift)
                }
            };
            let mut steps: Vec<(&str, f64, f64, f64)> = Vec::new();
            // Held where it was until the first hit — without this its
            // offset stood from the layer's start to its beat (a track
            // holds its first key's value before it) — and before every
            // hit over a person's own moves.
            if (keyed || k == 0) && local - attack > 1e-9 {
                steps.push(("base", local - attack, 0.0, 0.0));
            }
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
                let ramp = if step == 0 {
                    attack.min(local)
                } else {
                    decay / 3.0
                };
                steps.push((role, at, ramp, scale));
            }
            for (role, at, ramp, scale) in steps {
                let mut key = keyframe(
                    format!("{prefix}-{}-shake{n}-{k}-{role}", layer.id),
                    at,
                    ramp,
                );
                let (ox, oy) = (amplitude * scale * dx, amplitude * scale * dy);
                match at_rest(start + at) {
                    Position::Rule(mut rule) => {
                        let [x, y] = rule.offset.unwrap_or([0.0, 0.0]);
                        rule.offset = Some([x + ox, y + oy]);
                        key.placement = Some(rule);
                    }
                    Position::TopLeft(x, y) => {
                        key.horizontal_shift = Some(x + ox);
                        key.vertical_shift = Some(y + oy);
                    }
                }
                made.push(tagged_as(key, tag));
            }
        }
    }
    layer.keyframes.extend(made);
    layer.keyframes.sort_by(|a, b| a.time.total_cmp(&b.time));
    problems
}

/// Where a layer stands, as its shake moves it.
enum Position {
    /// A caption: the rule hanging its box (anchor and offset only).
    Rule(promo_model::Placement),
    /// Anything else: its drawn top-left, in canvas px.
    TopLeft(f64, f64),
}

/// A caption's text style at absolute `t`: its showing resource's (or its
/// own) captionStyle over the composition's, with its keyed size and
/// margins — what the renderer lays it out with.
fn caption_text_style(
    person: &ProjectLayer,
    t: f64,
    settings: &promo_model::CompositionSettings,
    resources: &[ProjectResource],
) -> promo_text::TextStyle {
    let showing = crate::layer_resource_id(person, t, resources);
    let style = resources
        .iter()
        .find(|r| Some(r.id.as_str()) == showing)
        .and_then(|r| r.caption_style.clone())
        .or_else(|| person.caption_style.clone());
    let mut text = crate::caption::caption_style(style.as_ref(), settings);
    if let Some(values) = crate::layer_caption_values(
        person,
        t,
        crate::CaptionValues {
            font_size: text.font_size,
            vertical_margin: text.vertical_margin,
            left_margin: text.left_margin,
        },
    ) {
        text.font_size = values.font_size;
        text.vertical_margin = values.vertical_margin;
        text.left_margin = values.left_margin;
    }
    text
}

/// The placement rule that hangs a caption's box where it stands at
/// absolute `t`: its keyed rule, else its style's, else the one its
/// margins amount to (the box's top at the vertical margin; across, by its
/// alignment between the side margins) — so an offset added to it moves
/// the words by exactly that many canvas px, whichever placed them.
fn caption_rule(
    person: &ProjectLayer,
    t: f64,
    settings: &promo_model::CompositionSettings,
    resources: &[ProjectResource],
) -> promo_model::Placement {
    let local = crate::layer_local_time(person, t);
    let keyed = previous_on(person, "captionPosition", local)
        .or_else(|| {
            person
                .keyframes
                .iter()
                .filter(|k| k.placement.is_some())
                .min_by(|a, b| a.time.total_cmp(&b.time))
        })
        .and_then(|k| k.placement.clone());
    let text = caption_text_style(person, t, settings, resources);
    let rule = keyed.or(text.placement.clone()).unwrap_or_else(|| {
        let (anchor, x) = match text.align {
            promo_text::Align::Leading => (promo_model::Anchor::TopLeft, text.left_margin),
            promo_text::Align::Center => (
                promo_model::Anchor::Top,
                (text.left_margin - text.right_margin) / 2.0,
            ),
            promo_text::Align::Trailing => (promo_model::Anchor::TopRight, -text.right_margin),
        };
        promo_model::Placement {
            height: None,
            width: None,
            mode: None,
            anchor: Some(anchor),
            offset: Some([x, text.vertical_margin]),
        }
    });
    // Only where it hangs: a caption's size is its text.
    promo_model::Placement {
        height: None,
        width: None,
        mode: None,
        ..rule
    }
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
        let flash_name = format!("flash {n}");
        let owner = Owner {
            clock: meta.composition_settings.speech_clock.as_ref(),
            name: &flash_name,
        };
        let found = span(range.as_ref(), ctx, owner, 0.0, ctx.end)
            .and_then(|(a, b)| points(trigger, ctx, owner, a, b));
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
        // The drawing's id is its own: ids are one namespace in the file,
        // and a layer and a resource sharing one is a break.
        let drawing_id = format!("{id}-drawing");
        let mut resource: ProjectResource = serde_json::from_value(json!({
            "id": drawing_id, "kind": "drawing", "filename": "", "displayName": format!("Flash {}", n + 1),
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
            Some(other) if is_speech(Some(other)) => match other.get("sentences") {
                Some(_) => "sentences".into(),
                None => "words".into(),
            },
            Some(other) => other.to_string(),
            None => "beat".into(),
        };
        let mut layer: ProjectLayer = serde_json::from_value(json!({
            "id": id, "name": format!("Flash · on {label}"), "sortIndex": top + 1 + n as i64,
            "kind": "drawing", "isEnabled": true, "startTime": 0, "duration": ctx.end,
            "resourceID": drawing_id, "keyframes": []
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
        if taken(&id) || taken(&drawing_id) {
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
