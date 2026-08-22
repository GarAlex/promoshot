//! How much of a caption has arrived.
//!
//! A reveal is a RULE — how fast, by what unit — resolved at every read, the
//! way placement and the fade shorthand are. Baking it into keyframes would
//! be dozens per caption, and every one of them would go stale the moment
//! the words or the font changed.

use crate::transition::Effect;
use promo_model::{Easing, ProjectLayer, RevealMode, RevealUnit, TextReveal};

/// Which units are showing at an instant, and which one is arriving now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    /// Units fully or partly arrived — the count to reveal.
    pub shown: usize,
    /// The unit currently arriving, for a highlight. `None` once the walk is
    /// over, so a karaoke line does not leave its last word lit forever.
    pub active: Option<usize>,
    /// Fraction of the whole walk, eased.
    pub fraction: f64,
    /// How long the whole walk lasts, in seconds — what an arrival time has
    /// to be measured against to become a fraction of it.
    pub total: f64,
}

/// How far the reveal has got at `time`, for a caption of `units` units.
///
/// Counts from the showing RESOURCE's tenure, not the layer's start: a
/// caption that appears at 0:12 starts typing when it appears — and a
/// statement SWAPPED in at 0:07 starts typing at 0:07, rather than
/// arriving with its reveal already spent. Found the honest way: the first
/// template to put a karaoke line on a swap keyframe never showed the
/// highlight, because the walk had finished before the words arrived.
pub fn progress(
    reveal: &TextReveal,
    layer: &ProjectLayer,
    time: f64,
    units: usize,
) -> Progress {
    if units == 0 {
        return Progress { shown: 0, active: None, fraction: 1.0, total: 0.0 };
    }
    let (tenure_start, tenure_end) = crate::transition::tenure(layer, time);
    let tenure_len = tenure_end.map(|end| (end - tenure_start).max(0.0));
    let total = reveal.total_seconds(units, tenure_len);
    let elapsed = crate::layer_local_time(layer, time) - tenure_start;
    let raw = if total > 0.0 { elapsed / total } else { 1.0 };
    let clamped = raw.clamp(0.0, 1.0);
    let fraction = reveal.easing.unwrap_or(Easing::Linear).apply(clamped);

    // At least one, so the caption is never on screen with nothing in it:
    // an empty first frame reads as a caption that failed to appear rather
    // than one about to type. Ceil after that, so the last unit lands as the
    // walk ends rather than a unit early.
    let shown = ((fraction * units as f64).ceil() as usize).clamp(1, units);
    let active = if fraction >= 1.0 {
        None
    } else {
        Some(shown.saturating_sub(1).min(units - 1))
    };
    Progress { shown, active, fraction, total }
}

/// The unit a reveal walks, in the rasterizer's vocabulary.
pub fn unit_of(reveal: &TextReveal) -> promo_text::RevealBy {
    match reveal.by {
        RevealUnit::Character => promo_text::RevealBy::Character,
        RevealUnit::Word => promo_text::RevealBy::Word,
        RevealUnit::Line => promo_text::RevealBy::Line,
    }
}

/// A band of the caption raster to draw: one line, cropped to what has
/// arrived, as fractions of the raster.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    /// `[u, v, width, height]` in 0…1 of the raster.
    pub uv: [f64; 4],
    /// True when this band carries the unit currently arriving — what a
    /// highlight tints and a wipe does not care about.
    pub active: bool,
    /// What this band does on its way in. Identity for a wipe, where a unit
    /// is simply there; a small move for the staggered modes.
    ///
    /// The offset is a fraction of the WHOLE raster's height — the walk knows
    /// the text's proportions but not how big it is drawn. Use
    /// [`Band::effect_for`] rather than reaching for this directly.
    pub effect: Effect,
}

impl Band {
    /// This band's effect in canvas units, given the height the whole raster
    /// occupies there.
    pub fn effect_for(&self, raster_height: f64) -> Effect {
        Effect {
            offset: [
                self.effect.offset[0] * raster_height,
                self.effect.offset[1] * raster_height,
            ],
            ..self.effect
        }
    }
}

/// The bands to draw for `progress`.
///
/// One per line that has anything showing, cropped at the right edge of the
/// last unit that has arrived on it. A line whose turn has not come yields
/// nothing; a line already passed yields its whole width. That ordering is
/// what makes a two-line caption finish line one before starting line two.
pub fn bands(
    layout: &promo_text::RevealLayout,
    progress: Progress,
    reveal: &TextReveal,
) -> Vec<Band> {
    if layout.width <= 0.0 || layout.height <= 0.0 {
        return Vec::new();
    }
    match reveal.mode {
        RevealMode::Highlight => highlight_bands(layout, progress),
        _ if reveal.animates() => stagger_bands(layout, progress, reveal),
        _ => wipe_bands(layout, progress),
    }
}

