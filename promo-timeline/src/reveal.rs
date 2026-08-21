//! How much of a caption has arrived.
//!
//! A reveal is a RULE — how fast, by what unit — resolved at every read, the
//! way placement and the fade shorthand are. Baking it into keyframes would
//! be dozens per caption, and every one of them would go stale the moment
//! the words or the font changed.

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
}

/// How far the reveal has got at `time`, for a caption of `units` units.
///
/// Counts from the LAYER's start, not the project's: a caption that appears
/// at 0:12 starts typing when it appears.
pub fn progress(
    reveal: &TextReveal,
    layer: &ProjectLayer,
    time: f64,
    units: usize,
) -> Progress {
    if units == 0 {
        return Progress { shown: 0, active: None, fraction: 1.0 };
    }
    let total = reveal.total_seconds(units, layer.duration);
    let elapsed = time - layer.start_time;
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
    Progress { shown, active, fraction }
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
    mode: RevealMode,
) -> Vec<Band> {
    if layout.width <= 0.0 || layout.height <= 0.0 {
        return Vec::new();
    }
    let highlight = mode == RevealMode::Highlight;
    let mut out = Vec::new();

    for (line, top) in layout.line_tops.iter().enumerate() {
        let line = line as u32;
        let on_line: Vec<(usize, &promo_text::UnitSpan)> = layout
            .units
            .iter()
            .enumerate()
            .filter(|(_, unit)| unit.line == line)
            .collect();
        if on_line.is_empty() {
            continue;
        }
        // A highlight shows everything and tints one unit; a wipe shows only
        // what has arrived.
        let visible: Vec<&(usize, &promo_text::UnitSpan)> = if highlight {
            on_line.iter().collect()
        } else {
            on_line.iter().filter(|(index, _)| *index < progress.shown).collect()
        };
        if visible.is_empty() {
            continue;
        }
        let v = top / layout.height;
        let dv = layout.line_height / layout.height;

        if highlight {
            // The whole line, then the active unit again on top of it.
            let left = visible.iter().map(|(_, u)| u.start_x).fold(f64::MAX, f64::min);
            let right = visible.iter().map(|(_, u)| u.end_x).fold(0.0f64, f64::max);
            out.push(Band {
                uv: [left / layout.width, v, (right - left) / layout.width, dv],
                active: false,
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
                    });
                }
            }
        } else {
            let left = visible.iter().map(|(_, u)| u.start_x).fold(f64::MAX, f64::min);
            let right = visible.iter().map(|(_, u)| u.end_x).fold(0.0f64, f64::max);
            // From the raster's left edge, so the plate and any padding
            // travel with the wipe rather than popping in at the first word.
            out.push(Band {
                uv: [0.0, v, right / layout.width, dv],
                active: false,
            });
            let _ = left;
        }
    }
    out
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
        let one = bands(&l, Progress { shown: 1, active: Some(0), fraction: 0.5 }, RevealMode::Wipe);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].uv, [0.0, 0.0, 60.0 / 200.0, 1.0]);

        let both = bands(&l, Progress { shown: 2, active: None, fraction: 1.0 }, RevealMode::Wipe);
        assert_eq!(both[0].uv, [0.0, 0.0, 120.0 / 200.0, 1.0]);

        let none = bands(&l, Progress { shown: 0, active: None, fraction: 0.0 }, RevealMode::Wipe);
        assert!(none.is_empty(), "nothing has arrived yet");
    }

    /// Line two waits for line one to finish.
    #[test]
    fn a_second_line_does_not_start_before_the_first_ends() {
        let l = layout(&[(0, 10.0, 60.0), (0, 70.0, 120.0), (1, 10.0, 90.0)], 2);
        let mid = bands(&l, Progress { shown: 2, active: Some(1), fraction: 0.6 }, RevealMode::Wipe);
        assert_eq!(mid.len(), 1, "still on the first line");
        assert_eq!(mid[0].uv[1], 0.0);

        let all = bands(&l, Progress { shown: 3, active: None, fraction: 1.0 }, RevealMode::Wipe);
        assert_eq!(all.len(), 2, "both lines now");
        assert_eq!(all[1].uv[1], 0.5, "the second band is the second line");
    }

    /// A highlight shows the whole line and marks one unit — the band the
    /// renderer draws in the highlight colour.
    #[test]
    fn a_highlight_shows_everything_and_marks_the_active_word() {
        let l = layout(&[(0, 10.0, 60.0), (0, 70.0, 120.0)], 1);
        let out = bands(&l, Progress { shown: 1, active: Some(1), fraction: 0.5 },
                        RevealMode::Highlight);
        assert_eq!(out.len(), 2, "the line, then the active word over it");
        assert!(!out[0].active);
        assert!(out[1].active);
        assert_eq!(out[1].uv, [70.0 / 200.0, 0.0, 50.0 / 200.0, 1.0]);
    }
}
