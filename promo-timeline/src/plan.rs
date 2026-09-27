//! The export clock: one rule for the composition's end, the frame rate
//! and the frame count, so `promo video`, the FFI export job and the apps'
//! export loop count the same frames at the same instants. Before this the
//! app clamped its rate at 240 and the CLI did not; the app rounded its
//! frame count where the CLI had once ceiled it; and every one of them
//! re-derived the composition's end.

use promo_model::ProjectMetadata;

/// The highest rate any export writes.
pub const MAX_FPS: f64 = 240.0;
/// The rate a project that names none renders at.
pub const DEFAULT_FPS: f64 = 30.0;

/// The composition's end for rendering: the furthest any layer with a
/// declared duration runs, or the recorded video's length. A layer with no
/// duration ends where it starts for THIS purpose — its media may run on,
/// but the timeline does not grow to meet it. (The Mac assembler walks the
/// same layers but skips backgrounds and adds its legacy trim fields; the
/// export plan takes the host's end when it wants its own.)
pub fn composition_duration(meta: &ProjectMetadata) -> f64 {
    let from_layers = meta
        .layers
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter_map(|l| l.duration.map(|d| l.start_time.max(0.0) + d.max(0.0)))
        .fold(0.0f64, f64::max);
    from_layers.max(meta.video_duration.max(0.0))
}

/// The rate an export runs at: an explicit override, else the project's
/// own, else [`DEFAULT_FPS`]; never below 1 nor above [`MAX_FPS`]. Zero,
/// negative or non-finite values mean "not given".
pub fn export_fps(project_fps: Option<f64>, override_fps: Option<f64>) -> f64 {
    let given = |f: Option<f64>| f.filter(|f| f.is_finite() && *f > 0.0);
    given(override_fps)
        .or(given(project_fps))
        .unwrap_or(DEFAULT_FPS)
        .clamp(1.0, MAX_FPS)
}

/// Frames in `[start, end)` at `fps`: rounded, never zero — a zero-length
/// composition still yields its poster frame. Rounding rather than ceiling
/// is what the CLI always did; the app once ceiled and wrote one frame more
/// than the CLI for any duration that was not a whole number of periods.
pub fn frame_count(start: f64, end: f64, fps: f64) -> usize {
    (((end - start) * fps).round() as usize).max(1)
}

/// What an export writes: the range, the rate, the count, the size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExportPlan {
    pub start: f64,
    pub end: f64,
    pub fps: f64,
    pub count: usize,
    /// The frame's size in pixels: [`export_size`].
    pub width: u32,
    pub height: u32,
}

/// The size a video export writes: the project's `videoExportWidth` ×
/// `videoExportHeight` when both are stated (16 or more), the canvas
/// otherwise — the composition fitted inside with black bars. The apps
/// always honoured it; headless renders wrote the canvas size whatever
/// the project said (review 2026-09-27, P2-31).
pub fn export_size(meta: &ProjectMetadata) -> (u32, u32) {
    let settings = &meta.composition_settings;
    let stated = |v: Option<f64>| v.filter(|v| v.is_finite() && *v >= 16.0);
    match (
        stated(settings.video_export_width),
        stated(settings.video_export_height),
    ) {
        (Some(w), Some(h)) => (w.round() as u32, h.round() as u32),
        _ => (
            settings.canvas_width.max(1.0).round() as u32,
            settings.canvas_height.max(1.0).round() as u32,
        ),
    }
}

impl ExportPlan {
    /// The composition time of frame `index` — the instant the renderer
    /// samples and the timestamp the file carries.
    pub fn frame_time(&self, index: usize) -> f64 {
        self.start + index as f64 / self.fps
    }
}

/// The plan for `meta`: `from`/`to` bound the range (default the whole
/// composition), `fps_override` beats the project's rate.
pub fn export_plan(
    meta: &ProjectMetadata,
    fps_override: Option<f64>,
    from: Option<f64>,
    to: Option<f64>,
) -> ExportPlan {
    let start = from.unwrap_or(0.0).max(0.0);
    let end = to
        .filter(|t| t.is_finite())
        .unwrap_or_else(|| composition_duration(meta))
        .max(start);
    let fps = export_fps(meta.composition_settings.fps, fps_override);
    let (width, height) = export_size(meta);
    ExportPlan {
        start,
        end,
        fps,
        count: frame_count(start, end, fps),
        width,
        height,
    }
}

