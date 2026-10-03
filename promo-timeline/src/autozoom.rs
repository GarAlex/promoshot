//! Auto zoom: a screen recording's camera, planned from what happened in it.
//!
//! The camera eases in to where the action is, holds while it plays, and
//! eases back out to the whole frame — or, when the next action comes soon,
//! pans straight there instead of pulling wide and diving back in. Quick
//! eased moves with holds, never one long drift: that is what reads as an
//! editor's hand rather than a machine's.
//!
//! The result is ORDINARY viewport keyframes. Nothing renders "auto zoom";
//! the plan is data an editor shows, retimes and deletes like any keyframe
//! a person wrote. Each planned keyframe carries an `auto` signature — a
//! hash of its values as written — and that is the whole ownership model:
//!
//! - a keyframe without the tag is the person's, never moved or deleted;
//! - a tagged keyframe whose values still hash to its tag is Auto zoom's,
//!   replaced by the next plan;
//! - a tagged keyframe a person edited no longer matches, so it is theirs
//!   now, kept, and planned around exactly like an untagged one.
//!
//! Signals are SOURCE seconds (the recording's own clock: pointer track,
//! activity track, skip spans); keyframes are LAYER-LOCAL seconds. Every
//! signal crosses through the same trim / cut / speed / loop mapping the
//! renderer reads frames with, so a trimmed or sped-up recording zooms on
//! the moment it actually shows.
//!
//! Pure and deterministic: the same project and options always give the
//! same keyframes, ids included — a re-plan of an unchanged project is a
//! no-op diff.
//!
//! Unlike most of this crate it has no Swift twin, on purpose: the apps
//! reach it through the editor's `autoZoom` command, and a second planner
//! would drift from this one the first time either was tuned.

use promo_model::{
    ActivityTrack, AutoZoomAmount, AutoZoomPace, BeyondEnd, Easing, ImageOrientation, ProjectLayer,
    ProjectLayerKeyframe, ProjectLayerKind, ProjectMetadata, ProjectResource, ProjectResourceKind,
    Size,
};
use std::borrow::Cow;

/// What a plan is asked for. [`PlanOptions::for_layer`] reads the layer's
/// stored `autoZoom`; a caller that changes a setting overrides a field.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanOptions<'a> {
    pub amount: AutoZoomAmount,
    pub pace: AutoZoomPace,
    /// Source-time spans `[t0, t1]` to leave wide.
    pub skip: Vec<[f64; 2]>,
    /// Measured activity to plan from instead of the resource's own — a
    /// host that has just measured plans before it stores.
    pub activity: Option<&'a ActivityTrack>,
}

impl Default for PlanOptions<'_> {
    fn default() -> Self {
        PlanOptions {
            amount: AutoZoomAmount::Normal,
            pace: AutoZoomPace::Normal,
            skip: Vec::new(),
            activity: None,
        }
    }
}

impl PlanOptions<'_> {
    /// The settings the layer's own `autoZoom` states, defaults elsewhere.
    pub fn for_layer(layer: &ProjectLayer) -> Self {
        let stored = layer.auto_zoom.clone().unwrap_or_default();
        PlanOptions {
            amount: stored.amount.unwrap_or(AutoZoomAmount::Normal),
            pace: stored.pace.unwrap_or(AutoZoomPace::Normal),
            skip: stored.skip,
            activity: None,
        }
    }
}

/// One closeup, in layer-local seconds: `start` is when the camera has
/// arrived, `end` when it begins to leave (the layer's end for a closeup
/// held to the last frame). The ramps lie outside the span.
#[derive(Debug, Clone, PartialEq)]
pub struct Shot {
    pub start: f64,
    pub end: f64,
    /// Linear magnification: the window is `1 / zoom` of the source per
    /// unit axis.
    pub zoom: f64,
    /// Unit source window `[x, y, w, h]`.
    pub window: [f64; 4],
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// Viewport-only keyframes, each tagged `auto`, in time order.
    pub keyframes: Vec<ProjectLayerKeyframe>,
    pub shots: Vec<Shot>,
    /// The most this layer's source can be magnified before a source pixel
    /// covers more than one output pixel. Every shot's zoom is at or below it.
    pub free_zoom: f64,
}

/// How quickly the camera moves and how long it stays, in seconds.
#[derive(Debug, Clone, Copy)]
struct Pace {
    ramp_in: f64,
    ramp_out: f64,
    min_hold: f64,
    /// Actions closer than this share one closeup (when they fit in it).
    merge_gap: f64,
    /// A closeup this close to the next one pans there instead of
    /// returning to the full frame first.
    pan_gap: f64,
}

fn pace_of(pace: AutoZoomPace) -> Pace {
    let p = |ramp_in, ramp_out, min_hold, merge_gap, pan_gap| Pace {
        ramp_in,
        ramp_out,
        min_hold,
        merge_gap,
        pan_gap,
    };
    match pace.known() {
        AutoZoomPace::Calm => p(1.3, 1.2, 2.2, 2.5, 3.0),
        AutoZoomPace::Brisk => p(0.8, 0.8, 1.1, 1.2, 1.8),
        _ => p(1.0, 1.0, 1.6, 1.8, 2.4),
    }
}

fn target_zoom(amount: AutoZoomAmount) -> f64 {
    match amount.known() {
        AutoZoomAmount::Subtle => 1.4,
        AutoZoomAmount::Strong => 2.5,
        _ => 1.8,
    }
}

/// Below this a zoom is a wobble, not a closeup: the shot is dropped.
const MIN_SHOT_ZOOM: f64 = 1.2;
/// Below this much free magnification there is nothing worth planning.
const MIN_FREE_ZOOM: f64 = 1.25;
/// Room around the interest box inside the window, so the action is never
/// pressed against the frame's edge.
const FIT_MARGIN: f64 = 1.15;
/// The camera arrives this long before the action, so the viewer sees it
/// happen rather than the end of a move.
const LEAD: f64 = 0.1;
/// And stays this long after it: the result of a click is part of it.
const LINGER: f64 = 0.4;
/// The shortest ramp the planner writes when something forces a ramp
/// shorter than the pace's: quicker than that is a jump cut.
const MIN_RAMP: f64 = 0.4;
/// Air between a planned move and a span that must stay wide (a person's
/// own viewport move, a skip span).
const WIDE_MARGIN: f64 = 0.15;
/// Activity bursts lighter than this are noise — a cursor blink, a shadow.
const MIN_ACTIVITY_WEIGHT: f64 = 0.15;
/// A click's interest: from a beat before it until its result has shown.
const CLICK_BEFORE: f64 = 0.3;
const CLICK_AFTER: f64 = 0.9;
/// The least an activity event is looked at for: a meter reports a
/// popover appearing or one keystroke as a single frame, and what it
/// changed stays worth seeing for about as long as a click's result.
const ACTIVITY_MIN: f64 = CLICK_AFTER;
/// A click's interest box, unit source coordinates per axis.
const CLICK_BOX: f64 = 0.1;
/// A dwell: the pointer resting this long within `REST_RADIUS`, after
/// travelling at least `DWELL_TRAVEL` from where it last rested.
const DWELL_MIN: f64 = 0.7;
const REST_RADIUS: f64 = 0.015;
const DWELL_TRAVEL: f64 = 0.05;
/// A rest longer than this is reading, not pointing: only its start counts.
const DWELL_CAP: f64 = 1.5;
const DWELL_BOX: f64 = 0.16;
const EPS: f64 = 1e-6;

