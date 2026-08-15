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
            trim_keyframes: serde_json::from_str(
                r#"[{"id":"K1","time":0,"isIncluded":true},
                    {"id":"K2","time":5,"isIncluded":false},
                    {"id":"K3","time":9,"isIncluded":true}]"#,
            )
            .unwrap(),
        });
        let cut = resource_for_cut(&res, Some("C2"));
        // The excluded 5→9 stretch is skipped, so output 5s lands at 9s.
        assert!((source_time_for_local(&cut, 5.0) - 9.0).abs() < 1e-9);
    }
}
