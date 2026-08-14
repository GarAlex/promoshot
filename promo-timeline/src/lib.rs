//! promo-timeline: pure timeline math — trim/pause mapping, loop folding,
//! keyframe interpolation, layout (media/drawing rects, letterbox), audio
//! gain segments. No I/O, no GPU: this crate must build on every target.
//!
//! Every public function mirrors a Swift twin (named in its doc comment) with
//! identical epsilons, clamps, walk order, and float widths — the Phase-1
//! parity harness diffs them fixture-by-fixture.

pub mod audio;
pub mod interpolation;
pub mod layout;
pub mod mapping;

pub use audio::{
    amplitude_for_fraction, audio_focus_intervals, duck_gate, gain_ramp_segments, layer_gain,
    level_points, merge_intervals, sample_automation, GainRampSegment, VolumePoint,
};
pub use interpolation::{
    interpolate_color_hex, layer_background_color_hex, layer_has_tilt_keyframes, layer_is_visible,
    layer_local_time, layer_opacity, layer_rotation, layer_tilt_offset, layer_transform,
    settings_background_color_hex, settings_interpolated_values, Transform,
};
pub use layout::{
    clamped_zoom, drawing_rect, letterbox_transform, media_corner_radius, media_rect,
};
pub use mapping::{
    effective_trimmed_media_duration, effective_video_playback_duration, extended_video_pauses,
    loop_folded, loop_period, output_time_for_source, playback_interval,
    playback_video_trim_ranges, source_time_for_local, video_segment, video_trim_ranges,
    ExtendedPause, PlaybackInterval, VideoSegment,
};

/// Result of folding a layer-local time into a looped resource's window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoopFold {
    /// Time within the loop window (== input when not folded).
    pub local: f64,
    /// Unfolded start of the current iteration (0 on the first pass).
    pub offset: f64,
}

