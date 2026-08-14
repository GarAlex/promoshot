//! Lane packing and the timeline viewport — the first slice of the editor
//! layer, ported from `TimelineLanes.swift`.
//!
//! Lane packing is what stops a 150-layer composition being a 150-row list:
//! layers that never coexist in time share a row. Purely a view-model
//! transform — `sort_index` still owns z-order, so a lane says *when* things
//! sit side by side, not what draws on top.

use promo_model::{ProjectLayer, ProjectLayerKind};

/// One row of the timeline, holding layers that never overlap in time.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineLane {
    /// Position in the packed array. Shifts whenever an earlier kind gains or
    /// loses a lane — use [`row_id`](Self::row_id) for anything durable.
    pub id: usize,
    /// The kind every layer in this lane shares (lanes pack per kind, so
    /// video, captions and audio keep their own bands, like an NLE).
    pub kind: ProjectLayerKind,
    /// Ordered by start time; guaranteed non-overlapping.
    pub layers: Vec<ProjectLayer>,
    /// Index of this lane within its kind's band (0 = first).
    pub index_within_kind: usize,
}

impl TimelineLane {
    /// Stable identity for scrolling. Deliberately not `id`, which is the
    /// position in the packed array; kind + index-within-kind survives a
    /// repack.
    pub fn row_id(&self) -> String {
        format!("{}#{}", self.kind.as_str(), self.index_within_kind)
    }
}

/// The slice of the timeline currently on screen.
///
/// Zoom and "show me what is near the playhead" are the same control: a window
/// of `span` seconds centred on the playhead, so narrowing it both magnifies
/// (fewer seconds across the same width) and focuses (layers outside it can be
/// dropped). Row heights never change — only horizontal scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimelineViewport {
    /// Where the window is centred (normally the playhead).
    pub center: f64,
    /// How many seconds the window covers. Infinite means fit everything.
    pub span: f64,
    /// The composition length; the window is clamped inside it.
    pub total: f64,
}

impl TimelineViewport {
    pub fn fit(total: f64) -> Self {
        Self {
            center: total / 2.0,
            span: f64::INFINITY,
            total,
        }
    }

    /// A window at least as long as the composition IS "fit" — which is why a
    /// 5-minute choice must not be offered on a 20-second project.
    pub fn is_fit(&self) -> bool {
        !self.span.is_finite() || self.span >= self.total
    }

    /// Visible time range, clamped to the composition and never zero-length.
    pub fn range(&self) -> (f64, f64) {
        let total = self.total.max(0.1);
        if self.is_fit() {
            return (0.0, total);
        }
        let span = self.span.min(total).max(0.1);
        let start = (self.center - span / 2.0)
            .max(0.0)
            .min((total - span).max(0.0));
        (start, start + span)
    }

    pub fn visible_duration(&self) -> f64 {
        let (lo, hi) = self.range();
        hi - lo
    }

    /// Where `time` sits across the viewport, 0…1 (values outside are
    /// off-screen and get clamped by the caller).
    pub fn fraction(&self, time: f64) -> f64 {
        let (lo, _) = self.range();
        (time - lo) / self.visible_duration().max(0.0001)
    }

    /// Does a layer have anything inside the window?
    pub fn intersects(&self, layer: &ProjectLayer) -> bool {
        let (lo, hi) = self.range();
        let start = layer.start_time.max(0.0);
        let end = match layer.duration {
            Some(d) => start + d.max(0.0),
            None => self.total.max(start),
        };
        end >= lo && start <= hi
    }

    /// Points per second at a given pixel width — the zoom, stated plainly.
    pub fn points_per_second(&self, width: f64) -> f64 {
        width / self.visible_duration().max(0.0001)
    }
}