fn line_v(layout: &promo_text::RevealLayout, line: u32) -> (f64, f64) {
    let top = layout.line_tops.get(line as usize).copied().unwrap_or(0.0);
    (top / layout.height, layout.line_height / layout.height)
}

/// Write-on: each line cropped to the last unit that has arrived.
///
/// From the raster's left edge, so the plate and its padding travel with the
/// text rather than the words appearing in mid air.
fn wipe_bands(layout: &promo_text::RevealLayout, progress: Progress) -> Vec<Band> {
    let mut out = Vec::new();
    for line in 0..layout.line_tops.len() as u32 {
        let arrived: Vec<&promo_text::UnitSpan> = layout
            .units
            .iter()
            .enumerate()
            .filter(|(index, unit)| unit.line == line && *index < progress.shown)
            .map(|(_, unit)| unit)
            .collect();
        if arrived.is_empty() {
            continue;
        }
        let right = arrived.iter().map(|u| u.end_x).fold(0.0f64, f64::max);
        let (v, dv) = line_v(layout, line);
        out.push(Band {
            uv: [0.0, v, right / layout.width, dv],
            active: false,
            effect: Effect::IDENTITY,
        });
    }
    out
}

/// Karaoke: the whole line, then the active unit again over it.
fn highlight_bands(layout: &promo_text::RevealLayout, progress: Progress) -> Vec<Band> {
    let mut out = Vec::new();
    for line in 0..layout.line_tops.len() as u32 {
        let on_line: Vec<(usize, &promo_text::UnitSpan)> = layout
            .units
            .iter()
            .enumerate()
            .filter(|(_, unit)| unit.line == line)
            .collect();
        if on_line.is_empty() {
            continue;
        }
        let (v, dv) = line_v(layout, line);
        let left = on_line.iter().map(|(_, u)| u.start_x).fold(f64::MAX, f64::min);
        let right = on_line.iter().map(|(_, u)| u.end_x).fold(0.0f64, f64::max);
        out.push(Band {
            uv: [left / layout.width, v, (right - left) / layout.width, dv],
            active: false,
            effect: Effect::IDENTITY,
        });
        if let Some(active) = progress.active {
            if let Some((_, unit)) = on_line.iter().find(|(index, _)| *index == active) {
                out.push(Band {
                    uv: [
                        unit.start_x / layout.width,
                        v,
                        (unit.end_x - unit.start_x) / layout.width,
                        dv,
                    ],
                    active: true,
                    effect: Effect::IDENTITY,
                });
            }
        }
    }
    out
}

/// Kinetic type: one band per UNIT, each with its own arrival.
///
/// The units keep the places the layout gave them — a stagger is about WHEN
/// each word arrives, not where it sits, so a line holds as many words as it
/// always did and a wrap is a wrap.
fn stagger_bands(
    layout: &promo_text::RevealLayout,
    progress: Progress,
    reveal: &TextReveal,
) -> Vec<Band> {
    let count = layout.units.len().max(1);
    // Consecutive units overlap by half an arrival: several in flight together
    // read as one motion, where a strict queue reads as a list being ticked off.
    let unit_span = reveal
        .unit_seconds
        .filter(|s| *s > 0.0 && progress.total > 0.0)
        .map(|seconds| (seconds / progress.total).clamp(0.01, 1.0))
        .unwrap_or((2.0 / (count + 1) as f64).min(1.0));
    // Starts spread across the room LEFT OVER once the last unit has had its
    // arrival — so the walk ends when the reveal ends, however long an
    // individual unit takes.
    let step = if count > 1 {
        (1.0 - unit_span).max(0.0) / (count - 1) as f64
    } else {
        0.0
    };

    let mut out = Vec::new();
    for (index, unit) in layout.units.iter().enumerate() {
        let started = index as f64 * step;
        let arrival = ((progress.fraction - started) / unit_span).clamp(0.0, 1.0);
        if arrival <= 0.0 {
            continue;
        }
        let (v, dv) = line_v(layout, unit.line);
        let (left, right) = unit_slice(layout, index, unit);
        out.push(Band {
            uv: [
                left / layout.width,
                v,
                (right - left) / layout.width,
                dv,
            ],
            active: false,
            effect: arrival_effect(reveal, arrival, layout.line_height / layout.height),
        });
    }
    out
}