/// The planned keyframes for `layer` (one of `project`'s, or one about to
/// be), or a plain sentence saying why there is nothing to plan.
pub fn plan(
    project: &ProjectMetadata,
    layer: &ProjectLayer,
    opts: &PlanOptions,
) -> Result<Plan, String> {
    if !matches!(
        layer.kind,
        ProjectLayerKind::Image | ProjectLayerKind::Video
    ) {
        return Err(format!(
            "Auto zoom works on a video or image layer — \"{}\" is a {:?} layer.",
            layer.name, layer.kind
        ));
    }
    if crate::transport::has_transport(layer) {
        // A seek or a pause makes the layer's clock a function of its own
        // keyframes; planning against it would mean re-planning on every
        // transport edit, and a stale plan zooms on the wrong moment.
        return Err(format!(
            "Auto zoom can't plan \"{}\": its playback is keyframed (a seek or a pause). \
             Remove those keyframes, or zoom by hand.",
            layer.name
        ));
    }
    if layer.keyframes.iter().any(|k| k.resource_id.is_some()) {
        return Err(format!(
            "Auto zoom can't plan \"{}\": it swaps to other material partway, and a \
             recording's pointer and activity describe only its own picture.",
            layer.name
        ));
    }
    // Planned around: everything a re-plan keeps. From here on `layer`
    // is the layer as the plan will find it.
    let mut kept = layer.clone();
    remove_plan(&mut kept);
    let layer = &kept;
    let resources = project.resources.as_deref().unwrap_or(&[]);
    let resource = layer
        .resource_id
        .as_deref()
        .and_then(|id| resources.iter().find(|r| r.id == id))
        .ok_or_else(|| {
            format!(
                "Auto zoom needs a recording: \"{}\" shows none.",
                layer.name
            )
        })?;

    let activity = opts.activity.or(resource.activity.as_ref());
    let pointer = resource
        .pointer
        .as_ref()
        .filter(|p| !p.samples.is_empty() || !p.clicks.is_empty());
    if pointer.is_none() && activity.is_none() {
        return Err(
            "Nothing to zoom to yet: this recording has no pointer track and hasn't been measured."
                .to_string(),
        );
    }

    let free_zoom = free_zoom(project, layer, resource)?;
    if free_zoom < MIN_FREE_ZOOM {
        return Err(format!(
            "The recording is too small to zoom into without blurring ({free_zoom:.1}× available)."
        ));
    }
    let target = target_zoom(opts.amount).min(free_zoom);
    let pace = pace_of(opts.pace);
    let clock = Clock::new(project, layer, resource);

    // Interest, in source time, then on the layer's clock.
    let mut interest = Vec::new();
    if let Some(track) = pointer {
        interest.extend(click_interest(track));
        interest.extend(dwell_interest(track, resource.duration));
    }
    if let Some(track) = activity {
        interest.extend(activity_interest(track));
    }
    let interest = deduplicated(interest);

    let mut cuts: Vec<f64> = activity
        .map(|a| a.cuts.iter().flat_map(|&c| clock.instants(c)).collect())
        .unwrap_or_default();
    cuts.extend(clock.seams());
    let scrolls: Vec<(f64, f64)> = activity
        .map(|a| {
            a.scrolls
                .iter()
                .flat_map(|s| clock.spans(s[0], s[1]))
                .collect()
        })
        .unwrap_or_default();
    let mut wide = held_spans(layer);
    for span in &opts.skip {
        wide.extend(clock.spans(span[0].min(span[1]), span[0].max(span[1])));
    }
    let barriers = Barriers {
        cuts: sorted(cuts),
        scrolls: sorted_spans(scrolls),
        wide: sorted_spans(wide),
    };

    let mut items: Vec<Item> = Vec::new();
    for i in &interest {
        for (l0, l1) in clock.spans(i.t0, i.t1) {
            items.push(Item {
                l0,
                l1,
                rect: i.rect,
                weight: i.weight,
            });
        }
    }
    let items = barriers.admitted(items);
    let shots = clusters(&items, &barriers, target, free_zoom, pace.merge_gap);
    let placed = timed(&shots, &barriers, &pace, clock.end);
    let (keyframes, shots) = written(project, layer, &placed, &pace, clock.end);
    Ok(Plan {
        keyframes,
        shots,
        free_zoom,
    })
}

/// Whether `keyframe` is exactly as Auto zoom wrote it: tagged, its values
/// still hashing to the tag, and carrying nothing but the viewport move — a
/// person who added an opacity to a planned keyframe made it theirs too.
pub fn is_planned(keyframe: &ProjectLayerKeyframe) -> bool {
    let Some(tag) = keyframe.auto.as_deref() else {
        return false;
    };
    if signature(keyframe).as_deref() != Some(tag) {
        return false;
    }
    let mut rebuilt = viewport_keyframe(
        keyframe.id.clone(),
        keyframe.time,
        keyframe.transition_duration,
        keyframe.easing,
        keyframe.viewport.unwrap_or(FULL_FRAME),
    );
    rebuilt.auto = keyframe.auto.clone();
    serde_json::to_value(keyframe).ok() == serde_json::to_value(&rebuilt).ok()
}

/// Which of the layer's keyframes (by index) a re-plan or a removal
/// replaces: the planned ones, except those a person's edit has claimed.
///
/// An edited closeup is a move, not a lone keyframe — it rides in from the
/// full frame and back out to it. Replacing the planned keyframes either
/// side of it would leave it holding from the layer's start to its end,
/// which is nobody's edit. So an edited planned keyframe claims its planned
/// neighbours on the viewport track, outward until (and including) the
/// full-frame keyframe on each side: the whole move stays the person's.
pub fn replaceable(layer: &ProjectLayer) -> Vec<bool> {
    let mut track: Vec<usize> = (0..layer.keyframes.len())
        .filter(|&i| layer.keyframes[i].viewport.is_some())
        .collect();
    track.sort_by(|&a, &b| layer.keyframes[a].time.total_cmp(&layer.keyframes[b].time));
    let planned: Vec<bool> = track
        .iter()
        .map(|&i| is_planned(&layer.keyframes[i]))
        .collect();
    let full = |n: usize| is_full(layer.keyframes[track[n]].viewport.unwrap_or(FULL_FRAME));
    let mut claimed = vec![false; track.len()];
    for n in 0..track.len() {
        if planned[n] || layer.keyframes[track[n]].auto.is_none() {
            continue;
        }
        let mut walk = |range: &mut dyn Iterator<Item = usize>| {
            for m in range {
                if !planned[m] {
                    break;
                }
                claimed[m] = true;
                if full(m) {
                    break;
                }
            }
        };
        walk(&mut (0..n).rev());
        walk(&mut (n + 1..track.len()));
    }
    let mut out = vec![false; layer.keyframes.len()];
    for (n, &i) in track.iter().enumerate() {
        out[i] = planned[n] && !claimed[n];
    }
    out
}

/// The layer without the keyframes a re-plan replaces ([`replaceable`]).
pub fn remove_plan(layer: &mut ProjectLayer) {
    let gone = replaceable(layer);
    let mut index = 0;
    layer.keyframes.retain(|_| {
        index += 1;
        !gone[index - 1]
    });
}

const FULL_FRAME: [f64; 4] = [0.0, 0.0, 1.0, 1.0];

/// The `auto` tag for a viewport keyframe as written: 8 lowercase hex of a
/// 64-bit FNV-1a over its time, window, ramp and easing, each quantized so
/// a float that round-trips through JSON hashes the same. Never the id —
/// an id is identity, not a value a person edits. `None` without a window.
pub fn signature(keyframe: &ProjectLayerKeyframe) -> Option<String> {
    let v = keyframe.viewport?;
    let q = |x: f64, scale: f64| (x * scale).round() as i64;
    let text = format!(
        "{}|{},{},{},{}|{}|{}",
        q(keyframe.time, 1e4),
        q(v[0], 1e5),
        q(v[1], 1e5),
        q(v[2], 1e5),
        q(v[3], 1e5),
        q(keyframe.transition_duration, 1e4),
        keyframe.easing.map(|e| e.as_str()).unwrap_or(""),
    );
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Some(format!("{:08x}", ((hash >> 32) ^ hash) as u32))
}

fn viewport_keyframe(
    id: String,
    time: f64,
    ramp: f64,
    easing: Option<Easing>,
    window: [f64; 4],
) -> ProjectLayerKeyframe {
    let mut keyframe: ProjectLayerKeyframe = serde_json::from_value(serde_json::json!({
        "id": id, "time": time, "transitionDuration": ramp
    }))
    .expect("a keyframe needs only an id, a time and a ramp");
    keyframe.easing = easing;
    keyframe.viewport = Some(window);
    keyframe
}

// --- the source clock -----------------------------------------------------

/// Source seconds onto the layer's clock: the inverse of what the renderer
/// does (`resource_for_cut` + `source_time_for_layer`), built from the same
/// pieces so the two cannot disagree about where a moment lands.
struct Clock<'a> {
    view: Cow<'a, ProjectResource>,
    /// A still has no clock of its own: the follow rule runs it on the
    /// layer's, and so does this.
    still: bool,
    speed: f64,
    /// One loop's timeline length when the layer loops.
    period: Option<f64>,
    /// The layer's own end, layer-local.
    end: f64,
}

impl<'a> Clock<'a> {
    fn new(project: &ProjectMetadata, layer: &ProjectLayer, resource: &'a ProjectResource) -> Self {
        let view = crate::mapping::resource_for_cut(resource, layer.media_cut_id.as_deref());
        let still = resource.kind != ProjectResourceKind::Video;
        let speed = crate::mapping::effective_speed(&view);
        let material = if still {
            0.0
        } else {
            crate::mapping::timeline_loop_period(&view)
        };
        let loops = !still
            && match layer.beyond_end {
                Some(policy) => policy == BeyondEnd::Loop,
                None => view.is_looped(),
            };
        let period = (loops && material > 0.01).then_some(material);
        // A layer with no duration runs to the composition's end (the
        // editor's rule); a composition with no end of its own falls back
        // to the material's.
        let end = layer
            .duration
            .filter(|d| d.is_finite() && *d > 0.0)
            .or_else(|| {
                Some(crate::plan::composition_duration(project) - layer.start_time)
                    .filter(|d| *d > 0.0)
            })
            .or_else(|| (material > 0.0).then_some(material))
            .unwrap_or(f64::INFINITY);
        Clock {
            view,
            still,
            speed,
            period,
            end,
        }
    }

    fn local(&self, source: f64) -> Option<f64> {
        if crate::mapping::video_trim_ranges(&self.view).is_empty() {
            // No known duration and no trim: the mapping is the identity
            // (at speed). Everything excluded: there is no answer.
            return crate::mapping::playback_video_trim_ranges(&self.view)
                .is_none()
                .then_some(source.max(0.0) / self.speed);
        }
        crate::mapping::output_time_for_source(&self.view, source).map(|t| t / self.speed)
    }

