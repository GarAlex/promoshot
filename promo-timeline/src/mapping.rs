//! Trim / pause / loop mapping — the source-media ↔ output-timeline math.
//! Each function mirrors its Swift twin in `RecordingProject.swift`
//! (`VideoTimelineMapping` + the `ProjectResource` trim extension) with the
//! same epsilons, clamps and walk order.

use crate::{loop_fold, LoopFold};
use promo_model::{ProjectResource, ProjectResourceKind, VideoTrimRange};

/// Swift `VideoExtendedPause`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExtendedPause {
    /// Layer-local output time at which the held frame begins.
    pub start_time: f64,
    pub duration: f64,
}

impl ExtendedPause {
    pub fn end_time(&self) -> f64 {
        self.start_time + self.duration
    }
}

/// Swift `VideoPlaybackInterval`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackInterval {
    pub media_time: f64,
    pub output_start: f64,
    pub output_end: f64,
    /// `0` for a held frame, `1` for normal playback.
    pub rate: f32,
}

/// Swift `VideoTimelineMapping.interval(atLocalTime:pauses:)`.
pub fn playback_interval(local_time: f64, pauses: &[ExtendedPause]) -> PlaybackInterval {
    let local = local_time.max(0.0);
    let mut sorted: Vec<ExtendedPause> = pauses.to_vec();
    sorted.sort_by(|a, b| {
        a.start_time
            .partial_cmp(&b.start_time)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut accumulated_pause = 0.0_f64;
    let mut previous_pause_end = 0.0_f64;

    for pause in sorted.iter().filter(|p| p.duration > 0.000_1) {
        if local < pause.start_time {
            return PlaybackInterval {
                media_time: (local - accumulated_pause).max(0.0),
                output_start: previous_pause_end,
                output_end: pause.start_time,
                rate: 1.0,
            };
        }

        let media_at_pause = (pause.start_time - accumulated_pause).max(0.0);
        if local < pause.end_time() {
            return PlaybackInterval {
                media_time: media_at_pause,
                output_start: pause.start_time,
                output_end: pause.end_time(),
                rate: 0.0,
            };
        }

        accumulated_pause += pause.duration;
        previous_pause_end = pause.end_time();
    }

    PlaybackInterval {
        media_time: (local - accumulated_pause).max(0.0),
        output_start: previous_pause_end,
        output_end: f64::MAX,
        rate: 1.0,
    }
}

/// Swift `videoTrimRanges()`.
pub fn video_trim_ranges(res: &ProjectResource) -> Vec<VideoTrimRange> {
    let end = res
        .duration
        .unwrap_or(0.0)
        .max(res.trim_end.unwrap_or(0.0))
        .max(0.0);
    if end <= 0.0 {
        return Vec::new();
    }

    let mut sorted: Vec<_> = res
        .trim_keyframes
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter(|k| k.time >= 0.0 && k.time <= end)
        .collect();
    sorted.sort_by(|a, b| {
        a.time
            .partial_cmp(&b.time)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if sorted.is_empty() {
        let start = res.trim_start.unwrap_or(0.0).max(0.0);
        let stop = if res.trim_end.unwrap_or(0.0) > 0.0 {
            res.trim_end.unwrap_or(end).min(end)
        } else {
            end
        };
        return if stop > start {
            vec![VideoTrimRange { start, end: stop }]
        } else {
            Vec::new()
        };
    }

    let mut ranges = Vec::new();
    let mut included = if sorted.first().map(|k| k.time) == Some(0.0) {
        sorted.first().map(|k| k.is_included).unwrap_or(true)
    } else {
        true
    };
    let mut cursor = 0.0_f64;

    for keyframe in &sorted {
        let t = keyframe.time.clamp(0.0, end);
        if included && t > cursor {
            ranges.push(VideoTrimRange {
                start: cursor,
                end: t,
            });
        }
        included = keyframe.is_included;
        cursor = t;
    }

    if included && end > cursor {
        ranges.push(VideoTrimRange { start: cursor, end });
    }

    ranges.retain(|r| r.end > r.start);
    ranges
}

/// Swift `playbackVideoTrimRanges()` — `None` = media metadata unknown (use
/// the whole asset); `Some(vec![])` = the user excluded everything.
pub fn playback_video_trim_ranges(res: &ProjectResource) -> Option<Vec<VideoTrimRange>> {
    let ranges = video_trim_ranges(res);
    if !ranges.is_empty() {
        return Some(ranges);
    }
    let has_known_duration = res.duration.unwrap_or(0.0).max(res.trim_end.unwrap_or(0.0)) > 0.0;
    let has_explicit_trim = res.trim_keyframes.as_ref().is_some_and(|k| !k.is_empty())
        || res.trim_start.unwrap_or(0.0) > 0.0;
    if has_known_duration || has_explicit_trim {
        Some(Vec::new())
    } else {
        None
    }
}

/// Swift `effectiveTrimmedMediaDuration`.
pub fn effective_trimmed_media_duration(res: &ProjectResource) -> f64 {
    video_trim_ranges(res).iter().map(|r| r.duration()).sum()
}

/// Swift `extendedVideoPauses`.
pub fn extended_video_pauses(res: &ProjectResource) -> Vec<ExtendedPause> {
    if res.kind != ProjectResourceKind::Video {
        return Vec::new();
    }
    let mut sorted: Vec<_> = res.trim_keyframes.as_deref().unwrap_or(&[]).to_vec();
    sorted.sort_by(|a, b| {
        a.time
            .partial_cmp(&b.time)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut inserted = 0.0_f64;
    let mut result = Vec::new();
    for keyframe in &sorted {
        let Some(duration) = keyframe.extended_pause_duration else {
            continue;
        };
        if duration <= 0.000_1 {
            continue;
        }
        let Some(media_time) = unpaused_local_time_for_source_time(res, keyframe.time) else {
            continue;
        };
        result.push(ExtendedPause {
            start_time: media_time + inserted,
            duration,
        });
        inserted += duration;
    }
    result
}

/// Swift `totalExtendedPauseDuration`.
pub fn total_extended_pause_duration(res: &ProjectResource) -> f64 {
    extended_video_pauses(res).iter().map(|p| p.duration).sum()
}

/// Swift `effectiveVideoPlaybackDuration` (== one loop period).
pub fn effective_video_playback_duration(res: &ProjectResource) -> f64 {
    effective_trimmed_media_duration(res) + total_extended_pause_duration(res)
}

/// Swift `loopPeriod`.
pub fn loop_period(res: &ProjectResource) -> f64 {
    effective_video_playback_duration(res)
}

/// Swift `ProjectResource.loopFolded(_:)`.
pub fn loop_folded(res: &ProjectResource, local: f64) -> LoopFold {
    if !res.is_looped() {
        return LoopFold { local, offset: 0.0 };
    }
    loop_fold(local, loop_period(res))
}

/// Swift `isVideoTimeIncluded(_:)`.
pub fn is_video_time_included(res: &ProjectResource, time: f64) -> bool {
    video_trim_ranges(res)
        .iter()
        .any(|r| time >= r.start && time < r.end)
}

/// Swift `nextIncludedVideoTime(after:)`.
pub fn next_included_video_time(res: &ProjectResource, time: f64) -> Option<f64> {
    let mut ranges = video_trim_ranges(res);
    ranges.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for range in ranges {
        if time < range.start {
            return Some(range.start);
        }
        if time >= range.start && time < range.end {
            return Some(time);
        }
    }
    None
}

/// Swift `sourceTime(forLocalTime:)` — honors trims, pauses, and looping.
pub fn source_time_for_local(res: &ProjectResource, local: f64) -> f64 {
    let playback = playback_interval(loop_folded(res, local).local, &extended_video_pauses(res));
    source_time_for_unpaused_local(res, playback.media_time)
}

/// Swift `outputTime(forSourceTime:)`.
pub fn output_time_for_source(res: &ProjectResource, source_time: f64) -> Option<f64> {
    let media_time = unpaused_local_time_for_source_time(res, source_time)?;
    let mut pauses = extended_video_pauses(res);
    pauses.sort_by(|a, b| {
        a.start_time
            .partial_cmp(&b.start_time)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut inserted = 0.0_f64;
    for pause in pauses {
        let media_position = pause.start_time - inserted;
        if media_position >= media_time - 0.000_1 {
            break;
        }
        inserted += pause.duration;
    }
    Some(media_time + inserted)
}

fn source_time_for_unpaused_local(res: &ProjectResource, local: f64) -> f64 {
    let ranges = video_trim_ranges(res);
    if ranges.is_empty() {
        // Empty can mean "no media duration yet" or "everything excluded".
        let has_explicit = res.trim_keyframes.as_ref().is_some_and(|k| !k.is_empty())
            || res.trim_start.unwrap_or(0.0) > 0.0
            || res.trim_end.unwrap_or(0.0) > 0.0;
        if has_explicit {
            return 0.0;
        }
        return local.max(0.0);
    }
    if local <= 0.0 {
        return ranges[0].start;
    }
    let mut cursor = 0.0_f64;
    for range in &ranges {
        let next = cursor + range.duration();
        if local < next {
            return range.start + (local - cursor);
        }
        cursor = next;
    }
    ranges.last().map(|r| r.end).unwrap_or(local.max(0.0))
}

fn unpaused_local_time_for_source_time(res: &ProjectResource, source_time: f64) -> Option<f64> {
    let mut cursor = 0.0_f64;
    let ranges = video_trim_ranges(res);
    let count = ranges.len();
    for (index, range) in ranges.iter().enumerate() {
        if source_time >= range.start && source_time < range.end {
            return Some(cursor + (source_time - range.start));
        }
        if index == count - 1 && (source_time - range.end).abs() < 0.000_1 {
            return Some(cursor + range.duration());
        }
        cursor += range.duration();
    }
    None
}

/// Swift `videoSegment(forLocalTime:)` result — the continuous source segment
/// containing `local`, in unfolded (layer) time for looped resources.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoSegment {
    pub source_start: f64,
    pub local_start: f64,
    pub local_end: f64,
    pub rate: f32,
}

/// Swift `videoSegment(forLocalTime:)`.
pub fn video_segment(res: &ProjectResource, local: f64) -> VideoSegment {
    let fold = loop_folded(res, local);
    let inner = base_video_segment(res, fold.local);
    let period = loop_period(res);
    if !res.is_looped() || period <= 0.01 {
        return inner;
    }
    VideoSegment {
        source_start: inner.source_start,
        local_start: inner.local_start + fold.offset,
        local_end: inner.local_end.min(period) + fold.offset,
        rate: inner.rate,
    }
}

fn base_video_segment(res: &ProjectResource, local: f64) -> VideoSegment {
    let pauses = extended_video_pauses(res);
    let playback = playback_interval(local, &pauses);
    let current_source = source_time_for_unpaused_local(res, playback.media_time);
    if playback.rate == 0.0 {
        return VideoSegment {
            source_start: current_source,
            local_start: playback.output_start,
            local_end: playback.output_end,
            rate: 0.0,
        };
    }

    let ranges = video_trim_ranges(res);
    if ranges.is_empty() {
        return VideoSegment {
            source_start: 0.0,
            local_start: playback.output_start,
            local_end: playback.output_end,
            rate: 1.0,
        };
    }
    let clamped = playback.media_time.max(0.0);
    let mut cursor = 0.0_f64;
    let count = ranges.len();
    for (index, range) in ranges.iter().enumerate() {
        let next = cursor + range.duration();
        if clamped < next || index == count - 1 {
            let inserted_pause_time = (local - playback.media_time).max(0.0);
            return VideoSegment {
                source_start: range.start,
                local_start: playback.output_start.max(cursor + inserted_pause_time),
                local_end: playback.output_end.min(next + inserted_pause_time),
                rate: 1.0,
            };
        }
        cursor = next;
    }
    // Unreachable: the loop always returns on the final range.
    VideoSegment {
        source_start: ranges[0].start,
        local_start: playback.output_start,
        local_end: playback.output_end,
        rate: 1.0,
    }
}

/// The clamped playback rate (`speed`; `None` is 1.0) — the same 0.1…10
/// window `source_time_for_layer` applies. Swift `effectiveSpeed`.
pub fn effective_speed(res: &ProjectResource) -> f64 {
    let s = res.speed.unwrap_or(1.0);
    if !s.is_finite() {
        return 1.0;
    }
    s.clamp(0.1, 10.0)
}

// --- Timeline-clock twins -------------------------------------------------
//
// The functions above run on the resource's 1x content clock; playback speed
// is applied by `source_time_for_layer` at the layer boundary. The Swift app's
// resource extension instead applies speed directly (its callers all pass
// timeline seconds), so these wrappers expose the same scaled view for the
// FFI hot path and the Swift↔Rust parity harness. For a `None` policy they
// agree with `source_time_for_layer` by construction.

/// Swift `loopPeriod` / `effectiveVideoPlaybackDuration`: one loop's TIMELINE
/// length — content divided by speed.
pub fn timeline_loop_period(res: &ProjectResource) -> f64 {
    loop_period(res) / effective_speed(res)
}

/// Swift `sourceTime(forLocalTime:)` — trims, pauses, looping, and speed.
pub fn timeline_source_time(res: &ProjectResource, local: f64) -> f64 {
    // Policy `None` can never answer `Hide`, so the unwrap is unreachable.
    source_time_for_layer(res, local, None).unwrap_or(local)
}

/// Swift `outputTime(forSourceTime:)` — the 1x output position divided by
/// speed, so a frame 4s into the source lands at 2s of timeline at 2x.
pub fn timeline_output_time_for_source(res: &ProjectResource, source_time: f64) -> Option<f64> {
    output_time_for_source(res, source_time).map(|t| t / effective_speed(res))
}

/// Swift `videoSegment(forLocalTime:)` — the continuous segment in timeline
/// seconds, its `rate` multiplied by the speed (what an `AVPlayer` slaved to
/// the timeline clock must run at).
pub fn timeline_video_segment(res: &ProjectResource, local: f64) -> VideoSegment {
    let speed = effective_speed(res);
    let period = timeline_loop_period(res);
    let fold = if res.is_looped() {
        loop_fold(local, period)
    } else {
        LoopFold { local, offset: 0.0 }
    };
    let inner = base_video_segment(res, fold.local * speed);
    let scaled = VideoSegment {
        source_start: inner.source_start,
        local_start: inner.local_start / speed,
        local_end: inner.local_end / speed,
        rate: if inner.rate == 0.0 {
            0.0
        } else {
            inner.rate * speed as f32
        },
    };
    if !res.is_looped() || period <= 0.01 {
        return scaled;
    }
    VideoSegment {
        source_start: scaled.source_start,
        local_start: scaled.local_start + fold.offset,
        local_end: scaled.local_end.min(period) + fold.offset,
        rate: scaled.rate,
    }
}

/// The resource as a layer playing `cut_id` sees it.
///
/// A cut shadows the resource's trim, so rather than threading a cut through
/// every mapping function this hands back a resource with those fields already
/// swapped. Layers with and without a cut then travel the SAME code — loop
/// folding, extended pauses, include/exclude ranges and all — which is the
/// only way to be sure a cut behaves exactly like the trim it replaces.
///
/// Borrowed when there is no cut, so the common case costs nothing.
pub fn resource_for_cut<'a>(
    resource: &'a ProjectResource,
    cut_id: Option<&str>,
) -> std::borrow::Cow<'a, ProjectResource> {
    let Some(cut_id) = cut_id else {
        return std::borrow::Cow::Borrowed(resource);
    };
    let Some(cut) = resource.media_cuts.iter().find(|c| c.id == cut_id) else {
        // A layer naming a cut that no longer exists plays the whole resource
        // rather than nothing. Losing a cut should not blank the layer.
        return std::borrow::Cow::Borrowed(resource);
    };
    let mut view = resource.clone();
    view.trim_start = cut.trim_start;
    view.trim_end = cut.trim_end;
    view.trim_keyframes = cut.trim_keyframes.clone();
    view.speed = cut.speed;
    std::borrow::Cow::Owned(view)
}

#[cfg(test)]
mod cut_tests {
    use super::*;
    use promo_model::{MediaCut, ProjectResourceKind};

    fn resource() -> ProjectResource {
        let mut json = serde_json::json!({
            "id": "R1", "kind": "video", "filename": "clip.mp4",
            "displayName": "Clip", "addedAt": 0, "duration": 30.0,
            "trimStart": 0.0, "trimEnd": 30.0,
            "imageCuts": [], "disabledAudioTrackIndices": []
        });
        json["mediaCuts"] = serde_json::json!([
            { "id": "C1", "name": "The formula bit", "trimStart": 12.0, "trimEnd": 18.0 }
        ]);
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn a_cut_shadows_the_resource_trim() {
        let res = resource();
        assert_eq!(res.kind, ProjectResourceKind::Video);

        let whole = resource_for_cut(&res, None);
        assert_eq!(whole.trim_start, Some(0.0));
        assert_eq!(whole.trim_end, Some(30.0));

        let cut = resource_for_cut(&res, Some("C1"));
        assert_eq!(cut.trim_start, Some(12.0));
        assert_eq!(cut.trim_end, Some(18.0));
        // Local time 0 in the cut is 12s into the source — the same mapping
        // that a resource-level trim would give.
        assert!((source_time_for_local(&cut, 0.0) - 12.0).abs() < 1e-9);
        assert!((source_time_for_local(&cut, 2.0) - 14.0).abs() < 1e-9);
    }

    #[test]
    fn a_missing_cut_plays_the_whole_resource() {
        let res = resource();
        let gone = resource_for_cut(&res, Some("deleted"));
        assert_eq!(gone.trim_start, Some(0.0));
        assert_eq!(gone.trim_end, Some(30.0));
    }

    #[test]
    fn a_cut_carries_its_own_include_exclude_ranges() {
        // The point of a cut over a plain in/out: it has the full trim model,
        // so one cut can itself skip a dull stretch in the middle.
        let mut res = resource();
        res.media_cuts.push(MediaCut {
            id: "C2".into(),
            name: "Two takes".into(),
            trim_start: Some(0.0),
            trim_end: Some(20.0),
            speed: None,
            trim_keyframes: serde_json::from_str(
                r#"[{"id":"K1","time":0,"isIncluded":true},
                    {"id":"K2","time":5,"isIncluded":false},
                    {"id":"K3","time":9,"isIncluded":true}]"#,
            )
            .unwrap(),
            extra: Default::default(),
        });
        let cut = resource_for_cut(&res, Some("C2"));
        // The excluded 5→9 stretch is skipped, so output 5s lands at 9s.
        assert!((source_time_for_local(&cut, 5.0) - 9.0).abs() < 1e-9);
    }
}

/// Where to read the source for a layer's local time, honouring what the
/// layer does once its material runs out.
///
/// `None` means "draw nothing" — the `Hide` policy, and the only case a caller
/// has to handle specially. `Hold` and `Loop` both answer with a time, so the
/// existing render path is unchanged for them.
///
/// The policy lives on the layer rather than the resource because looping is
/// not a property of a file: the same recording can loop under one layer and
/// freeze under another. A layer that names no policy falls back to the
/// resource's legacy `looped` flag, so existing projects behave exactly as
/// before.
pub fn source_time_for_layer(
    resource: &ProjectResource,
    local: f64,
    policy: Option<promo_model::BeyondEnd>,
) -> Option<f64> {
    use promo_model::BeyondEnd;
    // Speed converts between the two clocks: `local` is timeline seconds,
    // the mapping below wants source seconds. Clamped because a zero or
    // negative rate has no meaning and would divide the period to infinity.
    let speed = resource.speed.unwrap_or(1.0).clamp(0.1, 10.0);
    let content = loop_period(resource);
    // How long the material lasts ON THE TIMELINE, which is what `Loop` and
    // `Hide` are asking about.
    let period = content / speed;
    // An explicit policy OVERRIDES the resource's legacy `looped` flag — that
    // is the whole point of putting the policy on the layer. Routing Hold
    // through `source_time_for_local` would re-apply the resource-level fold
    // and quietly loop a layer that asked to freeze.
    let explicit = policy.is_some();
    let policy = policy.unwrap_or(if resource.is_looped() {
        BeyondEnd::Loop
    } else {
        BeyondEnd::Hold
    });
    match policy {
        BeyondEnd::Loop if period > 0.0 => {
            let folded = loop_fold(local, period);
            Some(source_time_for_unpaused_local(
                resource,
                playback_interval(folded.local * speed, &extended_video_pauses(resource))
                    .media_time,
            ))
        }
        BeyondEnd::Hide if period > 0.0 && local >= period => None,
        // Hold, and the degenerate cases: map straight through, which clamps
        // at the end of the trimmed range and so freezes the last frame.
        _ if explicit => Some(source_time_for_unpaused_local(
            resource,
            playback_interval(local * speed, &extended_video_pauses(resource)).media_time,
        )),
        _ => Some(source_time_for_local(resource, local * speed)),
    }
}

#[cfg(test)]
mod beyond_end_tests {
    use super::*;
    use promo_model::BeyondEnd;

    fn clip() -> ProjectResource {
        serde_json::from_value(serde_json::json!({
            "id": "R1", "kind": "video", "filename": "c.mp4",
            "displayName": "C", "addedAt": 0, "duration": 10.0,
            "trimStart": 2.0, "trimEnd": 6.0,
            "imageCuts": [], "disabledAudioTrackIndices": []
        }))
        .unwrap()
    }

    #[test]
    fn hold_overrides_a_looped_resource() {
        // The legacy `looped` flag must lose to an explicit per-layer Hold —
        // this used to fold 5.0 back to source 3.0 and loop instead of freeze.
        let mut res = clip();
        res.looped = Some(true);
        let t = source_time_for_layer(&res, 5.0, Some(BeyondEnd::Hold)).unwrap();
        assert!((t - 6.0).abs() < 1e-9, "held at the trim end, got {t}");
    }

    #[test]
    fn hold_freezes_on_the_last_frame() {
        let res = clip();
        // 4s of usable material; ask for 7s in.
        let t = source_time_for_layer(&res, 7.0, Some(BeyondEnd::Hold)).unwrap();
        assert!(
            (t - 6.0).abs() < 1e-9,
            "clamped to the end of the trim, got {t}"
        );
    }

    #[test]
    fn loop_starts_over() {
        let res = clip();
        let first = source_time_for_layer(&res, 1.0, Some(BeyondEnd::Loop)).unwrap();
        let wrapped = source_time_for_layer(&res, 5.0, Some(BeyondEnd::Loop)).unwrap();
        assert!(
            (first - wrapped).abs() < 1e-9,
            "4s period: 5s in should read the same frame as 1s in ({first} vs {wrapped})"
        );
    }

    #[test]
    fn hide_stops_drawing_rather_than_freezing() {
        let res = clip();
        assert!(source_time_for_layer(&res, 3.9, Some(BeyondEnd::Hide)).is_some());
        assert!(source_time_for_layer(&res, 4.1, Some(BeyondEnd::Hide)).is_none());
    }

    #[test]
    fn no_policy_honours_the_legacy_looped_flag() {
        // Existing projects said "looped" on the resource; they must keep
        // behaving that way when the layer says nothing.
        let mut res = clip();
        let held = source_time_for_layer(&res, 5.0, None).unwrap();
        assert!((held - 6.0).abs() < 1e-9);

        res.looped = Some(true);
        let looped = source_time_for_layer(&res, 5.0, None).unwrap();
        let first = source_time_for_layer(&res, 1.0, None).unwrap();
        assert!((looped - first).abs() < 1e-9);
    }
}

#[cfg(test)]
mod speed_mapping_tests {
    use super::*;
    use promo_model::BeyondEnd;

    fn clip(speed: Option<f64>) -> ProjectResource {
        let mut res: ProjectResource = serde_json::from_value(serde_json::json!({
            "id": "R1", "kind": "video", "filename": "c.mp4",
            "displayName": "C", "addedAt": 0, "duration": 20.0,
            "trimStart": 0.0, "trimEnd": 8.0,
            "imageCuts": [], "disabledAudioTrackIndices": []
        }))
        .unwrap();
        res.speed = speed;
        res
    }

    #[test]
    fn speed_scales_timeline_time_into_source_time() {
        // Two seconds on the timeline at 1.5x is three seconds of source.
        let fast = clip(Some(1.5));
        let t = source_time_for_layer(&fast, 2.0, Some(BeyondEnd::Hold)).unwrap();
        assert!((t - 3.0).abs() < 1e-9, "got {t}");

        let slow = clip(Some(0.5));
        let t = source_time_for_layer(&slow, 2.0, Some(BeyondEnd::Hold)).unwrap();
        assert!((t - 1.0).abs() < 1e-9, "got {t}");
    }

    #[test]
    fn the_material_runs_out_sooner_when_played_faster() {
        // 8s of source at 2x lasts 4s on the timeline, so Hide must stop
        // there rather than at 8.
        let fast = clip(Some(2.0));
        assert!(source_time_for_layer(&fast, 3.9, Some(BeyondEnd::Hide)).is_some());
        assert!(source_time_for_layer(&fast, 4.1, Some(BeyondEnd::Hide)).is_none());
    }

    #[test]
    fn a_faster_loop_wraps_sooner() {
        let fast = clip(Some(2.0));
        // Period is 4s of timeline; 4.5s in reads the same frame as 0.5s in.
        let first = source_time_for_layer(&fast, 0.5, Some(BeyondEnd::Loop)).unwrap();
        let wrapped = source_time_for_layer(&fast, 4.5, Some(BeyondEnd::Loop)).unwrap();
        assert!((first - wrapped).abs() < 1e-9, "{first} vs {wrapped}");
    }

    #[test]
    fn timeline_twins_agree_with_the_layer_mapping() {
        // The timeline-clock wrappers exist for the Swift parity surface; for
        // a `None` policy they must answer exactly what the engine's
        // layer-level mapping answers.
        let fast = clip(Some(2.0));
        for t in [0.0, 0.5, 1.7, 3.9, 4.1, 7.3] {
            let layer = source_time_for_layer(&fast, t, None).unwrap();
            assert!((timeline_source_time(&fast, t) - layer).abs() < 1e-12);
        }
        assert!((timeline_loop_period(&fast) - 4.0).abs() < 1e-12);
        // 8s of source in 4s of timeline: source 6.0 plays at t=3.0.
        assert!((timeline_output_time_for_source(&fast, 6.0).unwrap() - 3.0).abs() < 1e-12);
    }

    #[test]
    fn timeline_segment_scales_bounds_and_rate() {
        let fast = clip(Some(2.0));
        let seg = timeline_video_segment(&fast, 1.0);
        assert!((seg.source_start - 0.0).abs() < 1e-12);
        assert!((seg.local_start - 0.0).abs() < 1e-12);
        // The 8s trim range spans 4 timeline seconds.
        assert!((seg.local_end - 4.0).abs() < 1e-9, "{}", seg.local_end);
        assert!((seg.rate - 2.0).abs() < 1e-6, "{}", seg.rate);

        // At 1x the wrapper must be bit-for-bit the plain segment.
        let plain = clip(None);
        assert_eq!(timeline_video_segment(&plain, 1.0), video_segment(&plain, 1.0));
    }

    #[test]
    fn a_looped_timeline_segment_reanchors_at_the_scaled_period() {
        let mut fast = clip(Some(2.0));
        fast.looped = Some(true);
        // Period is 4 timeline seconds; 5s in sits in the second iteration.
        let seg = timeline_video_segment(&fast, 5.0);
        assert!((seg.local_start - 4.0).abs() < 1e-9, "{}", seg.local_start);
        assert!((seg.local_end - 8.0).abs() < 1e-9, "{}", seg.local_end);
        assert!((seg.rate - 2.0).abs() < 1e-6);
        // And the frame it shows matches the folded source mapping.
        let src = timeline_source_time(&fast, 5.0);
        assert!((src - 2.0).abs() < 1e-9, "{src}");
    }

    #[test]
    fn a_cut_carries_its_own_speed() {
        let mut res = clip(None);
        res.media_cuts = vec![promo_model::MediaCut {
            id: "C1".into(),
            name: "Quick".into(),
            trim_start: Some(0.0),
            trim_end: Some(8.0),
            trim_keyframes: None,
            speed: Some(2.0),
            extra: Default::default(),
        }];
        let whole = resource_for_cut(&res, None);
        assert_eq!(whole.speed, None);
        let cut = resource_for_cut(&res, Some("C1"));
        assert_eq!(cut.speed, Some(2.0));
        let t = source_time_for_layer(&cut, 1.0, Some(BeyondEnd::Hold)).unwrap();
        assert!((t - 2.0).abs() < 1e-9);
    }
}