/// Folds a layer-local time into the loop window of a resource whose one-loop
/// output length is `period` seconds. Mirrors Swift
/// `ProjectResource.loopFolded(_:)`: identity when `period` is degenerate
/// (≤ 0.01 s) or the time is still inside the first iteration.
pub fn loop_fold(local: f64, period: f64) -> LoopFold {
    if period <= 0.01 || local < period {
        return LoopFold { local, offset: 0.0 };
    }
    let iterations = (local / period).floor();
    LoopFold {
        local: local - iterations * period,
        offset: iterations * period,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use promo_model::{ProjectMetadata, ProjectResource, Size};

    // Values mirror ReVoiceTests/GifExportTests.testLoopedResource_timeMappingWraps.
    #[test]
    fn folds_into_third_iteration() {
        let fold = loop_fold(25.0, 10.0);
        assert_eq!(fold.local, 5.0);
        assert_eq!(fold.offset, 20.0);
    }

    #[test]
    fn identity_inside_first_iteration() {
        let fold = loop_fold(7.5, 10.0);
        assert_eq!(fold.local, 7.5);
        assert_eq!(fold.offset, 0.0);
    }

    #[test]
    fn identity_for_degenerate_period() {
        let fold = loop_fold(42.0, 0.0);
        assert_eq!(fold.local, 42.0);
        assert_eq!(fold.offset, 0.0);
    }

    #[test]
    fn exact_boundary_starts_next_iteration() {
        let fold = loop_fold(10.0, 10.0);
        assert_eq!(fold.local, 0.0);
        assert_eq!(fold.offset, 10.0);
    }

    // ---- fixture-driven mapping tests --------------------------------------

    fn fixture_project() -> ProjectMetadata {
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../fixtures/projects/project-4.json"
        ))
        .expect("fixture");
        ProjectMetadata::from_json(&raw).expect("decode")
    }

    /// The synthetic looping video: trims keep [0,3) and [5,10) of a 12 s
    /// asset, plus a 2 s held frame at source t=5. One loop = 8 s of media
    /// + 2 s pause = 10 s.
    fn looping_video(p: &ProjectMetadata) -> ProjectResource {
        p.resources.as_ref().unwrap()[0].clone()
    }

    #[test]
    fn trim_ranges_from_keyframes() {
        let p = fixture_project();
        let res = looping_video(&p);
        let ranges = video_trim_ranges(&res);
        assert_eq!(ranges.len(), 2);
        assert_eq!((ranges[0].start, ranges[0].end), (0.0, 3.0));
        assert_eq!((ranges[1].start, ranges[1].end), (5.0, 10.0));
        assert_eq!(effective_trimmed_media_duration(&res), 8.0);
        assert_eq!(effective_video_playback_duration(&res), 10.0);
    }

    #[test]
    fn extended_pause_lands_on_output_timeline() {
        let p = fixture_project();
        let res = looping_video(&p);
        let pauses = extended_video_pauses(&res);
        assert_eq!(pauses.len(), 1);
        // Source t=5 maps to unpaused-local 3 (after the [3,5) cut).
        assert_eq!(pauses[0].start_time, 3.0);
        assert_eq!(pauses[0].duration, 2.0);
    }

    #[test]
    fn source_time_walks_trims_pauses_and_loops() {
        let p = fixture_project();
        let res = looping_video(&p);
        // Before the pause: local 2 → source 2.
        assert_eq!(source_time_for_local(&res, 2.0), 2.0);
        // Inside the held frame (local 3..5): held at source 5.
        assert_eq!(source_time_for_local(&res, 4.0), 5.0);
        // After the pause: local 6 → media 4 → source 5 + (4-3) = 6.
        assert_eq!(source_time_for_local(&res, 6.0), 6.0);
        // Second loop iteration: local 12 folds to 2 → source 2.
        assert_eq!(source_time_for_local(&res, 12.0), 2.0);
        // And the held frame inside the second iteration.
        assert_eq!(source_time_for_local(&res, 14.0), 5.0);
    }

    #[test]
    fn output_time_reinserts_pauses() {
        let p = fixture_project();
        let res = looping_video(&p);
        // Source 2 (before the cut) → output 2 (no pause before it).
        assert_eq!(output_time_for_source(&res, 2.0), Some(2.0));
        // Source 6 → unpaused local 4 → +2 s pause inserted before it → 6.
        assert_eq!(output_time_for_source(&res, 6.0), Some(6.0));
        // Excluded source time has no output position.
        assert_eq!(output_time_for_source(&res, 4.0), None);
    }

    #[test]
    fn video_segment_reanchors_at_loop_boundaries() {
        let p = fixture_project();
        let res = looping_video(&p);
        // Local 12 (second iteration, media segment [0,3)): bounds re-anchor
        // into unfolded layer time.
        let seg = video_segment(&res, 12.0);
        assert_eq!(seg.source_start, 0.0);
        assert_eq!(seg.local_start, 10.0);
        assert_eq!(seg.local_end, 13.0);
        assert_eq!(seg.rate, 1.0);
        // Held frame in the second iteration: rate 0 with unfolded bounds.
        let held = video_segment(&res, 14.0);
        assert_eq!(held.rate, 0.0);
        assert_eq!(held.local_start, 13.0);
        assert_eq!(held.local_end, 15.0);
    }

    #[test]
    fn playback_interval_matches_swift_walk() {
        let pauses = [ExtendedPause {
            start_time: 3.0,
            duration: 2.0,
        }];
        let before = playback_interval(1.0, &pauses);
        assert_eq!(
            (
                before.media_time,
                before.output_start,
                before.output_end,
                before.rate
            ),
            (1.0, 0.0, 3.0, 1.0)
        );
        let held = playback_interval(4.0, &pauses);
        assert_eq!(
            (
                held.media_time,
                held.output_start,
                held.output_end,
                held.rate
            ),
            (3.0, 3.0, 5.0, 0.0)
        );
        let after = playback_interval(7.0, &pauses);
        assert_eq!((after.media_time, after.rate), (5.0, 1.0));
        assert_eq!(after.output_start, 5.0);
    }

    // ---- interpolation ------------------------------------------------------

    #[test]
    fn layer_transform_holds_then_eases() {
        let p = fixture_project();
        let layer = &p.layers.as_ref().unwrap()[1]; // "Looping clip", start 2.5
        let settings = &p.composition_settings;
        // Keyframes at local 0 (zoom 1) and 6 (zoom 2.2, transition 1.5):
        // hold until local 4.5, ease 4.5→6.
        let hold = layer_transform(layer, 2.5 + 4.0, settings);
        assert_eq!(hold.zoom, 1.0);
        let mid = layer_transform(layer, 2.5 + 5.25, settings); // progress 0.5
        assert!((mid.zoom - 1.6).abs() < 1e-12);
        assert!((mid.vertical_shift - 75.0).abs() < 1e-12);
        assert!((mid.horizontal_shift - -40.0).abs() < 1e-12);
        // transitionDuration 0 on the third keyframe → step exactly at 12.
        let step_before = layer_transform(layer, 2.5 + 11.999, settings);
        assert!((step_before.zoom - 2.2).abs() < 1e-12);
        let at = layer_transform(layer, 2.5 + 12.0, settings);
        assert!((at.zoom - 1.4).abs() < 1e-12);
    }

    #[test]
    fn rotation_and_tilt_interpolate() {
        let p = fixture_project();
        let layer = &p.layers.as_ref().unwrap()[1];
        // Rotation keyed at local 6 (45°) and 12 (-30°, transition 0): holds
        // 45 until 12, then steps.
        assert_eq!(layer_rotation(layer, 2.5 + 6.0), 45.0);
        assert_eq!(layer_rotation(layer, 2.5 + 9.0), 45.0);
        assert_eq!(layer_rotation(layer, 2.5 + 12.0), -30.0);
        // Before the first rotation keyframe: holds the first value.
        assert_eq!(layer_rotation(layer, 2.5), 45.0);
        // Tilt keyframes exist only on the third keyframe → constant.
        assert_eq!(layer_tilt_offset(layer, 2.5 + 1.0), Some((12.0, -20.0)));
        // The image layer eases tilt 0→(18,-25) over local 3..5.
        let image = &p.layers.as_ref().unwrap()[2]; // start 4
        let (tx, ty) = layer_tilt_offset(image, 4.0 + 4.0).unwrap(); // progress 0.5
        assert!((tx - 9.0).abs() < 1e-12);
        assert!((ty - -12.5).abs() < 1e-12);
    }

    #[test]
    fn background_color_eases_between_keyframes() {
        let p = fixture_project();
        let bg = &p.layers.as_ref().unwrap()[0];
        let settings = &p.composition_settings;
        assert_eq!(layer_background_color_hex(bg, 0.0, settings), "112233");
        // Keys at 0 (112233) and 10 (445566, transition 3): ease over 7..10.
        let mid = layer_background_color_hex(bg, 8.5, settings); // progress 0.5
        assert_eq!(mid, "2B3C4D"); // componentwise midpoint, rounded
        assert_eq!(layer_background_color_hex(bg, 10.0, settings), "445566");
    }

    #[test]
    fn settings_values_match_swift_defaults_path() {
        let p = fixture_project();
        let s = &p.composition_settings;
        // Keys at 0 (1.0/0/0) and 5 (1.8/-120/60, transition 1.2).
        let hold = settings_interpolated_values(s, 3.0);
        assert_eq!(hold.zoom, 1.0);
        let end = settings_interpolated_values(s, 5.0);
        assert!((end.zoom - 1.8).abs() < 1e-12);
        assert_eq!(settings_background_color_hex(s, 8.0), "FFAA00");
    }

    #[test]
    fn interpolate_color_matches_swift_rounding() {
        assert_eq!(interpolate_color_hex("000000", "FFFFFF", 0.5), "808080");
        assert_eq!(interpolate_color_hex("#0088FF", "0088ff", 0.25), "0088FF");
        assert_eq!(interpolate_color_hex("bogus", "FFFFFF", 0.5), "bogus");
        assert_eq!(interpolate_color_hex("bogus", "FFFFFF", 1.0), "FFFFFF");
    }

    // ---- audio --------------------------------------------------------------

    #[test]
    fn gain_ramps_from_resource_baseline() {
        let p = fixture_project();
        let voice = &p.layers.as_ref().unwrap()[3];
        // Keyframes: (3, 0.4, transition 1), (9, 1.0, transition 0); implicit
        // baseline anchor (0, default) inserted because first key is at t=3.
        let default_gain = 0.8_f32;
        assert_eq!(layer_gain(voice, 0.0, default_gain), 0.8);
        assert_eq!(layer_gain(voice, 1.5, default_gain), 0.8); // hold until 2
        let mid = layer_gain(voice, 2.5, default_gain); // ease 2..3, progress 0.5
        assert!((mid - 0.6).abs() < 1e-6);
        assert_eq!(layer_gain(voice, 3.0, default_gain), 0.4);
        assert_eq!(layer_gain(voice, 8.9, default_gain), 0.4);
        assert_eq!(layer_gain(voice, 9.0, default_gain), 1.0);

        let segments = gain_ramp_segments(voice, default_gain);
        // hold 0→2 @0.8, ramp 2→3 0.8→0.4, hold 3→9 @0.4, step at 9 →1.0
        assert_eq!(segments.len(), 4);
        assert_eq!(
            (
                segments[0].start_time,
                segments[0].end_time,
                segments[0].start_value
            ),
            (0.0, 2.0, 0.8)
        );
        assert_eq!(
            (
                segments[1].start_time,
                segments[1].end_time,
                segments[1].end_value
            ),
            (2.0, 3.0, 0.4)
        );
        assert_eq!((segments[2].start_time, segments[2].end_time), (3.0, 9.0));
        assert_eq!(
            (
                segments[3].start_time,
                segments[3].end_time,
                segments[3].end_value
            ),
            (9.0, 9.0, 1.0)
        );
    }

    #[test]
    fn focus_intervals_pick_enabled_focused_layers() {
        let p = fixture_project();
        let intervals = audio_focus_intervals(&p);
        // Only the "Looping clip" layer is audio-focused: 2.5 … 27.5.
        assert_eq!(intervals, vec![(2.5, 27.5)]);
    }

    // ---- layout -------------------------------------------------------------

    #[test]
    fn media_rect_scales_to_canvas_height() {
        let r = media_rect(
            Size::new(1920.0, 1080.0),
            Size::new(1080.0, 1920.0),
            1.0,
            50.0,
            -30.0,
        );
        // scale = 1920/1080 → width 1920*(1920/1080), height 1920.
        assert!((r.height() - 1920.0).abs() < 1e-9);
        assert_eq!(r.x(), 50.0);
        assert_eq!(r.y(), -30.0);
        // Zoom floors at 0.3.
        let tiny = media_rect(
            Size::new(100.0, 100.0),
            Size::new(1000.0, 1000.0),
            0.01,
            0.0,
            0.0,
        );
        assert!((tiny.height() - 300.0).abs() < 1e-9);
    }

    #[test]
    fn drawing_rect_aspect_fits_and_centers() {
        let r = drawing_rect(
            Size::new(1080.0, 1920.0),
            Size::new(1920.0, 1080.0),
            1.0,
            0.0,
            0.0,
        );
        // fit scale = min(1920/1080, 1080/1920) = 0.5625 → 607.5 × 1080.
        assert!((r.width() - 607.5).abs() < 1e-9);
        assert!((r.height() - 1080.0).abs() < 1e-9);
        assert!((r.x() - (1920.0 - 607.5) / 2.0).abs() < 1e-9);
        assert_eq!(r.y(), 0.0);
    }

    #[test]
    fn letterbox_matches_swift() {
        // 1080×1920 canvas into 3840×2160 output: scale by height.
        let (scale, (ox, oy)) =
            letterbox_transform(Size::new(1080.0, 1920.0), Size::new(3840.0, 2160.0));
        assert!((scale - 1.125).abs() < 1e-12);
        assert!((ox - (3840.0 - 1080.0 * 1.125) / 2.0).abs() < 1e-9);
        assert_eq!(oy, 0.0);
        // Degenerate canvas: identity.
        assert_eq!(
            letterbox_transform(Size::new(0.0, 0.0), Size::new(100.0, 100.0)),
            (1.0, (0.0, 0.0))
        );
    }
}