    /// Every layer-local copy of `t` (one per loop pass), inside the layer.
    fn copies(&self, t: f64) -> Vec<f64> {
        let mut out = Vec::new();
        match self.period {
            Some(period) => {
                let mut k = 0.0;
                while t + k * period < self.end && k < 10_000.0 {
                    out.push(t + k * period);
                    k += 1.0;
                }
            }
            None if t <= self.end => out.push(t),
            None => {}
        }
        out
    }

    /// Where a looping layer jumps back to its start: a new picture as
    /// surely as a measured cut, so the plan treats each seam as one.
    fn seams(&self) -> Vec<f64> {
        let Some(period) = self.period else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut k = 1.0;
        while k * period < self.end - EPS && k < 10_000.0 {
            out.push(k * period);
            k += 1.0;
        }
        out
    }

    fn instants(&self, source: f64) -> Vec<f64> {
        if self.still {
            return vec![source];
        }
        self.local(source)
            .map(|t| self.copies(t))
            .unwrap_or_default()
    }

    /// The layer-local spans a source span shows during: split around the
    /// excluded ranges, one copy per loop pass, cut to the layer.
    fn spans(&self, s0: f64, s1: f64) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        let clip = |(a, b): (f64, f64), out: &mut Vec<(f64, f64)>| {
            let (a, b) = (a.max(0.0), b.min(self.end));
            if b > a {
                out.push((a, b));
            }
        };
        if self.still {
            clip((s0, s1), &mut out);
            return out;
        }
        let ranges = crate::mapping::video_trim_ranges(&self.view);
        let pieces: Vec<(f64, f64)> = if ranges.is_empty() {
            match crate::mapping::playback_video_trim_ranges(&self.view) {
                None => vec![(s0, s1)],
                Some(_) => Vec::new(),
            }
        } else {
            ranges
                .iter()
                .map(|r| (s0.max(r.start), s1.min(r.end)))
                .filter(|(a, b)| b > a)
                .collect()
        };
        for (a, b) in pieces {
            let Some(la) = self.local(a) else { continue };
            // A range's end belongs to the next range (or to nothing), so
            // the end is mapped a hair inside and the hair put back.
            let lb = self
                .local((b - EPS).max(a))
                .map_or(la, |t| t + EPS / self.speed);
            let length = lb - la;
            for start in self.copies(la) {
                clip((start, start + length), &mut out);
            }
        }
        out
    }
}

// --- signals ----------------------------------------------------------------

/// Something worth looking at, in source time.
#[derive(Debug, Clone)]
struct Interest {
    t0: f64,
    t1: f64,
    rect: [f64; 4],
    weight: f64,
    click: bool,
}

/// The same, on the layer's clock.
#[derive(Debug, Clone)]
struct Item {
    l0: f64,
    l1: f64,
    rect: [f64; 4],
    weight: f64,
}

fn centred_box(x: f64, y: f64, side: f64) -> [f64; 4] {
    let x0 = (x - side / 2.0).clamp(0.0, 1.0 - side);
    let y0 = (y - side / 2.0).clamp(0.0, 1.0 - side);
    [x0, y0, side, side]
}

fn finite(values: &[f64]) -> bool {
    values.iter().all(|v| v.is_finite())
}

/// A click is the strongest evidence there is: someone pressed exactly here.
fn click_interest(track: &promo_model::PointerTrack) -> Vec<Interest> {
    track
        .clicks
        .iter()
        .filter(|c| finite(&c[..]))
        .map(|c| Interest {
            t0: c[0] - CLICK_BEFORE,
            t1: c[0] + CLICK_AFTER,
            rect: centred_box(c[1], c[2], CLICK_BOX),
            weight: 1.0,
            click: true,
        })
        .collect()
}