/// The moments a LOOK renders — `promo frames`, and `promo_render_frames`
/// on both servers, which used to pick them three ways (review 2026-09-27,
/// P2-32): explicit `times` in time order; else `sample` moments spread
/// over the plan's range, both ends included (one is the middle); else
/// every frame of the plan. A moment outside the composition is refused
/// rather than rendered as an empty frame, and so is more than `cap` of
/// them — the message names the way out.
pub fn look_times(
    times: &[f64],
    sample: Option<usize>,
    plan: &ExportPlan,
    duration: f64,
    cap: Option<usize>,
) -> Result<Vec<f64>, String> {
    const SLACK: f64 = 1e-3;
    let picked: Vec<f64> = if !times.is_empty() {
        if let Some(bad) = times
            .iter()
            .find(|t| !t.is_finite() || **t < 0.0 || **t > duration + SLACK)
        {
            return Err(format!(
                "time {bad:.3} is outside the composition (0…{duration:.3})"
            ));
        }
        let mut times = times.to_vec();
        times.sort_by(f64::total_cmp);
        times
    } else {
        if plan.end > duration + SLACK {
            return Err(format!(
                "the range runs to {:.3}, past the composition's end ({duration:.3})",
                plan.end
            ));
        }
        match sample {
            Some(0) => {
                return Err("a sample of 0 moments renders nothing — ask for 1 or more".into())
            }
            Some(1) => vec![(plan.start + plan.end) / 2.0],
            Some(n) => {
                let span = (plan.end - plan.start).max(0.0);
                (0..n)
                    .map(|i| plan.start + span * i as f64 / (n - 1) as f64)
                    .collect()
            }
            None => (0..plan.count).map(|i| plan.frame_time(i)).collect(),
        }
    };
    if let Some(cap) = cap {
        if picked.len() > cap {
            return Err(format!(
                "{} frames is more than one call should render — ask for a shorter \
                 range, fewer times, or a sample of {cap} or fewer",
                picked.len()
            ));
        }
    }
    Ok(picked)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(fps: Option<f64>, layers: &[(f64, Option<f64>)]) -> ProjectMetadata {
        let layers: Vec<String> = layers
            .iter()
            .enumerate()
            .map(|(i, (start, duration))| {
                let duration = duration.map(|d| format!(r#","duration":{d}"#)).unwrap_or_default();
                format!(
                    r#"{{"id":"L{i}","name":"l","sortIndex":{i},"kind":"caption","isEnabled":true,"startTime":{start}{duration},"captionText":"x","keyframes":[]}}"#
                )
            })
            .collect();
        let fps = fps.map(|f| format!(r#","fps":{f}"#)).unwrap_or_default();
        let raw = format!(
            r#"{{"id":"P","name":"p","createdAt":0,"state":"recorded","trimStart":0,"trimEnd":0,
                "videoDuration":0,"subtitles":[],
                "compositionSettings":{{"canvasWidth":320,"canvasHeight":180,"backgroundColorHex":"000000"{fps}}},
                "resources":[],"layers":[{}]}}"#,
            layers.join(",")
        );
        ProjectMetadata::from_json(&raw).expect("decode")
    }

    /// The project's export size when both sides are stated, the canvas
    /// otherwise — the apps' rule (`videoExportSize`).
    #[test]
    fn the_size_is_the_projects_export_size_else_the_canvas() {
        let mut m = meta(None, &[]);
        assert_eq!(export_size(&m), (320, 180));
        m.composition_settings.video_export_width = Some(1080.0);
        assert_eq!(export_size(&m), (320, 180), "one side alone is not a size");
        m.composition_settings.video_export_height = Some(1920.0);
        assert_eq!(export_size(&m), (1080, 1920));
        assert_eq!(
            (
                export_plan(&m, None, None, None).width,
                export_plan(&m, None, None, None).height
            ),
            (1080, 1920)
        );
        m.composition_settings.video_export_height = Some(8.0);
        assert_eq!(export_size(&m), (320, 180), "under 16 is not a size");
    }

    #[test]
    fn the_rate_is_override_then_project_then_thirty_clamped_to_the_band() {
        assert_eq!(export_fps(None, None), 30.0);
        assert_eq!(export_fps(Some(60.0), None), 60.0);
        assert_eq!(export_fps(Some(60.0), Some(24.0)), 24.0);
        assert_eq!(export_fps(Some(300.0), None), 240.0, "the app's ceiling");
        assert_eq!(
            export_fps(Some(0.0), Some(-1.0)),
            30.0,
            "zero and negative are 'not given'"
        );
        assert_eq!(export_fps(Some(f64::NAN), None), 30.0);
        assert_eq!(export_fps(None, Some(0.5)), 1.0);
    }

    #[test]
    fn the_count_rounds_and_never_renders_nothing() {
        assert_eq!(
            frame_count(0.0, 1.01, 30.0),
            30,
            "the app once ceiled to 31"
        );
        assert_eq!(frame_count(0.0, 1.02, 30.0), 31);
        assert_eq!(frame_count(0.0, 0.0, 30.0), 1);
        assert_eq!(frame_count(2.0, 4.0, 59.94), 120);
    }

    #[test]
    fn the_end_is_the_furthest_declared_layer_or_the_recording() {
        let m = meta(None, &[(0.0, Some(3.0)), (2.5, Some(4.0)), (9.0, None)]);
        assert_eq!(
            composition_duration(&m),
            6.5,
            "a layer with no duration ends where it starts"
        );
        let plan = export_plan(&m, None, None, None);
        assert_eq!(
            plan,
            ExportPlan {
                start: 0.0,
                end: 6.5,
                fps: 30.0,
                count: 195,
                width: 320,
                height: 180,
            }
        );
        assert!((plan.frame_time(30) - 1.0).abs() < 1e-12);
        let bounded = export_plan(&m, Some(24.0), Some(1.0), Some(2.5));
        assert_eq!(
            bounded,
            ExportPlan {
                start: 1.0,
                end: 2.5,
                fps: 24.0,
                count: 36,
                width: 320,
                height: 180,
            }
        );
        let inverted = export_plan(&m, None, Some(5.0), Some(1.0));
        assert_eq!(
            (inverted.start, inverted.end, inverted.count),
            (5.0, 5.0, 1)
        );
    }

    /// One rule for the moments a look renders: times in time order, a
    /// sample spread over the range with both ends, every frame otherwise;
    /// outside the composition or over the cap is refused, naming why.
    #[test]
    fn a_look_picks_its_moments_one_way() {
        let plan = |from: Option<f64>, to: Option<f64>, fps: f64| ExportPlan {
            start: from.unwrap_or(0.0),
            end: to.unwrap_or(10.0),
            fps,
            count: frame_count(from.unwrap_or(0.0), to.unwrap_or(10.0), fps),
            width: 64,
            height: 64,
        };
        let whole = plan(None, None, 2.0);
        assert_eq!(
            look_times(&[4.0, 0.5], None, &whole, 10.0, Some(240)).unwrap(),
            vec![0.5, 4.0]
        );
        let twelve = look_times(&[], Some(12), &whole, 10.0, Some(240)).unwrap();
        assert_eq!(twelve.len(), 12);
        assert_eq!((twelve[0], twelve[11]), (0.0, 10.0), "both ends");
        assert_eq!(
            look_times(&[], Some(1), &whole, 10.0, None).unwrap(),
            vec![5.0]
        );
        assert_eq!(
            look_times(&[], None, &plan(Some(2.0), Some(3.0), 4.0), 10.0, None).unwrap(),
            vec![2.0, 2.25, 2.5, 2.75]
        );
        let outside = look_times(&[12.0], None, &whole, 10.0, None).unwrap_err();
        assert!(outside.contains("outside the composition"), "{outside}");
        let past = look_times(&[], Some(3), &plan(None, Some(20.0), 2.0), 10.0, None).unwrap_err();
        assert!(past.contains("past the composition's end"), "{past}");
        let over = look_times(&[], None, &plan(None, None, 30.0), 10.0, Some(240)).unwrap_err();
        assert!(over.starts_with("300 frames"), "{over}");
        assert!(look_times(&[], Some(0), &whole, 10.0, None).is_err());
    }
}