/// The horizontal slice a unit carries with it: out to the midpoint of the
/// gap on each side, and to the raster edge at the ends of a line.
///
/// A unit's span is its GLYPHS. Cropping to that shaves the overhang a
/// slanted or looping letter puts outside its own advance, and leaves the
/// spaces belonging to nobody. Slicing at the midpoints instead tiles the
/// line exactly — no ink clipped, no pixel drawn twice — so once everything
/// has landed a stagger is the same picture as no reveal at all.
fn unit_slice(
    layout: &promo_text::RevealLayout,
    index: usize,
    unit: &promo_text::UnitSpan,
) -> (f64, f64) {
    let neighbour = |step: isize| -> Option<&promo_text::UnitSpan> {
        let at = index as isize + step;
        layout
            .units
            .get(usize::try_from(at).ok()?)
            .filter(|other| other.line == unit.line)
    };
    let left = neighbour(-1)
        .map(|prev| (prev.end_x + unit.start_x) / 2.0)
        .unwrap_or(0.0);
    let right = neighbour(1)
        .map(|next| (unit.end_x + next.start_x) / 2.0)
        .unwrap_or(layout.width);
    (left, right)
}

/// What one unit does on the way in.
fn arrival_effect(reveal: &TextReveal, arrival: f64, line_height: f64) -> Effect {
    match reveal.mode {
        RevealMode::Fade => Effect { opacity: arrival, ..Effect::IDENTITY },
        RevealMode::Rise => Effect {
            opacity: arrival,
            // Up into place. Half a line is far enough to read as movement
            // and near enough not to fly across the frame.
            offset: [0.0, (1.0 - arrival) * reveal.rise.unwrap_or(0.5) * line_height],
            ..Effect::IDENTITY
        },
        RevealMode::Scale => Effect {
            opacity: arrival,
            scale: 0.6 + 0.4 * arrival,
            ..Effect::IDENTITY
        },
        _ => Effect::IDENTITY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caption(duration: Option<f64>) -> ProjectLayer {
        let mut layer: ProjectLayer = serde_json::from_str(
            r#"{"id":"L","name":"L","sortIndex":0,"kind":"caption","isEnabled":true,
                "startTime":2,"keyframes":[]}"#,
        )
        .expect("layer");
        layer.duration = duration;
        layer
    }

    fn rule(json: &str) -> TextReveal {
        serde_json::from_str(json).expect("reveal")
    }

    /// The pace comes from the caption's own life unless it is stated, so a
    /// reveal lands with the caption rather than at some unrelated moment —
    /// which matters because no word timings exist to sync to.
    #[test]
    fn an_unpaced_reveal_spreads_across_the_caption() {
        let layer = caption(Some(4.0));
        let r = rule(r#"{"by":"word"}"#);
        assert_eq!(progress(&r, &layer, 2.0, 4).shown, 1, "the first word is there at once");
        assert_eq!(progress(&r, &layer, 4.0, 4).shown, 2, "half way, half the words");
        assert_eq!(progress(&r, &layer, 6.0, 4).shown, 4, "all of them by the end");
        assert_eq!(progress(&r, &layer, 99.0, 4).shown, 4, "and they stay");
    }

    /// A typewriter usually states its pace instead.
    #[test]
    fn seconds_per_unit_sets_the_pace() {
        let layer = caption(Some(60.0));
        let r = rule(r#"{"by":"character","secondsPer":0.1}"#);
        // Ten characters at a tenth of a second each is one second.
        assert_eq!(progress(&r, &layer, 2.5, 10).shown, 5);
        assert_eq!(progress(&r, &layer, 3.0, 10).shown, 10);
    }

    /// A total wins over a per-unit rate, and validate names the conflict.
    #[test]
    fn a_stated_total_wins_over_a_rate() {
        let layer = caption(Some(60.0));
        let r = rule(r#"{"by":"word","secondsPer":10,"seconds":2}"#);
        assert_eq!(progress(&r, &layer, 3.0, 4).shown, 2, "two seconds total, not forty");
    }

    /// The highlight has to stop somewhere: leaving the last word lit for the
    /// rest of the caption is not karaoke, it is a typo.
    #[test]
    fn the_active_unit_goes_out_when_the_walk_is_over() {
        let layer = caption(Some(4.0));
        let r = rule(r#"{"by":"word","mode":"highlight"}"#);
        assert_eq!(progress(&r, &layer, 3.0, 4).active, Some(0),
                   "a quarter through four words, the first is the live one");
        assert_eq!(progress(&r, &layer, 6.0, 4).active, None);
    }

    fn wipe() -> TextReveal {
        serde_json::from_str(r#"{"by":"word","mode":"wipe"}"#).expect("rule")
    }
    fn highlight() -> TextReveal {
        serde_json::from_str(r#"{"by":"word","mode":"highlight"}"#).expect("rule")
    }

    fn layout(units: &[(u32, f64, f64)], lines: usize) -> promo_text::RevealLayout {
        promo_text::RevealLayout {
            units: units
                .iter()
                .map(|(line, a, b)| promo_text::UnitSpan { line: *line, start_x: *a, end_x: *b })
                .collect(),
            line_tops: (0..lines).map(|i| i as f64 * 50.0).collect(),
            line_height: 50.0,
            width: 200.0,
            height: 50.0 * lines as f64,
        }
    }

    /// A wipe crops from the raster's left edge to the last arrived unit, so
    /// the plate travels with the text instead of the words appearing in mid
    /// air.
    #[test]
    fn a_wipe_crops_the_line_to_what_has_arrived() {
        let l = layout(&[(0, 10.0, 60.0), (0, 70.0, 120.0)], 1);
        let one = bands(&l, Progress { shown: 1, active: Some(0), fraction: 0.5, total: 1.0 }, &wipe());
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].uv, [0.0, 0.0, 60.0 / 200.0, 1.0]);

        let both = bands(&l, Progress { shown: 2, active: None, fraction: 1.0, total: 1.0 }, &wipe());
        assert_eq!(both[0].uv, [0.0, 0.0, 120.0 / 200.0, 1.0]);

        let none = bands(&l, Progress { shown: 0, active: None, fraction: 0.0, total: 1.0 }, &wipe());
        assert!(none.is_empty(), "nothing has arrived yet");
    }

    /// Line two waits for line one to finish.
    #[test]
    fn a_second_line_does_not_start_before_the_first_ends() {
        let l = layout(&[(0, 10.0, 60.0), (0, 70.0, 120.0), (1, 10.0, 90.0)], 2);
        let mid = bands(&l, Progress { shown: 2, active: Some(1), fraction: 0.6, total: 1.0 }, &wipe());
        assert_eq!(mid.len(), 1, "still on the first line");
        assert_eq!(mid[0].uv[1], 0.0);

        let all = bands(&l, Progress { shown: 3, active: None, fraction: 1.0, total: 1.0 }, &wipe());
        assert_eq!(all.len(), 2, "both lines now");
        assert_eq!(all[1].uv[1], 0.5, "the second band is the second line");
    }

    /// A highlight shows the whole line and marks one unit — the band the
    /// renderer draws in the highlight colour.
    #[test]
    fn a_highlight_shows_everything_and_marks_the_active_word() {
        let l = layout(&[(0, 10.0, 60.0), (0, 70.0, 120.0)], 1);
        let out = bands(&l, Progress { shown: 1, active: Some(1), fraction: 0.5, total: 1.0 }, &highlight());
        assert_eq!(out.len(), 2, "the line, then the active word over it");
        assert!(!out[0].active);
        assert!(out[1].active);
        assert_eq!(out[1].uv, [70.0 / 200.0, 0.0, 50.0 / 200.0, 1.0]);
    }

    /// A stagger is the same walk with each unit ARRIVING rather than simply
    /// being there — and the units keep the places the layout gave them, so
    /// a line holds as many words as it always did.
    #[test]
    fn a_stagger_gives_each_unit_its_own_arrival_in_place() {
        let rule: TextReveal =
            serde_json::from_str(r#"{"by":"word","mode":"rise"}"#).expect("rule");
        // Two words on line one, one on line two — the real layout.
        let l = layout(&[(0, 10.0, 60.0), (0, 70.0, 120.0), (1, 10.0, 90.0)], 2);

        let early = bands(&l, Progress { shown: 1, active: Some(0), fraction: 0.1, total: 1.0 }, &rule);
        assert_eq!(early.len(), 1, "only the first word has started");
        assert!(early[0].effect.opacity < 1.0, "and it is still arriving");
        assert!(early[0].effect.offset[1] > 0.0, "from below");
        // The first word carries the start of its line with it: a unit's
        // slice runs to the midpoint of the gap beside it, so the words tile
        // the line rather than each being shaved to its own glyphs.
        assert_eq!(early[0].uv[0], 0.0, "out to the edge of the line");
        assert_eq!(early[0].uv[2], 65.0 / 200.0, "and to the midpoint of the gap");

        let mid = bands(&l, Progress { shown: 2, active: Some(1), fraction: 0.5, total: 1.0 }, &rule);
        assert_eq!(mid.len(), 2, "two in flight");
        assert!(mid[0].effect.opacity > mid[1].effect.opacity,
                "the earlier word is further along than the later one");
        assert_eq!(mid[0].uv[1], 0.0, "first line");
        assert_eq!(mid[1].uv[1], 0.0, "same line — a stagger does not stack words");

        let done = bands(&l, Progress { shown: 3, active: None, fraction: 1.0, total: 1.0 }, &rule);
        assert_eq!(done.len(), 3);
        assert!(done.iter().all(|b| b.effect.is_identity()),
                "everything has landed, so nothing is still moving");
        assert_eq!(done[2].uv[1], 0.5, "the third word is on the second line");

        // Tiling, not overlapping: two words fading in at once must not
        // double-composite where they meet.
        assert_eq!(done[0].uv[0] + done[0].uv[2], done[1].uv[0],
                   "the first word's slice ends exactly where the second's begins");
        assert_eq!(done[1].uv[0] + done[1].uv[2], 1.0, "and the last reaches the edge");
    }

    /// A wipe is the same mechanism with no arrival at all — the unification
    /// worth keeping: a typewriter is a stagger that does not animate.
    #[test]
    fn a_wipe_is_a_stagger_with_nothing_moving() {
        let l = layout(&[(0, 10.0, 60.0), (0, 70.0, 120.0)], 1);
        let out = bands(&l, Progress { shown: 1, active: Some(0), fraction: 0.4, total: 1.0 }, &wipe());
        assert!(out.iter().all(|b| b.effect.is_identity()));
    }

    /// `unitSeconds` is SECONDS, so it has to be measured against how long
    /// the walk actually lasts — which for the common case (no pace stated)
    /// is the layer's own duration, not some constant.
    #[test]
    fn an_arrival_time_is_seconds_of_the_real_walk() {
        let rule: TextReveal = serde_json::from_str(
            r#"{"by":"word","mode":"fade","unitSeconds":1.0}"#).expect("rule");
        let l = layout(&[(0, 0.0, 50.0), (0, 60.0, 110.0)], 1);
        let long = Progress { shown: 1, active: Some(0), fraction: 0.4, total: 10.0 };
        let short = Progress { shown: 1, active: Some(0), fraction: 0.4, total: 2.0 };

        // One second is a TENTH of the ten-second walk and a HALF of the
        // two-second one, so four-tenths of the way through, the first word
        // has long landed in one and is still arriving in the other.
        let slow = bands(&l, long, &rule);
        let quick = bands(&l, short, &rule);
        assert!(slow[0].effect.is_identity(), "a second is a tenth here, long over");
        assert!(
            quick[0].effect.opacity > 0.0 && quick[0].effect.opacity < 1.0,
            "and half of the walk there, so still on its way: {}",
            quick[0].effect.opacity,
        );
    }

    /// A statement swapped in mid-layer starts ITS OWN walk: the reveal
    /// counts from the resource's tenure, so a karaoke line on a swap
    /// keyframe highlights when it is on screen — not before it arrives.
    #[test]
    fn a_swapped_in_caption_starts_its_own_walk() {
        let layer: ProjectLayer = serde_json::from_str(
            r#"{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0C01","name":"Words",
                "sortIndex":0,"kind":"caption","isEnabled":true,
                "startTime":0,"duration":10,
                "resourceID":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0C02",
                "keyframes":[{"id":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0C03",
                  "time":4,"transitionDuration":0,
                  "resourceID":"D65E2A61-33DD-4BA1-B1F6-9F2E5C8B0C04"}]}"#,
        )
        .expect("layer");
        let rule: TextReveal =
            serde_json::from_str(r#"{"by":"word","mode":"wipe"}"#).expect("rule");

        // Just after the swap: the new statement has barely begun. On the
        // layer's clock this instant would be 42% through the walk.
        let just_in = progress(&rule, &layer, 4.2, 6);
        assert!(
            just_in.fraction < 0.1,
            "a swapped-in caption starts typing when it appears, got {}",
            just_in.fraction,
        );
        // And with no stated pace, its walk spreads across ITS tenure
        // (4s..10s), finishing as the layer ends.
        let near_end = progress(&rule, &layer, 9.9, 6);
        assert!(near_end.fraction > 0.95, "got {}", near_end.fraction);
        // The first statement's walk spread across the tenure it actually
        // had — done by the swap, not still typing at 40% of a layer-length
        // walk it never got to finish.
        let first = progress(&rule, &layer, 3.9, 6);
        assert!(first.fraction > 0.95, "got {}", first.fraction);
    }
}