/// A pointer that travels and then rests is pointing at something — a
/// hover, a menu being read, a field about to be typed in. Resting where it
/// already rested is not news, so a dwell needs travel first.
fn dwell_interest(track: &promo_model::PointerTrack, duration: Option<f64>) -> Vec<Interest> {
    let samples: Vec<[f64; 3]> = track
        .samples
        .iter()
        .copied()
        .filter(|s| finite(&s[..]))
        .collect();
    let Some(first) = samples.first() else {
        return Vec::new();
    };
    let last_t = samples.last().map_or(first[0], |s| s[0]);
    let track_end = duration
        .filter(|d| d.is_finite())
        .unwrap_or(last_t)
        .max(last_t);
    let dist = |a: &[f64; 3], b: (f64, f64)| ((a[1] - b.0).powi(2) + (a[2] - b.1).powi(2)).sqrt();
    let mut rested = (first[1], first[2]);
    let mut out = Vec::new();
    let mut i = 0;
    while i < samples.len() {
        let anchor = samples[i];
        let mut j = i + 1;
        while j < samples.len() && dist(&samples[j], (anchor[1], anchor[2])) <= REST_RADIUS {
            j += 1;
        }
        // Samples arrive when the pointer moves, so a rest lasts until the
        // first sample that left it — or the recording's end.
        let rest_end = samples.get(j).map_or(track_end, |s| s[0]);
        if rest_end - anchor[0] >= DWELL_MIN {
            if dist(&anchor, rested) >= DWELL_TRAVEL {
                out.push(Interest {
                    t0: anchor[0],
                    t1: rest_end.min(anchor[0] + DWELL_CAP),
                    rect: centred_box(anchor[1], anchor[2], DWELL_BOX),
                    weight: 0.5,
                    click: false,
                });
            }
            rested = (anchor[1], anchor[2]);
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// Where the picture changed on its own account: typing, a sheet sliding
/// up, a chart redrawing. Light bursts are noise.
fn activity_interest(track: &ActivityTrack) -> Vec<Interest> {
    track
        .events
        .iter()
        .filter(|e| finite(&e[..]) && e[6] >= MIN_ACTIVITY_WEIGHT && e[1] >= e[0])
        .map(|e| {
            let (w, h) = (e[4].clamp(0.0, 1.0), e[5].clamp(0.0, 1.0));
            Interest {
                t0: e[0],
                t1: e[1].max(e[0] + ACTIVITY_MIN),
                rect: [e[2].clamp(0.0, 1.0 - w), e[3].clamp(0.0, 1.0 - h), w, h],
                weight: e[6].min(1.0),
                click: false,
            }
        })
        .collect()
}

/// One event, one piece of evidence: a dwell or a burst of activity at the
/// time and place of a click is the click seen twice, and the click wins.
fn deduplicated(all: Vec<Interest>) -> Vec<Interest> {
    let clicks: Vec<&Interest> = all.iter().filter(|i| i.click).collect();
    let centre = |r: &[f64; 4]| (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    let shadowed = |i: &Interest| {
        clicks.iter().any(|c| {
            let overlap = i.t0 < c.t1 && c.t0 < i.t1;
            let (cx, cy) = centre(&c.rect);
            let (ix, iy) = centre(&i.rect);
            let inside = cx >= i.rect[0]
                && cx <= i.rect[0] + i.rect[2]
                && cy >= i.rect[1]
                && cy <= i.rect[1] + i.rect[3];
            overlap && (inside || ((cx - ix).powi(2) + (cy - iy).powi(2)).sqrt() < 0.2)
        })
    };
    all.iter()
        .filter(|i| i.click || !shadowed(i))
        .cloned()
        .collect()
}

// --- what the plan must respect -------------------------------------------

/// Layer-local times and spans a plan must respect.
struct Barriers {
    /// A new screen: no closeup holds across one.
    cuts: Vec<f64>,
    /// The content moving as a whole: no zoom-in starts during one, and a
    /// held closeup starts pulling wide when one begins.
    scrolls: Vec<(f64, f64)>,
    /// Spans the camera must be at the full frame throughout: where the
    /// person's own viewport keyframes hold or move it, and skip spans.
    wide: Vec<(f64, f64)>,
}

fn sorted(mut v: Vec<f64>) -> Vec<f64> {
    v.sort_by(f64::total_cmp);
    v.dedup_by(|a, b| (*a - *b).abs() < EPS);
    v
}

fn sorted_spans(mut v: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    v.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    v
}

impl Barriers {
    /// Items split at cuts, cut back where a scroll starts, started after a
    /// scroll they began inside, and dropped where the frame must be wide.
    fn admitted(&self, items: Vec<Item>) -> Vec<Item> {
        let mut split = Vec::new();
        for item in items {
            let mut start = item.l0;
            for &c in &self.cuts {
                if c > start + EPS && c < item.l1 - EPS {
                    split.push(Item {
                        l0: start,
                        l1: c,
                        ..item.clone()
                    });
                    start = c;
                }
            }
            split.push(Item { l0: start, ..item });
        }
        let mut out = Vec::new();
        for mut item in split {
            for &(s0, s1) in &self.scrolls {
                if item.l0 >= s0 && item.l0 < s1 {
                    item.l0 = s1;
                }
                if s0 > item.l0 && s0 < item.l1 {
                    item.l1 = s0;
                }
            }
            if item.l1 - item.l0 < 0.2 {
                continue;
            }
            if self
                .wide
                .iter()
                .any(|&(w0, w1)| item.l0 < w1 && w0 < item.l1)
            {
                continue;
            }
            out.push(item);
        }
        out.sort_by(|a, b| a.l0.total_cmp(&b.l0).then(a.l1.total_cmp(&b.l1)));
        out
    }

    fn cut_between(&self, a: f64, b: f64) -> bool {
        self.cuts.iter().any(|&c| c > a + EPS && c < b - EPS)
    }

    /// A scroll or a wide span between two moments: the camera may not
    /// pan across it.
    fn blocks_pan(&self, a: f64, b: f64) -> bool {
        self.scrolls.iter().any(|&(s0, s1)| s0 < b && a < s1)
            || self
                .wide
                .iter()
                .any(|&(w0, w1)| w0 - WIDE_MARGIN < b && a < w1 + WIDE_MARGIN)
    }

    /// The earliest a zoom-in may start ramping for an action starting at
    /// `start`: after any wide span and any scroll that ended before it,
    /// and after the cut that brought its screen in — a ramp begun before
    /// the cut would move over the old screen toward a spot that only
    /// means something on the new one.
    fn floor(&self, start: f64) -> f64 {
        let wide = self
            .wide
            .iter()
            .filter(|&&(_, w1)| w1 <= start + EPS)
            .map(|&(_, w1)| w1 + WIDE_MARGIN);
        let scroll = self
            .scrolls
            .iter()
            .filter(|&&(_, s1)| s1 <= start + EPS)
            .map(|&(_, s1)| s1);
        let cut = self.cuts.iter().copied().filter(|&c| c <= start + EPS);
        wide.chain(scroll).chain(cut).fold(0.0, f64::max)
    }

    /// The first cut after an action: where its hold must end.
    fn cut_cap(&self, start: f64, end: f64) -> f64 {
        self.cuts
            .iter()
            .copied()
            .filter(|&c| c > start + EPS && c >= end - EPS)
            .fold(f64::INFINITY, f64::min)
    }

    /// The latest a closeup on an action ending at `end` may hold before it
    /// starts pulling wide: at a cut, when a scroll starts, or early enough
    /// to be back at the full frame before a wide span.
    fn hold_cap(&self, start: f64, end: f64, ramp_out: f64) -> f64 {
        let scroll = self
            .scrolls
            .iter()
            .filter(|&&(s0, _)| s0 >= end - EPS)
            .map(|&(s0, _)| s0);
        let wide = self
            .wide
            .iter()
            .filter(|&&(w0, _)| w0 >= end - EPS)
            .map(|&(w0, _)| w0 - WIDE_MARGIN - ramp_out);
        scroll.chain(wide).fold(self.cut_cap(start, end), f64::min)
    }
}

fn is_full(v: [f64; 4]) -> bool {
    let c = crate::viewport::clamped_window(v);
    (c[0]).abs() < 1e-6
        && (c[1]).abs() < 1e-6
        && (c[2] - 1.0).abs() < 1e-6
        && (c[3] - 1.0).abs() < 1e-6
}

/// Where the person's own viewport keyframes — everything on `layer`'s
/// viewport track once the replaceable plan is gone — keep the camera
/// busy: every
/// instant their track is away from the full frame, and every ramp of
/// theirs (a planned keyframe landing inside one would cut it short). A
/// ramp written in percent is a share of the gap before it, so that whole
/// gap is theirs; a smooth ramp bends through its neighbours, so the
/// neighbours' gaps are too. Before the first keyframe the track rests at
/// that keyframe's value — a closeup with no anchor holds from zero.
fn held_spans(layer: &ProjectLayer) -> Vec<(f64, f64)> {
    let mut manual: Vec<&ProjectLayerKeyframe> = layer
        .keyframes
        .iter()
        .filter(|k| k.viewport.is_some())
        .collect();
    manual.sort_by(|a, b| a.time.total_cmp(&b.time));
    let full = |k: &ProjectLayerKeyframe| is_full(k.viewport.unwrap_or(FULL_FRAME));
    let mut out = Vec::new();
    let (Some(first), Some(last)) = (manual.first(), manual.last()) else {
        return out;
    };
    if !full(first) {
        out.push((f64::NEG_INFINITY, first.time));
    }
    out.push((first.time, first.time));
    for i in 1..manual.len() {
        let (a, b) = (manual[i - 1], manual[i]);
        if full(a) && full(b) {
            // A ramp from the full frame to the full frame shows nothing,
            // however a planned keyframe before it reshapes it (a spline
            // between equal values stays flat). Only the instant is theirs:
            // a closeup held across it would be pulled out there.
            out.push((b.time, b.time));
            continue;
        }
        let ramp_start = b.time - crate::interpolation::ramp_seconds(b, b.time - a.time);
        if !full(a) {
            out.push((a.time, ramp_start));
        }
        out.push((ramp_start, b.time));
        if b.transition_percent.is_some() {
            out.push((a.time, b.time));
        }
        if b.easing.is_some_and(|e| e.known().is_smooth()) {
            let before = i
                .checked_sub(2)
                .map_or(f64::NEG_INFINITY, |j| manual[j].time);
            let after = manual.get(i + 1).map_or(f64::INFINITY, |k| k.time);
            out.push((before, after));
        }
    }
    if !full(last) {
        out.push((last.time, f64::INFINITY));
    }
    out
}

// --- shots -------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Cluster {
    start: f64,
    end: f64,
    zoom: f64,
    window: [f64; 4],
}

fn union(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    let x0 = a[0].min(b[0]);
    let y0 = a[1].min(b[1]);
    let x1 = (a[0] + a[2]).max(b[0] + b[2]);
    let y1 = (a[1] + a[3]).max(b[1] + b[3]);
    [x0, y0, x1 - x0, y1 - y0]
}

/// The most a box can be magnified with room around it.
fn fit_zoom(rect: [f64; 4]) -> f64 {
    1.0 / (rect[2].max(rect[3]).max(1e-3) * FIT_MARGIN)
}

/// Consecutive actions share a closeup while they come close together and
/// all still fit the window the first one asked for — the zoom of a shot is
/// set by what opened it, so a later action can join but never pull the
/// camera back mid-shot.
fn clusters(
    items: &[Item],
    barriers: &Barriers,
    target: f64,
    free_zoom: f64,
    merge_gap: f64,
) -> Vec<Cluster> {
    struct Open {
        start: f64,
        end: f64,
        rect: [f64; 4],
        zoom: f64,
        wx: f64,
        wy: f64,
        weight: f64,
    }
    let open = |item: &Item| Open {
        start: item.l0,
        end: item.l1,
        rect: item.rect,
        zoom: target.min(fit_zoom(item.rect)).min(free_zoom),
        wx: (item.rect[0] + item.rect[2] / 2.0) * item.weight,
        wy: (item.rect[1] + item.rect[3] / 2.0) * item.weight,
        weight: item.weight,
    };
    let close = |o: Open| {
        let side = 1.0 / o.zoom;
        let (cx, cy) = (o.wx / o.weight, o.wy / o.weight);
        // Centred on the weight of the evidence, then slid to keep the
        // whole union in view (it fits: that is the merge rule), then into
        // the frame.
        let place = |c: f64, lo: f64, len: f64| {
            let mut x = c - side / 2.0;
            if len <= side {
                x = x.clamp(lo + len - side, lo);
            }
            x.clamp(0.0, 1.0 - side)
        };
        Cluster {
            start: o.start,
            end: o.end,
            zoom: o.zoom,
            window: [
                place(cx, o.rect[0], o.rect[2]),
                place(cy, o.rect[1], o.rect[3]),
                side,
                side,
            ],
        }
    };
    let mut out = Vec::new();
    let mut current: Option<Open> = None;
    for item in items {
        if let Some(o) = current.as_mut() {
            let joined = union(o.rect, item.rect);
            let fits = fit_zoom(joined) >= o.zoom - 1e-9;
            let near = item.l0 - o.end <= merge_gap;
            let apart = barriers.cut_between(o.start, item.l1)
                || barriers.blocks_pan(o.end, item.l0.max(o.end));
            if fits && near && !apart {
                o.end = o.end.max(item.l1);
                o.rect = joined;
                o.wx += (item.rect[0] + item.rect[2] / 2.0) * item.weight;
                o.wy += (item.rect[1] + item.rect[3] / 2.0) * item.weight;
                o.weight += item.weight;
                continue;
            }
        }
        if let Some(done) = current.replace(open(item)) {
            out.push(close(done));
        }
    }
    if let Some(done) = current {
        out.push(close(done));
    }
    out.retain(|c| c.zoom >= MIN_SHOT_ZOOM);
    out
}

// --- timing --------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Placed {
    /// When the camera has arrived.
    arrival: f64,
    /// Seconds the arrival ramp takes.
    ramp: f64,
    /// Arrived by panning from the previous closeup.
    panned: bool,
    zoom: f64,
    window: [f64; 4],
    action_end: f64,
    /// When the camera starts to leave.
    hold_end: f64,
    /// The latest the hold may stretch to for a pan: the next cut.
    cut_cap: f64,
    /// Rides the closeup to the layer's last frame.
    to_end: bool,
}

/// Arrive at `desired` over the pace's ramp, starting no earlier than
/// `floor` — a shorter ramp when there is less room, a later arrival only
/// when even the shortest ramp does not fit.
fn arrive(desired: f64, floor: f64, ramp_in: f64) -> (f64, f64) {
    let mut ramp = ramp_in;
    let mut arrival = desired;
    if arrival - ramp < floor {
        ramp = arrival - floor;
        let shortest = MIN_RAMP.min(ramp_in);
        if ramp < shortest {
            ramp = shortest;
            arrival = floor + ramp;
        }
    }
    (arrival, ramp)
}

/// Each shot's arrival, hold and exit: in just before the action, hold
/// until it has played (and at least the pace's minimum), then pan to the
/// next shot when it is near or pull back to the full frame.
fn timed(shots: &[Cluster], barriers: &Barriers, pace: &Pace, end: f64) -> Vec<Placed> {
    let mut placed: Vec<Placed> = Vec::new();
    for shot in shots {
        let desired = shot.start - LEAD;
        let floor = barriers.floor(shot.start);
        let (mut arrival, mut ramp) = arrive(desired, floor, pace.ramp_in);
        let mut panned = false;
        let mut previous = placed.last().cloned();
        if let Some(p) = previous.as_mut() {
            let ramp_start = arrival - ramp;
            let gap = ramp_start - p.hold_end;
            let mut pan = gap <= pace.pan_gap && !barriers.blocks_pan(p.action_end, arrival);
            if pan {
                if ramp_start >= p.hold_end {
                    if ramp_start <= p.cut_cap {
                        p.hold_end = ramp_start;
                    } else if arrival - p.cut_cap <= 1.5 * pace.ramp_in {
                        // The old screen is gone at the cut: the pan starts
                        // there rather than hold a window on a new screen.
                        p.hold_end = p.cut_cap;
                        ramp = arrival - p.hold_end;
                    } else {
                        pan = false;
                    }
                } else {
                    // Too close for both the minimum hold and a full ramp:
                    // the hold gives way first (never below the action and
                    // its beat), then the ramp, then the arrival slips.
                    let least = (p.action_end + LINGER)
                        .min(p.hold_end)
                        .min(p.cut_cap)
                        .max(p.action_end);
                    if ramp_start >= least {
                        p.hold_end = ramp_start;
                    } else {
                        p.hold_end = least;
                        ramp = (arrival - least).max(MIN_RAMP.min(pace.ramp_in));
                        arrival = least + ramp;
                    }
                }
            }
            if pan {
                panned = true;
            } else {
                let back = p.hold_end + pace.ramp_out;
                let floor = floor.max(back);
                (arrival, ramp) = arrive(desired, floor, pace.ramp_in);
                previous = placed.last().cloned();
            }
        }
        let action_end = shot.end;
        if arrival > action_end - 0.2 || arrival >= end - EPS {
            // The camera would get there after the action: no shot.
            continue;
        }
        let cap = barriers.hold_cap(shot.start, action_end, pace.ramp_out);
        let mut hold_end = (action_end + LINGER).max(arrival + pace.min_hold).min(cap);
        // A pull-out that cannot finish before the layer ends reads as a
        // glitch, not a move: the closeup rides to the last frame — unless
        // a cut, a scroll or a wide span comes first. Riding through one
        // would hold a window on a screen it was never chosen for, so the
        // hold ends early enough for the pull-out to finish in time.
        let free_to_end = cap >= end - EPS;
        let to_end = free_to_end && hold_end + pace.ramp_out > end - EPS;
        if !free_to_end {
            hold_end = hold_end.min(end - pace.ramp_out);
        }
        if hold_end < action_end - EPS {
            // It cannot show the action and still be wide in time for a
            // span that must be.
            continue;
        }
        if let (Some(p), Some(last)) = (previous, placed.last_mut()) {
            *last = p;
        }
        placed.push(Placed {
            arrival,
            ramp,
            panned,
            zoom: shot.zoom,
            window: shot.window,
            action_end,
            hold_end: if to_end { end } else { hold_end },
            cut_cap: barriers.cut_cap(shot.start, action_end),
            to_end,
        });
        if to_end {
            break;
        }
    }
    placed
}

// --- writing it down -------------------------------------------------------

fn round(x: f64, scale: f64) -> f64 {
    (x * scale).round() / scale + 0.0
}

/// Every id the project already uses except the keyframes this plan
/// replaces (`layer` is the layer as the re-plan keeps it): ids are unique
/// project-wide, and validation breaks on a repeat.
fn taken_ids(project: &ProjectMetadata, layer: &ProjectLayer) -> std::collections::HashSet<String> {
    let mut taken = std::collections::HashSet::new();
    for resource in project.resources.as_deref().unwrap_or(&[]) {
        taken.insert(resource.id.clone());
    }
    for other in promo_model::nesting::all_layers(project) {
        taken.insert(other.id.clone());
        if other.id != layer.id {
            taken.extend(other.keyframes.iter().map(|k| k.id.clone()));
        }
    }
    taken.insert(layer.id.clone());
    taken.extend(layer.keyframes.iter().map(|k| k.id.clone()));
    taken
}

fn written(
    project: &ProjectMetadata,
    layer: &ProjectLayer,
    placed: &[Placed],
    pace: &Pace,
    end: f64,
) -> (Vec<ProjectLayerKeyframe>, Vec<Shot>) {
    let mut taken = taken_ids(project, layer);
    let mut keyframes = Vec::new();
    let mut add = |time: f64, ramp: f64, easing: Option<Easing>, window: [f64; 4]| {
        let mut id = format!("{}-auto-{}", layer.id, keyframes.len());
        while taken.contains(&id) {
            id.push('b');
        }
        taken.insert(id.clone());
        let window = window.map(|v| round(v, 1e5));
        let mut keyframe =
            viewport_keyframe(id, round(time, 1e4), round(ramp, 1e4), easing, window);
        keyframe.auto = signature(&keyframe);
        keyframes.push(keyframe);
    };
    let mut shots = Vec::new();
    for (i, p) in placed.iter().enumerate() {
        let ramp_start = p.arrival - p.ramp;
        if i == 0 {
            // Before its first viewport keyframe a track rests at that
            // keyframe's value: without a full-frame anchor the layer
            // would open already zoomed in.
            let preceded = layer
                .keyframes
                .iter()
                .any(|k| k.viewport.is_some() && k.time <= ramp_start + EPS);
            if !preceded {
                add(ramp_start.max(0.0), 0.0, None, FULL_FRAME);
            }
        }
        let easing = if p.panned {
            Easing::EaseInOut
        } else {
            Easing::EaseOut
        };
        add(p.arrival, p.ramp, Some(easing), p.window);
        let pans_on = placed.get(i + 1).is_some_and(|next| next.panned);
        if !p.to_end && !pans_on {
            add(
                p.hold_end + pace.ramp_out,
                pace.ramp_out,
                Some(Easing::EaseInOut),
                FULL_FRAME,
            );
        }
        shots.push(Shot {
            start: round(p.arrival, 1e4),
            end: round(if p.to_end { end } else { p.hold_end }, 1e4),
            zoom: p.zoom,
            window: p.window.map(|v| round(v, 1e5)),
        });
    }
    (keyframes, shots)
}

// --- quality ----------------------------------------------------------------

/// How far the source can be magnified before it is upscaled: its pixels
/// over the output pixels the layer covers. Measured at the layer's
/// LARGEST drawn size over its life — a closeup planned against the small
/// moment would blur at the large one — on the export frame, not the
/// canvas, since a 4K export of a 1080p canvas shows twice the pixels.
fn free_zoom(
    project: &ProjectMetadata,
    layer: &ProjectLayer,
    resource: &ProjectResource,
) -> Result<f64, String> {
    let source = crate::layout::resource_source_size(resource).ok_or_else(|| {
        format!(
            "Auto zoom needs the recording's pixel size to know how far it can zoom without \
             blurring, and \"{}\" doesn't say it.",
            resource.display_name
        )
    })?;
    let source = match layer.image_orientation {
        Some(ImageOrientation::RotateLeft) | Some(ImageOrientation::RotateRight) => {
            Size::new(source.height(), source.width())
        }
        _ => source,
    };
    let framed = crate::frame::framed_pixel_size(
        source,
        crate::interpolation::effective_frame(layer, layer.start_time, Some(resource)).as_ref(),
    );
    let settings = &project.composition_settings;
    let canvas = Size::new(settings.canvas_width, settings.canvas_height);
    let (ew, eh) = crate::plan::export_size(project);
    let (scale, _) = crate::layout::letterbox_transform(canvas, Size::new(ew as f64, eh as f64));
    let resources = project.resources.as_deref().unwrap_or(&[]);
    let largest = std::iter::once(layer.start_time)
        .chain(
            layer
                .keyframes
                .iter()
                .filter(|k| k.zoom.is_some() || k.placement.is_some())
                .map(|k| layer.start_time + k.time.max(0.0)),
        )
        .map(|t| {
            crate::interpolation::layer_transform_along_paths(layer, t, settings, resources).zoom
        })
        .fold(0.0f64, f64::max);
    let drawn = crate::layout::media_rect(framed, canvas, largest, 0.0, 0.0);
    let (dw, dh) = (drawn.width() * scale, drawn.height() * scale);
    if dw <= 0.0 || dh <= 0.0 {
        return Err(format!(
            "Auto zoom can't measure how large \"{}\" is drawn.",
            layer.name
        ));
    }
    Ok((framed.width() / dw).min(framed.height() / dh))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    const FULL: [f64; 4] = [0.0, 0.0, 1.0, 1.0];

    /// A 20 s 4K recording on a 720p canvas: 3× of free zoom, so the
    /// amount decides unless a test says otherwise.
    fn project_with(resource: Value, layer: Value) -> ProjectMetadata {
        let mut r = json!({
            "id": "R", "kind": "video", "filename": "r.mov", "displayName": "Rec",
            "addedAt": 0, "duration": 20.0, "pixelWidth": 3840.0, "pixelHeight": 2160.0
        });
        merge(&mut r, resource);
        let mut l = json!({
            "id": "L", "name": "Screen", "sortIndex": 0, "kind": "video", "isEnabled": true,
            "startTime": 0, "duration": 20.0, "resourceID": "R", "keyframes": []
        });
        merge(&mut l, layer);
        let p = json!({
            "id": "P", "name": "p", "createdAt": 0, "state": "recorded", "trimStart": 0,
            "trimEnd": 0, "videoDuration": 0, "subtitles": [],
            "compositionSettings": {"canvasWidth": 1280, "canvasHeight": 720},
            "resources": [r], "layers": [l]
        });
        ProjectMetadata::from_json(&p.to_string()).expect("project")
    }

    fn merge(into: &mut Value, extra: Value) {
        if let (Some(a), Value::Object(b)) = (into.as_object_mut(), extra) {
            a.extend(b);
        }
    }

    fn clicks(clicks: &[[f64; 3]]) -> Value {
        json!({"pointer": {"samples": [[0.0, 0.5, 0.5]], "clicks": clicks}})
    }

    fn layer(p: &ProjectMetadata) -> &ProjectLayer {
        &p.layers.as_ref().unwrap()[0]
    }

    fn planned(p: &ProjectMetadata) -> Plan {
        plan(p, layer(p), &PlanOptions::for_layer(layer(p))).expect("plan")
    }

    fn planned_with(p: &ProjectMetadata, opts: PlanOptions) -> Plan {
        plan(p, layer(p), &opts).expect("plan")
    }

    /// The layer as it renders with the plan written into it.
    fn applied(p: &ProjectMetadata, plan: &Plan) -> ProjectLayer {
        let mut l = layer(p).clone();
        remove_plan(&mut l);
        l.keyframes.extend(plan.keyframes.iter().cloned());
        l.keyframes.sort_by(|a, b| a.time.total_cmp(&b.time));
        l
    }

    fn at(l: &ProjectLayer, t: f64) -> [f64; 4] {
        crate::viewport::layer_viewport(l, t).unwrap_or(FULL)
    }

    fn close(a: [f64; 4], b: [f64; 4]) -> bool {
        a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-4)
    }

    fn contains(w: [f64; 4], x: f64, y: f64) -> bool {
        x >= w[0] && x <= w[0] + w[2] && y >= w[1] && y <= w[1] + w[3]
    }

    #[test]
    fn the_same_input_plans_the_same_keyframes() {
        let p = project_with(clicks(&[[3.0, 0.2, 0.3], [9.0, 0.7, 0.6]]), json!({}));
        let a = planned(&p);
        let b = planned(&p);
        assert_eq!(a, b);
        assert_eq!(
            serde_json::to_string(&a.keyframes).unwrap(),
            serde_json::to_string(&b.keyframes).unwrap()
        );
        assert!(
            a.keyframes.iter().all(is_planned),
            "every keyframe is tagged"
        );
        assert!(a.keyframes.iter().all(|k| k.auto == signature(k)));
    }

    /// A click: anchored wide, in just before it, held through its result,
    /// back out — all on the viewport track alone.
    #[test]
    fn a_click_zooms_in_just_before_and_holds_through_it() {
        let p = project_with(clicks(&[[5.0, 0.8, 0.3]]), json!({}));
        let plan = planned(&p);
        assert_eq!(plan.shots.len(), 1, "{:?}", plan.shots);
        let shot = &plan.shots[0];
        assert!(
            (shot.zoom - 1.8).abs() < 1e-9,
            "normal amount: {}",
            shot.zoom
        );
        assert!(
            (shot.start - 4.6).abs() < 1e-9,
            "arrives before the click's lead-in"
        );
        assert!(
            (shot.end - 6.3).abs() < 1e-9,
            "holds past the result: {}",
            shot.end
        );
        assert!(contains(shot.window, 0.8, 0.3), "{:?}", shot.window);
        assert!((shot.window[2] - 1.0 / 1.8).abs() < 1e-4);

        let times: Vec<f64> = plan.keyframes.iter().map(|k| k.time).collect();
        assert_eq!(times, vec![3.6, 4.6, 7.3], "anchor, closeup, return");
        assert_eq!(plan.keyframes[1].easing, Some(Easing::EaseOut));
        assert_eq!(plan.keyframes[2].easing, Some(Easing::EaseInOut));
        assert!(plan
            .keyframes
            .iter()
            .all(|k| k.zoom.is_none() && k.vertical_shift.is_none() && k.opacity.is_none()));

        let l = applied(&p, &plan);
        assert!(close(at(&l, 0.0), FULL), "opens wide");
        assert!(close(at(&l, 3.6), FULL));
        assert!(close(at(&l, 5.0), shot.window), "in on the click");
        assert!(close(at(&l, 6.3), shot.window), "still holding");
        assert!(!close(at(&l, 6.8), shot.window), "pulling out");
        assert!(close(at(&l, 7.3), FULL));
        assert!(close(at(&l, 19.0), FULL));
    }

    #[test]
    fn a_dwell_after_travel_is_interest_and_the_opening_rest_is_not() {
        let p = project_with(
            json!({"pointer": {"samples": [
                [0.0, 0.1, 0.1], [1.0, 0.6, 0.6], [1.5, 0.605, 0.6], [3.0, 0.1, 0.9],
                [3.1, 0.1, 0.9]
            ]}}),
            json!({}),
        );
        let plan = planned(&p);
        assert_eq!(plan.shots.len(), 2, "{:?}", plan.shots);
        assert!(contains(plan.shots[0].window, 0.6, 0.6));
        assert!(plan.shots[0].start < 1.0, "arrives as the rest begins");
        // The pointer rests at its last spot from 3.0 to the recording's end.
        assert!(contains(plan.shots[1].window, 0.1, 0.9));
    }

    #[test]
    fn measured_activity_alone_is_enough() {
        let p = project_with(
            json!({"activity": {"meter": 1, "rate": 10, "events": [
                [3.0, 4.0, 0.1, 0.1, 0.2, 0.1, 0.8],
                [8.0, 9.0, 0.5, 0.5, 0.1, 0.1, 0.05]
            ]}}),
            json!({}),
        );
        let plan = planned(&p);
        assert_eq!(plan.shots.len(), 1, "the light burst is noise");
        let w = plan.shots[0].window;
        assert!(contains(w, 0.1, 0.1) && contains(w, 0.3, 0.2), "{w:?}");
        assert!((plan.shots[0].start - 2.9).abs() < 1e-9);
    }

    #[test]
    fn a_click_wins_over_the_activity_it_caused() {
        let p = project_with(
            json!({
                "pointer": {"samples": [[0.0, 0.5, 0.5]], "clicks": [[5.0, 0.4, 0.4]]},
                "activity": {"meter": 1, "rate": 10,
                    "events": [[5.0, 5.5, 0.3, 0.3, 0.2, 0.2, 0.9]]}
            }),
            json!({}),
        );
        let plan = planned(&p);
        assert_eq!(plan.shots.len(), 1);
        assert!(
            (plan.shots[0].start - 4.6).abs() < 1e-9,
            "the click's timing"
        );
    }

    #[test]
    fn nothing_to_go_on_is_said_plainly() {
        let p = project_with(json!({}), json!({}));
        let err = plan(&p, layer(&p), &PlanOptions::default()).unwrap_err();
        assert_eq!(
            err,
            "Nothing to zoom to yet: this recording has no pointer track and hasn't been measured."
        );
    }

    #[test]
    fn only_a_plain_video_or_image_layer_is_planned() {
        let p = project_with(clicks(&[[5.0, 0.5, 0.5]]), json!({"kind": "caption"}));
        assert!(plan(&p, layer(&p), &PlanOptions::default()).is_err());
        let p = project_with(
            clicks(&[[5.0, 0.5, 0.5]]),
            json!({"keyframes": [{"id": "k", "time": 2, "transitionDuration": 0,
                                  "playback": "pause"}]}),
        );
        let err = plan(&p, layer(&p), &PlanOptions::default()).unwrap_err();
        assert!(err.contains("playback is keyframed"), "{err}");
    }

    /// Close enough to pan at a calm pace, too far at normal: the same two
    /// clicks either chain closeup-to-closeup or return wide in between.
    #[test]
    fn near_shots_pan_and_far_ones_return_by_pace() {
        let p = project_with(clicks(&[[3.0, 0.2, 0.2], [9.0, 0.8, 0.8]]), json!({}));
        let full_frames = |plan: &Plan| {
            plan.keyframes
                .iter()
                .filter(|k| k.viewport == Some(FULL))
                .count()
        };
        let normal = planned(&p);
        assert_eq!(normal.shots.len(), 2);
        assert_eq!(full_frames(&normal), 3, "anchor and two returns");

        let calm = planned_with(
            &p,
            PlanOptions {
                pace: AutoZoomPace::Calm,
                ..Default::default()
            },
        );
        assert_eq!(full_frames(&calm), 2, "anchor and the last return");
        let second = calm
            .keyframes
            .iter()
            .find(|k| k.viewport == Some(calm.shots[1].window))
            .unwrap();
        assert_eq!(
            second.easing,
            Some(Easing::EaseInOut),
            "a pan, eased both ends"
        );
        let l = applied(&p, &calm);
        for t in [4.0, 6.0, 7.0] {
            assert!(!is_full(at(&l, t)), "never wide between the pair at {t}");
        }

        let brisk = planned_with(
            &p,
            PlanOptions {
                pace: AutoZoomPace::Brisk,
                ..Default::default()
            },
        );
        assert_eq!(full_frames(&brisk), 3);
    }

    #[test]
    fn overlapping_shots_pan_without_losing_either_action() {
        let p = project_with(clicks(&[[3.0, 0.2, 0.2], [5.5, 0.8, 0.8]]), json!({}));
        let plan = planned(&p);
        assert_eq!(plan.shots.len(), 2, "too far apart to share one window");
        let times: Vec<f64> = plan.keyframes.iter().map(|k| k.time).collect();
        assert!(times.windows(2).all(|w| w[1] > w[0]), "{times:?}");
        let l = applied(&p, &plan);
        assert!(close(at(&l, 3.0), plan.shots[0].window));
        assert!(close(at(&l, 5.5), plan.shots[1].window));
        assert!(!is_full(at(&l, 4.6)), "panning, not returning");
    }

    #[test]
    fn a_person_s_full_frame_keyframe_first_means_no_anchor() {
        let p = project_with(
            clicks(&[[5.0, 0.5, 0.5]]),
            json!({"keyframes": [{"id": "mine", "time": 0, "transitionDuration": 0,
                                  "viewport": [0, 0, 1, 1]}]}),
        );
        let plan = planned(&p);
        assert_eq!(plan.keyframes.len(), 2, "closeup and return only");
        let p = project_with(clicks(&[[5.0, 0.5, 0.5]]), json!({}));
        assert_eq!(planned(&p).keyframes[0].viewport, Some(FULL));
        assert_eq!(planned(&p).keyframes[0].transition_duration, 0.0);
    }

    #[test]
    fn a_closeup_near_the_end_holds_to_the_last_frame() {
        let p = project_with(clicks(&[[18.5, 0.5, 0.5]]), json!({}));
        let plan = planned(&p);
        assert_eq!(plan.shots[0].end, 20.0);
        let last = plan.keyframes.last().unwrap();
        assert_ne!(last.viewport, Some(FULL), "no half-finished pull-out");
        let l = applied(&p, &plan);
        assert!(close(at(&l, 19.99), plan.shots[0].window));
    }

    #[test]
    fn free_zoom_caps_every_shot_and_too_little_is_refused() {
        // 1080p on a 720p canvas: 1.5× before a source pixel is stretched.
        let p = project_with(
            json!({"pixelWidth": 1920.0, "pixelHeight": 1080.0,
                   "pointer": {"samples": [[0.0, 0.5, 0.5]], "clicks": [[5.0, 0.5, 0.5]]}}),
            json!({}),
        );
        let strong = planned_with(
            &p,
            PlanOptions {
                amount: AutoZoomAmount::Strong,
                ..Default::default()
            },
        );
        assert!((strong.free_zoom - 1.5).abs() < 1e-9);
        assert!((strong.shots[0].zoom - 1.5).abs() < 1e-9);
        // The source exactly fills the canvas: no room at all.
        let p = project_with(
            json!({"pixelWidth": 1280.0, "pixelHeight": 720.0,
                   "pointer": {"samples": [[0.0, 0.5, 0.5]], "clicks": [[5.0, 0.5, 0.5]]}}),
            json!({}),
        );
        let err = plan(&p, layer(&p), &PlanOptions::default()).unwrap_err();
        assert_eq!(
            err,
            "The recording is too small to zoom into without blurring (1.0× available)."
        );
        // A layer drawn at twice its size has half the room.
        let p = project_with(
            clicks(&[[5.0, 0.5, 0.5]]),
            json!({"keyframes": [{"id": "z", "time": 0, "transitionDuration": 0, "zoom": 2.0}]}),
        );
        assert!((planned(&p).free_zoom - 1.5).abs() < 1e-9);
    }

    #[test]
    fn a_wide_burst_zooms_only_as_far_as_it_fits() {
        let p = project_with(
            json!({"activity": {"meter": 1, "rate": 10, "events": [
                [3.0, 4.0, 0.2, 0.2, 0.5, 0.3, 0.8]
            ]}}),
            json!({}),
        );
        let plan = planned(&p);
        let fit = 1.0 / (0.5 * 1.15);
        assert!(
            (plan.shots[0].zoom - fit).abs() < 1e-9,
            "{}",
            plan.shots[0].zoom
        );
        let w = plan.shots[0].window;
        assert!(contains(w, 0.2, 0.2) && contains(w, 0.7, 0.5), "{w:?}");
        // So wide it would barely move: no shot.
        let p = project_with(
            json!({"activity": {"meter": 1, "rate": 10, "events": [
                [3.0, 4.0, 0.1, 0.1, 0.8, 0.5, 0.8]
            ]}}),
            json!({}),
        );
        assert!(planned(&p).shots.is_empty());
    }

    #[test]
    fn a_shot_never_holds_across_a_cut() {
        let p = project_with(
            json!({"activity": {"meter": 1, "rate": 10,
                "events": [[2.0, 6.0, 0.1, 0.1, 0.2, 0.2, 0.8]], "cuts": [4.0]}}),
            json!({}),
        );
        let plan = planned(&p);
        assert!(plan.shots.len() >= 2, "{:?}", plan.shots);
        for shot in &plan.shots {
            assert!(
                !(shot.start < 4.0 - 1e-9 && shot.end > 4.0 + 1e-9),
                "spans the cut: {shot:?}"
            );
        }
    }

    fn spans(shot: &Shot, t: f64) -> bool {
        shot.start < t - 1e-9 && shot.end > t + 1e-9
    }

    /// The last-frame hold gives way to a cut or a scroll near the end:
    /// the closeup leaves in time to be wide by the last frame.
    #[test]
    fn a_late_cut_or_scroll_beats_the_hold_to_the_end() {
        let p = project_with(
            json!({"pointer": {"samples": [[0.0, 0.5, 0.5]], "clicks": [[18.0, 0.2, 0.2]]},
                   "activity": {"meter": 1, "rate": 10, "events": [], "cuts": [19.3]}}),
            json!({}),
        );
        let plan = planned(&p);
        assert_eq!(plan.shots.len(), 1, "{:?}", plan.shots);
        assert!(!spans(&plan.shots[0], 19.3), "{:?}", plan.shots[0]);
        assert_eq!(plan.keyframes.last().unwrap().viewport, Some(FULL));
        assert!(plan.keyframes.iter().all(|k| k.time <= 20.0 + 1e-9));
        let l = applied(&p, &plan);
        assert!(is_full(at(&l, 20.0)), "wide by the last frame");

        let p = project_with(
            json!({"pointer": {"samples": [[0.0, 0.5, 0.5]], "clicks": [[17.8, 0.2, 0.2]]},
                   "activity": {"meter": 1, "rate": 10, "events": [],
                                "scrolls": [[19.2, 19.9]]}}),
            json!({}),
        );
        let plan = planned(&p);
        assert_eq!(plan.shots.len(), 1, "{:?}", plan.shots);
        assert!(plan.shots[0].end <= 19.2 + 1e-9, "{:?}", plan.shots[0]);
        assert_eq!(plan.keyframes.last().unwrap().viewport, Some(FULL));
    }

    /// An action just after a cut: the zoom-in starts on the new screen.
    #[test]
    fn a_zoom_in_never_starts_before_the_cut_that_brought_its_screen() {
        let p = project_with(
            json!({"pointer": {"samples": [[0.0, 0.5, 0.5]], "clicks": [[5.2, 0.2, 0.2]]},
                   "activity": {"meter": 1, "rate": 10, "events": [], "cuts": [5.0]}}),
            json!({}),
        );
        let plan = planned(&p);
        assert_eq!(plan.shots.len(), 1, "{:?}", plan.shots);
        assert!(!spans(&plan.shots[0], 5.0), "{:?}", plan.shots[0]);
        let closeup = plan
            .keyframes
            .iter()
            .find(|k| k.viewport == Some(plan.shots[0].window))
            .unwrap();
        assert!(
            closeup.time - closeup.transition_duration >= 5.0 - 1e-9,
            "ramps on the new screen: {closeup:?}"
        );
        let l = applied(&p, &plan);
        assert!(is_full(at(&l, 4.99)), "the old screen stays wide");
    }

    /// A meter at 10 fps reports a popover or a keystroke as one frame.
    #[test]
    fn a_one_frame_burst_still_earns_a_closeup() {
        let p = project_with(
            json!({"activity": {"meter": 1, "rate": 10, "events": [
                [3.0, 3.0, 0.1, 0.1, 0.1, 0.1, 1.0],
                [12.0, 12.1, 0.7, 0.7, 0.1, 0.1, 1.0]
            ]}}),
            json!({}),
        );
        let plan = planned(&p);
        assert_eq!(plan.shots.len(), 2, "{:?}", plan.shots);
        assert!(contains(plan.shots[0].window, 0.15, 0.15));
        assert!(contains(plan.shots[1].window, 0.75, 0.75));
        assert!((plan.shots[0].start - 2.9).abs() < 1e-9);
    }

    /// A looping layer jumps back to its start at every seam: a cut.
    #[test]
    fn a_loop_seam_is_a_cut() {
        let p = project_with(
            json!({"duration": 5.0,
                   "pointer": {"samples": [[0.0, 0.5, 0.5]], "clicks": [[2.0, 0.2, 0.2]]}}),
            json!({"duration": 16.0, "beyondEnd": "loop"}),
        );
        let plan = planned(&p);
        assert!(plan.shots.len() >= 3, "one per pass: {:?}", plan.shots);
        for shot in &plan.shots {
            for seam in [5.0, 10.0, 15.0] {
                assert!(!spans(shot, seam), "spans the seam at {seam}: {shot:?}");
            }
        }
        assert!(
            plan.keyframes
                .iter()
                .all(|k| k.easing != Some(Easing::EaseInOut) || k.viewport == Some(FULL)),
            "no pan between the same window either side of a seam"
        );
    }

    #[test]
    fn no_zoom_in_starts_during_a_scroll_and_a_scroll_pulls_wide() {
        let p = project_with(
            json!({"activity": {"meter": 1, "rate": 10,
                "events": [[3.0, 6.0, 0.1, 0.1, 0.2, 0.2, 0.8]], "scrolls": [[2.0, 3.5]]}}),
            json!({}),
        );
        let plan = planned(&p);
        let l = applied(&p, &plan);
        for t in [2.0, 2.5, 3.0, 3.5] {
            assert!(is_full(at(&l, t)), "wide while scrolling at {t}");
        }
        assert_eq!(plan.shots.len(), 1);

        let p = project_with(
            json!({"activity": {"meter": 1, "rate": 10,
                "events": [[1.0, 5.0, 0.1, 0.1, 0.2, 0.2, 0.8]], "scrolls": [[3.0, 6.0]]}}),
            json!({}),
        );
        let plan = planned(&p);
        assert_eq!(plan.shots[0].end, 3.0, "leaves as the scroll starts");
        let l = applied(&p, &plan);
        assert!(!is_full(at(&l, 3.0)));
        assert!(is_full(at(&l, 4.0)));
        assert!(is_full(at(&l, 5.5)), "no zoom back in mid-scroll");
    }

    #[test]
    fn skip_spans_stay_wide() {
        let p = project_with(
            clicks(&[[5.0, 0.5, 0.5], [12.0, 0.5, 0.5]]),
            json!({"autoZoom": {"skip": [[4.0, 7.0]]}}),
        );
        let plan = planned(&p);
        assert_eq!(plan.shots.len(), 1);
        assert!(plan.shots[0].start > 11.0);
        let l = applied(&p, &plan);
        for t in [3.85, 4.0, 5.0, 7.0, 7.15] {
            assert!(is_full(at(&l, t)), "wide at {t}");
        }
    }

    /// The person's own closeup (in at 6, out by 9) is never touched, and
    /// no planned move lands in it or its ramps.
    #[test]
    fn a_person_s_own_viewport_move_is_untouched_and_avoided() {
        let mine = json!([
            {"id": "m0", "time": 0, "transitionDuration": 0, "viewport": [0, 0, 1, 1]},
            {"id": "m1", "time": 6, "transitionDuration": 1, "viewport": [0.5, 0.5, 0.5, 0.5]},
            {"id": "m2", "time": 9, "transitionDuration": 1, "viewport": [0, 0, 1, 1]}
        ]);
        let p = project_with(
            clicks(&[[7.0, 0.2, 0.2], [13.0, 0.2, 0.2]]),
            json!({ "keyframes": mine }),
        );
        let plan = planned(&p);
        assert_eq!(plan.shots.len(), 1, "{:?}", plan.shots);
        assert!(plan.shots[0].start > 12.0);
        assert!(plan
            .keyframes
            .iter()
            .all(|k| !(4.85..=9.15).contains(&k.time)));
        assert!(plan.keyframes.iter().all(|k| !k.id.starts_with('m')));
        let l = applied(&p, &plan);
        let before = layer(&p);
        for id in ["m0", "m1", "m2"] {
            let a = before.keyframes.iter().find(|k| k.id == id).unwrap();
            let b = l.keyframes.iter().find(|k| k.id == id).unwrap();
            assert_eq!(a, b);
        }
        assert!(
            close(at(&l, 7.5), [0.5, 0.5, 0.5, 0.5]),
            "theirs still plays"
        );
    }

    /// A person's closeup with no full-frame anchor holds from zero: the
    /// whole stretch before it is theirs.
    #[test]
    fn an_unanchored_manual_closeup_holds_from_the_start() {
        let p = project_with(
            clicks(&[[2.0, 0.2, 0.2], [14.0, 0.2, 0.2]]),
            json!({"keyframes": [
                {"id": "m1", "time": 6, "transitionDuration": 1, "viewport": [0.5, 0.5, 0.5, 0.5]},
                {"id": "m2", "time": 9, "transitionDuration": 1, "viewport": [0, 0, 1, 1]}
            ]}),
        );
        let plan = planned(&p);
        assert_eq!(plan.shots.len(), 1);
        assert!(plan.shots[0].start > 13.0);
    }

    #[test]
    fn an_edited_planned_keyframe_becomes_the_person_s() {
        let p = project_with(clicks(&[[5.0, 0.5, 0.5], [14.0, 0.3, 0.3]]), json!({}));
        let first = planned(&p);
        let mut edited = applied(&p, &first);
        // The person nudges the first closeup's window.
        let closeup = edited
            .keyframes
            .iter_mut()
            .find(|k| k.viewport == Some(first.shots[0].window))
            .unwrap();
        closeup.viewport = Some([0.1, 0.1, 0.5, 0.5]);
        let nudged = closeup.id.clone();
        assert!(!is_planned(closeup), "no longer matches its tag");
        let mut p2 = p.clone();
        p2.layers.as_mut().unwrap()[0] = edited;
        let second = planned(&p2);
        assert!(second.keyframes.iter().all(|k| k.id != nudged));
        assert_eq!(second.shots.len(), 1, "the edited one's span is theirs now");
        assert!(second.shots[0].start > 13.0);
    }

    #[test]
    fn an_added_field_makes_a_planned_keyframe_the_person_s() {
        let p = project_with(clicks(&[[5.0, 0.5, 0.5]]), json!({}));
        let mut k = planned(&p).keyframes[1].clone();
        assert!(is_planned(&k));
        k.opacity = Some(0.5);
        assert!(
            !is_planned(&k),
            "the signature holds, but it carries more now"
        );
    }

    #[test]
    fn a_trimmed_and_sped_recording_zooms_on_the_moment_it_shows() {
        // Source 4 s onward; a click at source 9 shows 5 s into the layer.
        let p = project_with(
            json!({"trimStart": 4.0, "trimEnd": 20.0,
                   "pointer": {"samples": [[0.0, 0.5, 0.5]], "clicks": [[9.0, 0.5, 0.5]]}}),
            json!({"duration": 16.0}),
        );
        assert!((planned(&p).shots[0].start - 4.6).abs() < 1e-9);
        // At 2×, the same click shows at 2.5 s.
        let p = project_with(
            json!({"trimStart": 4.0, "trimEnd": 20.0, "speed": 2.0,
                   "pointer": {"samples": [[0.0, 0.5, 0.5]], "clicks": [[9.0, 0.5, 0.5]]}}),
            json!({"duration": 8.0}),
        );
        let plan = planned(&p);
        let l = applied(&p, &plan);
        assert!(close(at(&l, 2.5), plan.shots[0].window));
        // A click in the trimmed-off head is not on screen at all.
        let p = project_with(
            json!({"trimStart": 4.0, "trimEnd": 20.0,
                   "pointer": {"samples": [[0.0, 0.5, 0.5]], "clicks": [[2.0, 0.5, 0.5]]}}),
            json!({"duration": 16.0}),
        );
        assert!(planned(&p).shots.is_empty());
    }

    #[test]
    fn ids_are_unique_across_the_project_and_stable() {
        let p = project_with(clicks(&[[3.0, 0.2, 0.2], [12.0, 0.7, 0.7]]), json!({}));
        let plan = planned(&p);
        let ids: Vec<&str> = plan.keyframes.iter().map(|k| k.id.as_str()).collect();
        assert_eq!(ids[0], "L-auto-0");
        let unique: std::collections::HashSet<&&str> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
        // Re-planning over the written plan reuses the same ids.
        let mut p2 = p.clone();
        p2.layers.as_mut().unwrap()[0] = applied(&p, &plan);
        assert_eq!(planned(&p2), plan);
    }
}