/// When the lane timeline is worth offering.
///
/// Lanes trade vertical rows for horizontal density, so they need horizontal
/// room: on a phone the timeline is ~270 pt after the label column, which
/// turns a dozen segments into unlabelled slivers — strictly worse than one
/// row per layer. Judged on measured width rather than platform, so an iPad in
/// split view and a narrow desktop window get the same honest answer.
pub struct TimelineLanePolicy;

impl TimelineLanePolicy {
    /// Room for a readable label plus a few segments.
    pub const MINIMUM_TIMELINE_WIDTH: f64 = 520.0;
    /// Below this the lane label collapses to its icon to buy back space.
    pub const COMPACT_LABEL_WIDTH: f64 = 720.0;

    pub fn lanes_fit(timeline_width: f64) -> bool {
        timeline_width >= Self::MINIMUM_TIMELINE_WIDTH
    }

    pub fn uses_compact_labels(timeline_width: f64) -> bool {
        timeline_width < Self::COMPACT_LABEL_WIDTH
    }
}

/// Which layers a windowed view shows.
///
/// `always_include` survives the filter wherever it sits — the selected layer
/// must never disappear because the playhead moved, which would leave it
/// active but unreachable. Both the lane view and the classic list decide
/// visibility through here, so they cannot drift.
pub fn visible_layers(
    layers: &[ProjectLayer],
    viewport: Option<&TimelineViewport>,
    always_include: &[String],
) -> Vec<ProjectLayer> {
    match viewport {
        None => layers.to_vec(),
        Some(window) => layers
            .iter()
            .filter(|l| window.intersects(l) || always_include.contains(&l.id))
            .cloned()
            .collect(),
    }
}

/// Greedy interval packing, per kind, deterministic.
///
/// `gutter` keeps neighbouring layers from visually touching: two layers share
/// a lane only when the gap between them is at least this long.
///
/// `viewport`: when given, only layers with content inside the window are
/// packed, so lanes that are empty near the playhead disappear instead of
/// taking a row. Packing still runs over the FULL layer extents, so a lane
/// never claims two layers overlap just because the window clipped them.
pub fn pack(
    layers: &[ProjectLayer],
    total_duration: f64,
    gutter: f64,
    viewport: Option<&TimelineViewport>,
    always_include: &[String],
) -> Vec<TimelineLane> {
    let layers = visible_layers(layers, viewport, always_include);

    // Kind bands keep the order the layer list already uses: ascending
    // sort_index.
    let mut by_sort = layers.clone();
    by_sort.sort_by_key(|l| l.sort_index);
    let mut order: Vec<ProjectLayerKind> = Vec::new();
    let mut grouped: Vec<(ProjectLayerKind, Vec<ProjectLayer>)> = Vec::new();
    for layer in by_sort {
        match grouped.iter_mut().find(|(k, _)| *k == layer.kind) {
            Some((_, bucket)) => bucket.push(layer),
            None => {
                order.push(layer.kind);
                grouped.push((layer.kind, vec![layer]));
            }
        }
    }

    let mut lanes: Vec<TimelineLane> = Vec::new();
    for kind in order {
        let Some((_, group)) = grouped.iter().find(|(k, _)| *k == kind) else {
            continue;
        };
        // Deterministic: earliest first, ties broken by z-order then id, so
        // input order cannot change the packing.
        let mut sorted = group.clone();
        sorted.sort_by(|a, b| {
            a.start_time
                .partial_cmp(&b.start_time)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.sort_index.cmp(&b.sort_index))
                .then(a.id.cmp(&b.id))
        });

        let mut packed: Vec<Vec<ProjectLayer>> = Vec::new();
        let mut lane_ends: Vec<f64> = Vec::new();
        for layer in sorted {
            let start = layer.start_time.max(0.0);
            let end = match layer.duration {
                Some(d) => start + d.max(0.0),
                None => total_duration.max(start),
            };
            // First lane this layer fits after; a new one otherwise.
            match lane_ends.iter().position(|e| e + gutter <= start) {
                Some(slot) => {
                    packed[slot].push(layer);
                    lane_ends[slot] = end;
                }
                None => {
                    packed.push(vec![layer]);
                    lane_ends.push(end);
                }
            }
        }

        for (index, lane_layers) in packed.into_iter().enumerate() {
            lanes.push(TimelineLane {
                id: lanes.len(),
                kind,
                layers: lane_layers,
                index_within_kind: index,
            });
        }
    }
    lanes
}

