//! promo-timeline: pure timeline math — trim/pause mapping, loop folding,
//! keyframe interpolation, layout (media/drawing rects, letterbox), audio
//! gain segments. No I/O, no GPU: this crate must build on every target.
//!
//! Every public function mirrors a Swift twin (named in its doc comment) with
//! identical epsilons, clamps, walk order, and float widths — the Phase-1
//! parity harness diffs them fixture-by-fixture.

pub mod attachment;
pub mod audio;
pub mod frame;
pub mod gradient;
pub mod interpolation;
pub mod layout;
pub mod mapping;
pub mod motion;
pub mod plan;
pub mod reveal;
pub mod sprite;
pub mod transition;
pub mod validate;
pub mod viewport;

pub use attachment::{resolve_attachments, run_containing, runs, AttachmentProblem};
pub use audio::{
    amplitude_for_fraction, audio_focus_intervals, audio_inputs, duck_gate, gain_ramp_segments,
    layer_gain, level_points, merge_intervals, placed_segments, sample_automation, scaled_segments,
    AudioInput, AudioSource, GainRampSegment, PlacedSegment, ScaledSegment, VolumePoint,
    DUCK_FACTOR, DUCK_RAMP,
};
pub use frame::{bake_slab, framed_pixel_size, rotate_bgra, slab_geometry, SlabGeometry};
pub use gradient::layer_background_gradient;
pub use interpolation::{
    interpolate_color_hex, layer_adjustments, layer_background_color_hex, layer_caption_values,
    layer_has_tilt_keyframes, layer_is_visible, layer_local_time, layer_mask_placement,
    layer_opacity, layer_position_waypoints, layer_rotation, layer_shutter, layer_tilt_offset,
    layer_transform, layer_transform_along_paths, ramp_seconds, settings_background_color_hex,
    settings_interpolated_values, CaptionValues, MaskPlacement, PositionWaypoint,
    ResolvedAdjustments, Transform,
};
pub use layout::{
    clamped_zoom, drawing_rect, letterbox_transform, media_corner_radius, media_rect,
};
pub use mapping::{
    effective_speed, effective_trimmed_media_duration, effective_video_playback_duration,
    extended_video_pauses, loop_folded, loop_period, output_time_for_source, playback_interval,
    playback_video_trim_ranges, resource_for_cut, source_time_for_layer, source_time_for_local,
    timeline_loop_period, timeline_output_time_for_source, timeline_source_time,
    timeline_video_segment, video_segment, video_trim_ranges, ExtendedPause, PlaybackInterval,
    VideoSegment,
};
pub use motion::{path_document_polyline, path_polyline, point_along, point_along_range, Polyline};
pub use plan::{
    composition_duration, export_fps, export_plan, frame_count, ExportPlan, DEFAULT_FPS, MAX_FPS,
};
pub use sprite::{
    frame_at as sprite_frame_at, is_nearest, layer_resource_id, sheet_for, SpriteFrame,
};
pub use viewport::{compose_uv, layer_viewport};

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
    use promo_model::{ProjectMetadata, ProjectResource, Size, VideoTrimRange};

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
    fn caption_values_are_none_without_style_keyframes() {
        let p = fixture_project();
        let base = CaptionValues {
            font_size: 54.0,
            vertical_margin: 34.0,
            left_margin: 90.0,
        };
        let mut caption = p.layers.as_ref().unwrap()[1].clone();
        for k in &mut caption.keyframes {
            k.zoom = None;
            k.horizontal_shift = None;
            k.vertical_shift = None;
        }
        assert_eq!(
            layer_caption_values(&caption, caption.start_time + 0.5, base),
            None
        );

        // A layer that keys them eases, and zoom drives FONT SIZE — the app's
        // mapping, not the media-layer one.
        let moving = &p.layers.as_ref().unwrap()[1];
        let got = layer_caption_values(moving, 2.5 + 5.25, base).expect("keys a style");
        assert!((got.font_size - 1.6).abs() < 1e-12);
        assert!((got.vertical_margin - 75.0).abs() < 1e-12);
        assert!((got.left_margin - -40.0).abs() < 1e-12);
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

    // ---- mix graph (Swift AudioTimelineBuilderTests vectors) --------------

    fn segs(ranges: &[(f64, f64)], start: f64, limit: f64) -> Vec<(f64, f64, f64)> {
        let ranges: Vec<VideoTrimRange> = ranges
            .iter()
            .map(|&(start, end)| VideoTrimRange { start, end })
            .collect();
        placed_segments(&ranges, start, limit, &[])
            .iter()
            .map(|s| (s.source_start, s.duration, s.output_start))
            .collect()
    }

    fn pause(start_time: f64, duration: f64) -> ExtendedPause {
        ExtendedPause {
            start_time,
            duration,
        }
    }

    #[test]
    fn placed_segments_match_the_swift_vectors() {
        assert!(segs(&[], 0.0, 10.0).is_empty());
        assert!(segs(&[(0.0, 5.0)], 0.0, 0.0).is_empty());
        assert!(segs(&[(0.0, 5.0)], 0.0, -1.0).is_empty());
        assert!(segs(&[(3.0, 3.0)], 0.0, 10.0).is_empty());
        assert!(segs(&[(8.0, 3.0)], 0.0, 10.0).is_empty());
        assert_eq!(segs(&[(3.0, 8.0)], 0.0, 10.0), vec![(3.0, 5.0, 0.0)]);
        assert_eq!(segs(&[(3.0, 8.0)], 2.5, 10.0), vec![(3.0, 5.0, 2.5)]);
        assert_eq!(segs(&[(0.0, 10.0)], 0.0, 4.0), vec![(0.0, 4.0, 0.0)]);
        assert_eq!(
            segs(&[(0.0, 2.0), (5.0, 8.0)], 1.0, 100.0),
            vec![(0.0, 2.0, 1.0), (5.0, 3.0, 3.0)]
        );
        assert_eq!(
            segs(&[(0.0, 2.0), (10.0, 14.0)], 0.0, 3.5),
            vec![(0.0, 2.0, 0.0), (10.0, 1.5, 2.0)]
        );
        assert_eq!(
            segs(&[(0.0, 3.0), (10.0, 12.0)], 0.0, 3.0),
            vec![(0.0, 3.0, 0.0)]
        );
        let total: f64 = segs(&[(0.0, 4.0), (4.0, 8.0), (8.0, 12.0)], 0.0, 7.0)
            .iter()
            .map(|s| s.1)
            .sum();
        assert!((total - 7.0).abs() < 1e-9);
    }

    #[test]
    fn extended_pauses_open_silent_gaps_in_the_placement() {
        let one = |r: &[(f64, f64)], start: f64, limit: f64, pauses: &[ExtendedPause]| {
            let ranges: Vec<VideoTrimRange> = r
                .iter()
                .map(|&(start, end)| VideoTrimRange { start, end })
                .collect();
            placed_segments(&ranges, start, limit, pauses)
                .iter()
                .map(|s| (s.source_start, s.duration, s.output_start))
                .collect::<Vec<_>>()
        };
        // Output 4…6 is intentionally empty: only this video's audio pauses.
        assert_eq!(
            one(&[(0.0, 10.0)], 1.0, 12.0, &[pause(3.0, 2.0)]),
            vec![(0.0, 3.0, 1.0), (3.0, 7.0, 6.0)]
        );
        assert_eq!(
            one(&[(0.0, 2.0), (5.0, 8.0)], 0.0, 7.0, &[pause(2.0, 2.0)]),
            vec![(0.0, 2.0, 0.0), (5.0, 3.0, 4.0)]
        );
        assert_eq!(
            one(
                &[(0.0, 10.0)],
                0.0,
                13.0,
                &[pause(3.0, 2.0), pause(8.0, 1.0)]
            ),
            vec![(0.0, 3.0, 0.0), (3.0, 3.0, 5.0), (6.0, 4.0, 9.0)]
        );
    }

    #[test]
    fn scaled_segments_divide_only_the_output_placement_by_speed() {
        let scaled =
            |r: (f64, f64), start: f64, limit: f64, pauses: &[ExtendedPause], speed: f64| {
                scaled_segments(
                    &[VideoTrimRange {
                        start: r.0,
                        end: r.1,
                    }],
                    start,
                    limit,
                    pauses,
                    speed,
                )
                .iter()
                .map(|s| {
                    (
                        s.source_start,
                        s.source_duration,
                        s.output_start,
                        s.output_duration,
                    )
                })
                .collect::<Vec<_>>()
            };
        assert_eq!(
            scaled((3.0, 8.0), 2.5, 10.0, &[], 1.0),
            vec![(3.0, 5.0, 2.5, 5.0)]
        );
        assert_eq!(
            scaled((0.0, 8.0), 10.0, 100.0, &[], 2.0),
            vec![(0.0, 8.0, 10.0, 4.0)]
        );
        assert_eq!(
            scaled((0.0, 8.0), 0.0, 3.0, &[], 2.0),
            vec![(0.0, 6.0, 0.0, 3.0)]
        );
        assert_eq!(
            scaled((2.0, 4.0), 0.0, 100.0, &[], 0.5),
            vec![(2.0, 2.0, 0.0, 4.0)]
        );
        assert_eq!(
            scaled((0.0, 8.0), 1.0, 100.0, &[pause(4.0, 2.0)], 2.0),
            vec![(0.0, 4.0, 1.0, 2.0), (4.0, 4.0, 4.0, 2.0)]
        );
    }

    fn nesting_doc(
        parent_start: f64,
        parent_speed: f64,
        parent_trim: f64,
        flat: bool,
    ) -> ProjectMetadata {
        // A clip with a gain ramp inside a composition, placed as a card.
        let clip_layer = |start: f64, sort: i64| {
            serde_json::json!({"id": "N", "name": "clip", "sortIndex": sort, "kind": "video", "isEnabled": true,
                "startTime": start, "duration": 4, "resourceID": "V", "audioFocus": true,
                "keyframes": [
                    {"id": "K0", "time": 0, "gain": 1.0, "transitionDuration": 0},
                    {"id": "K1", "time": 2, "gain": 0.25, "transitionDuration": 1}]})
        };
        let clip = serde_json::json!({"id": "V", "kind": "video", "filename": "v.mp4", "displayName": "v",
            "addedAt": 0, "duration": 6, "imageCuts": [], "disabledAudioTrackIndices": [1]});
        let layers = if flat {
            serde_json::json!([clip_layer(parent_start + 0.5, 0)])
        } else {
            serde_json::json!([{"id": "P", "name": "card", "sortIndex": 0, "kind": "video", "isEnabled": true,
                "startTime": parent_start, "duration": 5, "resourceID": "C", "keyframes": []}])
        };
        let mut resources = vec![clip];
        if !flat {
            resources.push(serde_json::json!({"id": "C", "kind": "composition", "filename": "", "displayName": "card",
                "addedAt": 0, "duration": 6, "speed": parent_speed, "trimStart": parent_trim,
                "pixelWidth": 320, "pixelHeight": 180, "imageCuts": [],
                "composition": {"canvasWidth": 320, "canvasHeight": 180, "layers": [clip_layer(0.5, 0)]}}));
        }
        let json = serde_json::json!({
            "id": "P", "name": "nest", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 10, "videoDuration": 10, "subtitles": [],
            "compositionSettings": {"canvasWidth": 320, "canvasHeight": 180},
            "resources": resources, "layers": layers
        });
        ProjectMetadata::from_json(&json.to_string()).unwrap()
    }

    /// A composition placed as a clip makes the sound its layers would
    /// make flattened onto the parent's clock: same inputs, same focus,
    /// offset by the card's start.
    #[test]
    fn nested_audio_equals_the_flattened_graph() {
        let nested = nesting_doc(2.0, 1.0, 0.0, false);
        let flat = nesting_doc(2.0, 1.0, 0.0, true);
        let (a, fa) = audio_inputs(&nested, &|_| true);
        let (b, fb) = audio_inputs(&flat, &|_| true);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].start_time, b[0].start_time);
        assert_eq!(a[0].duration_cap, Some(4.0));
        assert_eq!(a[0].speed, b[0].speed);
        assert_eq!(
            a[0].volume_points, b[0].volume_points,
            "the ramp rides the card's clock"
        );
        assert_eq!(
            a[0].disabled_audio_track_indices,
            b[0].disabled_audio_track_indices
        );
        assert_eq!(a[0].included_ranges, b[0].included_ranges);
        assert_eq!(fa, fb, "focus spans move with the card");
        assert_eq!(fa, vec![(2.5, 6.5)]);
    }

    /// The card's own speed and trim reshape the nested sound: a clip
    /// starting before the trim loses that much source, everything runs
    /// at the card's rate, and the ramp's points land where the card
    /// puts them.
    #[test]
    fn nested_audio_follows_the_cards_speed_and_trim() {
        let doc = nesting_doc(2.0, 2.0, 1.0, false);
        let (inputs, focus) = audio_inputs(&doc, &|_| true);
        assert_eq!(inputs.len(), 1);
        let input = &inputs[0];
        // The clip plays 0.5…4.5 on the card's clock; the card plays 1.0…6
        // at 2x from t=2 — so the clip runs 2.0…3.75 on the parent, having
        // skipped its first half second of source.
        assert!(
            (input.start_time - 2.0).abs() < 1e-9,
            "{}",
            input.start_time
        );
        assert!(
            (input.duration_cap.unwrap() - 1.75).abs() < 1e-9,
            "{:?}",
            input.duration_cap
        );
        assert_eq!(input.speed, 2.0);
        let ranges = input.included_ranges.as_ref().unwrap();
        assert_eq!(ranges.len(), 1);
        assert!(
            (ranges[0].start - 0.5).abs() < 1e-9 && (ranges[0].end - 6.0).abs() < 1e-9,
            "{ranges:?}"
        );
        // Ramp points: the card's clock t maps to 2 + (t - 1) / 2. The ramp
        // ends at card time 0.5 + 2 = 2.5 -> parent 2.75.
        let points = input.volume_points.as_ref().unwrap();
        let last = points.last().unwrap();
        assert!((last.time - 2.75).abs() < 1e-6, "{points:?}");
        assert!((last.volume - 0.25).abs() < 1e-6);
        assert_eq!(focus.len(), 1);
        assert!(
            (focus[0].0 - 2.0).abs() < 1e-9 && (focus[0].1 - 3.75).abs() < 1e-9,
            "{focus:?}"
        );
    }

    #[test]
    fn audio_inputs_derive_the_apps_mix_graph() {
        let p = fixture_project();
        let (inputs, focus) = audio_inputs(&p, &|_| true);
        assert_eq!(inputs.len(), 2, "the looping clip and the voice");
        let clip = &inputs[0];
        assert_eq!(clip.source, AudioSource::Resource(clip_resource_id(&p)));
        assert!(clip.is_focused && !clip.single_track);
        assert_eq!(clip.start_time, 2.5);
        assert_eq!(clip.duration_cap, Some(25.0));
        assert!((clip.volume - 1.0).abs() < 1e-6);
        let voice = &inputs[1];
        assert!(voice.single_track && !voice.is_focused);
        assert!((voice.volume - 0.8).abs() < 1e-6);
        // Two gain keyframes become output-time automation offset by the
        // layer's start, in the fraction domain.
        let points = voice
            .volume_points
            .as_ref()
            .expect("keyframed gain is automation");
        assert!(
            (points[0].time - 1.0).abs() < 1e-9,
            "offset by the layer start"
        );
        assert!(points.iter().all(|p| (0.0..=1.0).contains(&p.volume)));
        assert_eq!(focus, vec![(2.5, 27.5)]);

        // A layer whose media is gone contributes nothing.
        let (without, _) = audio_inputs(&p, &|l| l.name != "Voice");
        assert_eq!(without.len(), 1);
    }

    /// A gain keyframe with no ramp is a STEP. Two breakpoints at one
    /// time read as a ramp from the previous breakpoint, which faded a cut
    /// to 25% at 6s in from 4s; the pre-step point sits a millisecond early
    /// so the level holds until the keyframe, as `layer_gain` reports.
    #[test]
    fn a_gain_step_holds_until_its_keyframe_in_the_mix_graph() {
        let raw = r#"{"id":"P","name":"s","createdAt":0,"state":"recorded",
            "trimStart":0,"trimEnd":0,"videoDuration":0,"subtitles":[],
            "compositionSettings":{"canvasWidth":320,"canvasHeight":180,"backgroundColorHex":"000000"},
            "resources":[{"id":"M","kind":"audio","filename":"m.wav","displayName":"m","addedAt":0,"duration":8,
                          "imageCuts":[],"disabledAudioTrackIndices":[]}],
            "layers":[{"id":"L","name":"m","sortIndex":0,"kind":"audio","isEnabled":true,"startTime":1,"duration":8,"resourceID":"M",
                       "keyframes":[{"id":"K0","time":0,"gain":1,"transitionDuration":0},
                                    {"id":"K1","time":6,"gain":0.25,"transitionDuration":0}]}]}"#;
        let p = ProjectMetadata::from_json(raw).expect("decode");
        let (inputs, _) = audio_inputs(&p, &|_| true);
        let points = inputs[0].volume_points.clone().expect("automation");
        let flat: Vec<(f64, f32)> = points.iter().map(|p| (p.time, p.volume)).collect();
        assert_eq!(flat, vec![(1.0, 1.0), (6.999, 1.0), (7.0, 0.25)]);
        assert!(
            (sample_automation(6.5, &points) - 1.0).abs() < 1e-6,
            "holds before the step"
        );
        assert!(
            (sample_automation(7.0, &points) - 0.25).abs() < 1e-6,
            "and cuts at it"
        );
    }

    fn clip_resource_id(p: &ProjectMetadata) -> String {
        p.layers
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .find(|l| l.name == "Looping clip")
            .and_then(|l| l.resource_id.clone())
            .expect("fixture has the looping clip")
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