/// The row holding `layer_id`, for scrolling it into view. `None` when the
/// layer is not in the packed set (e.g. focus dropped it).
pub fn row_id_containing(layer_id: &str, lanes: &[TimelineLane]) -> Option<String> {
    lanes
        .iter()
        .find(|lane| lane.layers.iter().any(|l| l.id == layer_id))
        .map(|lane| lane.row_id())
}

/// How many rows the packed timeline needs — the compression a composition
/// gets from lanes (`layers.len()` is the un-packed cost).
pub fn lane_count(layers: &[ProjectLayer], total_duration: f64, gutter: f64) -> usize {
    pack(layers, total_duration, gutter, None, &[]).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors `TimelineLanesTests.swift` case for case. Those tests are the
    /// parity fixture for this port: if a case here disagrees with the one
    /// there, the two implementations have diverged.
    fn layer(
        name: &str,
        kind: ProjectLayerKind,
        sort_index: i64,
        start: f64,
        duration: Option<f64>,
    ) -> ProjectLayer {
        let json = format!(
            r#"{{"id": "{name}", "name": "{name}", "sortIndex": {sort_index},
                 "kind": "{kind}", "isEnabled": true, "startTime": {start}
                 {duration}, "keyframes": []}}"#,
            kind = kind.as_str(),
            duration = duration
                .map(|d| format!(", \"duration\": {d}"))
                .unwrap_or_default(),
        );
        serde_json::from_str(&json).expect("layer fixture")
    }

    fn image(name: &str, sort_index: i64, start: f64, duration: Option<f64>) -> ProjectLayer {
        layer(name, ProjectLayerKind::Image, sort_index, start, duration)
    }

    /// The invariant everything else rests on.
    fn assert_no_overlaps(lanes: &[TimelineLane], total: f64) {
        for lane in lanes {
            for pair in lane.layers.windows(2) {
                let a_end = match pair[0].duration {
                    Some(d) => pair[0].start_time + d,
                    None => total,
                };
                assert!(
                    a_end <= pair[1].start_time,
                    "lane {}: {} overlaps {}",
                    lane.id,
                    pair[0].name,
                    pair[1].name
                );
            }
        }
    }

    #[test]
    fn sequential_layers_collapse_to_one_lane() {
        let layers: Vec<_> = (0..150)
            .map(|i| image(&format!("img{i}"), i, i as f64 * 4.0, Some(4.0)))
            .collect();
        let lanes = pack(&layers, 600.0, 0.0, None, &[]);
        assert_eq!(
            lanes.len(),
            1,
            "non-overlapping layers need exactly one lane"
        );
        assert_eq!(lanes[0].layers.len(), 150);
        assert_no_overlaps(&lanes, 600.0);
    }

    #[test]
    fn overlap_depth_decides_lane_count() {
        // Starts every 2 s, each 6 s long → three coexist at any moment.
        let layers: Vec<_> = (0..150)
            .map(|i| image(&format!("img{i}"), i, i as f64 * 2.0, Some(6.0)))
            .collect();
        let lanes = pack(&layers, 400.0, 0.0, None, &[]);
        assert_eq!(lanes.len(), 3, "lane count follows overlap depth");
        assert_no_overlaps(&lanes, 400.0);
        assert_eq!(
            lanes.iter().map(|l| l.layers.len()).sum::<usize>(),
            150,
            "no layer is lost"
        );
    }

    #[test]
    fn kinds_get_their_own_bands() {
        let layers = vec![
            layer("bg", ProjectLayerKind::Background, 0, 0.0, None),
            layer("clip", ProjectLayerKind::Video, 1, 0.0, Some(10.0)),
            layer("shot", ProjectLayerKind::Image, 2, 0.0, Some(10.0)),
            layer("cap", ProjectLayerKind::Caption, 3, 0.0, Some(10.0)),
        ];
        let lanes = pack(&layers, 10.0, 0.0, None, &[]);
        assert_eq!(lanes.len(), 4, "different kinds never share a lane");
        assert_eq!(
            lanes.iter().map(|l| l.kind).collect::<Vec<_>>(),
            vec![
                ProjectLayerKind::Background,
                ProjectLayerKind::Video,
                ProjectLayerKind::Image,
                ProjectLayerKind::Caption
            ],
            "bands keep the list's top-to-bottom order"
        );
        assert!(lanes.iter().all(|l| l.index_within_kind == 0));
    }

    #[test]
    fn open_ended_layers_occupy_the_whole_timeline() {
        let layers = vec![
            layer("bg", ProjectLayerKind::Background, 0, 0.0, None),
            layer("bg2", ProjectLayerKind::Background, 1, 30.0, None),
        ];
        let lanes = pack(&layers, 120.0, 0.0, None, &[]);
        assert_eq!(lanes.len(), 2);
        assert_no_overlaps(&lanes, 120.0);
    }

    #[test]
    fn gutter_keeps_touching_layers_apart() {
        let layers = vec![image("a", 0, 0.0, Some(5.0)), image("b", 1, 5.0, Some(5.0))];
        assert_eq!(
            pack(&layers, 10.0, 0.0, None, &[]).len(),
            1,
            "abutting layers may share a lane when no gutter is asked for"
        );
        assert_eq!(
            pack(&layers, 10.0, 0.5, None, &[]).len(),
            2,
            "a gutter forces visually touching layers apart"
        );
    }

    #[test]
    fn packing_is_deterministic() {
        let layers: Vec<_> = (0..40)
            .map(|i| image(&format!("l{i}"), i, (i % 8) as f64 * 3.0, Some(5.0)))
            .collect();
        let first = pack(&layers, 60.0, 0.0, None, &[]);
        // Reversed rather than shuffled: no RNG, same point.
        let mut reordered = layers.clone();
        reordered.reverse();
        let second = pack(&reordered, 60.0, 0.0, None, &[]);
        let ids = |lanes: &[TimelineLane]| -> Vec<Vec<String>> {
            lanes
                .iter()
                .map(|l| l.layers.iter().map(|x| x.id.clone()).collect())
                .collect()
        };
        assert_eq!(
            ids(&first),
            ids(&second),
            "input order must not change the packing"
        );
    }

    #[test]
    fn viewport_windows_and_clamps() {
        let total = 600.0;

        let fit = TimelineViewport::fit(total);
        assert!(fit.is_fit());
        assert_eq!(fit.range(), (0.0, total));

        let mid = TimelineViewport {
            center: 300.0,
            span: 60.0,
            total,
        };
        assert!((mid.range().0 - 270.0).abs() < 1e-9);
        assert!((mid.range().1 - 330.0).abs() < 1e-9);
        assert!((mid.fraction(300.0) - 0.5).abs() < 1e-9);

        // Near the start it clamps instead of showing negative time.
        let head = TimelineViewport {
            center: 5.0,
            span: 60.0,
            total,
        };
        assert!((head.range().0 - 0.0).abs() < 1e-9);
        assert!((head.range().1 - 60.0).abs() < 1e-9);

        // Near the end it clamps the other way.
        let tail = TimelineViewport {
            center: 599.0,
            span: 60.0,
            total,
        };
        assert!((tail.range().1 - total).abs() < 1e-9);
        assert!((tail.range().0 - 540.0).abs() < 1e-9);

        // A window wider than the composition is just fit.
        assert!(TimelineViewport {
            center: 300.0,
            span: 9_000.0,
            total
        }
        .is_fit());

        // Zoom stated plainly: 60 s across 900 pt is 15 pt/s.
        assert!((mid.points_per_second(900.0) - 15.0).abs() < 1e-9);
        assert!((fit.points_per_second(900.0) - 1.5).abs() < 1e-9);
    }

    #[test]
    fn window_longer_than_composition_is_fit() {
        let total = 20.0;
        for span in [30.0, 60.0, 300.0] {
            let window = TimelineViewport {
                center: 10.0,
                span,
                total,
            };
            assert!(
                window.is_fit(),
                "a {span}s window over a {total}s composition can only be Fit"
            );
            assert_eq!(window.range(), (0.0, total));
        }

        let real = TimelineViewport {
            center: 10.0,
            span: 10.0,
            total,
        };
        assert!(!real.is_fit());
        assert!((real.range().0 - 5.0).abs() < 1e-9);
        assert!((real.range().1 - 15.0).abs() < 1e-9);
        assert!(
            (real.points_per_second(900.0) - 90.0).abs() < 1e-9,
            "10 s across 900 pt is 90 pt/s — against 45 for the fitted 20 s"
        );
    }

    #[test]
    fn focus_keeps_only_layers_in_the_window() {
        let layers = vec![
            image("early", 0, 0.0, Some(10.0)),
            image("here", 1, 95.0, Some(10.0)),
            image("straddling", 2, 80.0, Some(40.0)),
            image("late", 3, 500.0, Some(10.0)),
            layer("forever", ProjectLayerKind::Background, 4, 0.0, None),
        ];
        let window = TimelineViewport {
            center: 100.0,
            span: 30.0,
            total: 600.0,
        };
        let lanes = pack(&layers, 600.0, 0.0, Some(&window), &[]);
        let mut kept: Vec<String> = lanes
            .iter()
            .flat_map(|l| l.layers.iter().map(|x| x.name.clone()))
            .collect();
        kept.sort();
        assert_eq!(
            kept,
            vec![
                "forever".to_string(),
                "here".to_string(),
                "straddling".to_string()
            ],
            "layers outside the window drop; straddling and open-ended stay"
        );

        let all = pack(&layers, 600.0, 0.0, None, &[]);
        assert_eq!(
            all.iter().map(|l| l.layers.len()).sum::<usize>(),
            layers.len()
        );
    }

    #[test]
    fn row_identity_survives_repacking() {
        let early = layer("early", ProjectLayerKind::Video, 0, 0.0, Some(5.0));
        let target = image("target", 1, 20.0, Some(5.0));
        let overlapping = image("overlapping", 2, 22.0, Some(5.0));

        let before = pack(&[early.clone(), target.clone()], 60.0, 0.0, None, &[]);
        let row_before = row_id_containing(&target.id, &before);
        assert_eq!(row_before.as_deref(), Some("image#0"));

        // Adding a video lane shifts array positions but not the row identity.
        let extra_video = layer("v2", ProjectLayerKind::Video, 3, 2.0, Some(5.0));
        let after = pack(
            &[early.clone(), extra_video, target.clone()],
            60.0,
            0.0,
            None,
            &[],
        );
        assert_eq!(
            row_id_containing(&target.id, &after),
            row_before,
            "a new video lane must not change where the image layer is revealed"
        );
        let pos = |lanes: &[TimelineLane]| -> Option<usize> {
            lanes
                .iter()
                .find(|l| l.layers.iter().any(|x| x.id == target.id))
                .map(|l| l.id)
        };
        assert_ne!(
            pos(&after),
            pos(&before),
            "array position DID shift — which is why row_id exists"
        );

        // A layer pushed into a second lane of its kind reports that lane.
        let packed = pack(&[target, overlapping.clone()], 60.0, 0.0, None, &[]);
        assert_eq!(
            row_id_containing(&overlapping.id, &packed).as_deref(),
            Some("image#1")
        );
    }

    #[test]
    fn row_identity_is_none_when_focus_hides_the_layer() {
        let near = image("near", 0, 100.0, Some(5.0));
        let far = image("far", 1, 500.0, Some(5.0));
        let window = TimelineViewport {
            center: 100.0,
            span: 30.0,
            total: 600.0,
        };
        let lanes = pack(&[near.clone(), far.clone()], 600.0, 0.0, Some(&window), &[]);
        assert!(row_id_containing(&near.id, &lanes).is_some());
        assert!(
            row_id_containing(&far.id, &lanes).is_none(),
            "a layer outside the focus window has no row to scroll to"
        );
    }

    #[test]
    fn filtering_never_hides_the_selection() {
        let near = image("near", 0, 100.0, Some(5.0));
        let far = image("far", 1, 500.0, Some(5.0));
        let other = image("other", 2, 520.0, Some(5.0));
        let all = vec![near, far.clone(), other];
        let window = TimelineViewport {
            center: 100.0,
            span: 30.0,
            total: 600.0,
        };

        let unpinned = visible_layers(&all, Some(&window), &[]);
        assert_eq!(
            unpinned.iter().map(|l| l.name.clone()).collect::<Vec<_>>(),
            vec!["near".to_string()]
        );

        let pinned = visible_layers(&all, Some(&window), std::slice::from_ref(&far.id));
        assert_eq!(
            pinned.iter().map(|l| l.name.clone()).collect::<Vec<_>>(),
            vec!["near".to_string(), "far".to_string()],
            "the selected layer survives the filter; the others do not"
        );

        let lanes = pack(
            &all,
            600.0,
            0.0,
            Some(&window),
            std::slice::from_ref(&far.id),
        );
        assert!(
            lanes
                .iter()
                .any(|l| l.layers.iter().any(|x| x.id == far.id)),
            "the selection keeps its lane too"
        );

        // No window: pinning changes nothing.
        assert_eq!(
            visible_layers(&all, None, std::slice::from_ref(&far.id)).len(),
            all.len()
        );
    }

    #[test]
    fn lanes_only_offered_where_they_fit() {
        assert!(!TimelineLanePolicy::lanes_fit(390.0));
        assert!(!TimelineLanePolicy::lanes_fit(270.0));
        assert!(TimelineLanePolicy::lanes_fit(700.0));
        assert!(TimelineLanePolicy::lanes_fit(900.0));

        assert!(TimelineLanePolicy::uses_compact_labels(600.0));
        assert!(!TimelineLanePolicy::uses_compact_labels(900.0));
    }

    #[test]
    fn reports_compression_for_a_realistic_composition() {
        let mut layers = vec![layer("bg", ProjectLayerKind::Background, 0, 0.0, None)];
        // 280 layers over 3 hours, the shape of the composition soak fixture.
        for i in 0..280i64 {
            let start = i as f64 * 38.0 + (i % 7) as f64 * 11.0;
            let duration = 60.0 + (i % 5) as f64 * 60.0;
            let kind = [
                ProjectLayerKind::Image,
                ProjectLayerKind::Video,
                ProjectLayerKind::Drawing,
                ProjectLayerKind::Caption,
            ][(i % 4) as usize];
            layers.push(layer(&format!("l{i}"), kind, i + 1, start, Some(duration)));
        }
        let total = 10_800.0;
        let lanes = pack(&layers, total, 0.0, None, &[]);
        assert_no_overlaps(&lanes, total);
        println!(
            "TIMELINE LANES: {} layers → {} lanes ({:.1}× fewer rows)",
            layers.len(),
            lanes.len(),
            layers.len() as f64 / lanes.len() as f64
        );
        assert!(
            lanes.len() < 20,
            "a 3-hour composition must not need one row per layer"
        );
    }
}
