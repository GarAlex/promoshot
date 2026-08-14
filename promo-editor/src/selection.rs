//! Selection, pinning and reveal — Stage 1 slice 1.2.
//!
//! Four rules that today live as three pieces of SwiftUI `@State`
//! (`selectedLayerID`, `pendingScrollTarget`, and `isPinnedOutsideWindow`)
//! plus `ensureSelectedLayer`. They are small, but they are *rules*, and a
//! second front end reimplementing them would get them subtly different — the
//! pinning rule in particular exists because a selected layer that scrolls out
//! of the playhead window is selected, editable by the inspector, and
//! invisible.

use crate::timeline::{TimelineLane, TimelineViewport};
use promo_model::ProjectLayer;

/// Where the view should scroll to reveal a layer.
///
/// The two timeline projections anchor differently: the lane view scrolls to a
/// *row* (many layers share one), the classic list to the layer's own row. The
/// caller says which projection it is showing; the answer names what to scroll
/// to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevealAnchor {
    /// Lane view: the row id (`"image#0"`).
    Row(String),
    /// Classic list: the layer id.
    Layer(String),
}

/// What the timeline has selected, and what it still owes the user a scroll to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    selected: Option<String>,
    pending_reveal: Option<String>,
}

impl Selection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    pub fn is_selected(&self, layer_id: &str) -> bool {
        self.selected.as_deref() == Some(layer_id)
    }

    /// Selects an existing layer. No reveal: the user pointed at it, so it is
    /// already on screen.
    pub fn select(&mut self, layer_id: &str) {
        self.selected = Some(layer_id.to_string());
    }

    pub fn clear(&mut self) {
        self.selected = None;
        self.pending_reveal = None;
    }

    /// Selects a layer that was just added, and queues a scroll to it — a new
    /// layer is appended wherever its kind and time put it, which is often off
    /// screen.
    pub fn select_new(&mut self, layer_id: &str) {
        self.selected = Some(layer_id.to_string());
        self.pending_reveal = Some(layer_id.to_string());
    }

    /// Ask for a scroll to `layer_id` without changing the selection.
    pub fn request_reveal(&mut self, layer_id: &str) {
        self.pending_reveal = Some(layer_id.to_string());
    }

    pub fn has_pending_reveal(&self) -> bool {
        self.pending_reveal.is_some()
    }

    /// Ids that must survive viewport filtering — feed straight to
    /// [`crate::visible_layers`] / [`crate::pack`].
    ///
    /// This is the rule that keeps a selected layer reachable when the
    /// playhead moves away from it.
    pub fn pinned_ids(&self) -> Vec<String> {
        self.selected.iter().cloned().collect()
    }

    /// True when this layer is only on screen because it is pinned — the view
    /// marks it, so "why is this here?" has an answer.
    ///
    /// `window_active` is false when the timeline is fitted: with no window
    /// there is no filtering, so nothing can be pinned *outside* it.
    pub fn is_pinned_outside_window(
        &self,
        layer: &ProjectLayer,
        viewport: &TimelineViewport,
        window_active: bool,
    ) -> bool {
        window_active && self.is_selected(&layer.id) && !viewport.intersects(layer)
    }

    /// Consumes the pending reveal and says what to scroll to.
    ///
    /// `lanes` given = the lane view is showing, so the anchor is the row
    /// holding the layer; `None` = the classic list, where the layer is its
    /// own row. Returns `None` — and still consumes the request — when focus
    /// has dropped the layer, because there is nothing to scroll to and a
    /// request that can never be served must not be retried forever.
    pub fn take_reveal(&mut self, lanes: Option<&[TimelineLane]>) -> Option<RevealAnchor> {
        let target = self.pending_reveal.take()?;
        match lanes {
            None => Some(RevealAnchor::Layer(target)),
            Some(lanes) => crate::row_id_containing(&target, lanes).map(RevealAnchor::Row),
        }
    }

    /// Keeps the selection valid after the layer list changes: an unchanged
    /// selection stays, a deleted one falls back to the first layer, and an
    /// empty project selects nothing.
    ///
    /// `ordered` is the layer list in display order.
    pub fn reconcile(&mut self, ordered: &[ProjectLayer]) {
        if ordered.is_empty() {
            self.selected = None;
            return;
        }
        if let Some(current) = &self.selected {
            if ordered.iter().any(|l| l.id == *current) {
                return;
            }
        }
        self.selected = Some(ordered[0].id.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::pack;
    use promo_model::ProjectLayerKind;

    fn layer(name: &str, sort_index: i64, start: f64, duration: Option<f64>) -> ProjectLayer {
        let json = format!(
            r#"{{"id": "{name}", "name": "{name}", "sortIndex": {sort_index},
                 "kind": "image", "isEnabled": true, "startTime": {start}
                 {duration}, "keyframes": []}}"#,
            duration = duration
                .map(|d| format!(", \"duration\": {d}"))
                .unwrap_or_default(),
        );
        serde_json::from_str(&json).expect("layer fixture")
    }

    #[test]
    fn selecting_an_existing_layer_does_not_scroll() {
        let mut sel = Selection::new();
        sel.select("a");
        assert_eq!(sel.selected(), Some("a"));
        assert!(
            !sel.has_pending_reveal(),
            "the user pointed at it — it is already on screen"
        );
    }

    #[test]
    fn a_new_layer_is_selected_and_revealed() {
        let mut sel = Selection::new();
        sel.select_new("fresh");
        assert_eq!(sel.selected(), Some("fresh"));
        assert_eq!(
            sel.take_reveal(None),
            Some(RevealAnchor::Layer("fresh".into()))
        );
        assert!(!sel.has_pending_reveal(), "the request is consumed");
        assert_eq!(sel.take_reveal(None), None, "and not served twice");
    }

    #[test]
    fn reveal_resolves_to_a_row_in_the_lane_view() {
        let target = layer("target", 1, 20.0, Some(5.0));
        let overlapping = layer("overlapping", 2, 22.0, Some(5.0));
        let lanes = pack(&[target.clone(), overlapping.clone()], 60.0, 0.0, None, &[]);

        let mut sel = Selection::new();
        sel.select_new(&overlapping.id);
        assert_eq!(
            sel.take_reveal(Some(&lanes)),
            Some(RevealAnchor::Row("image#1".into())),
            "the lane view scrolls to the row, not the layer"
        );
    }

    #[test]
    fn a_reveal_focus_cannot_serve_is_dropped_not_retried() {
        let near = layer("near", 0, 100.0, Some(5.0));
        let far = layer("far", 1, 500.0, Some(5.0));
        let window = TimelineViewport {
            center: 100.0,
            span: 30.0,
            total: 600.0,
        };
        let lanes = pack(&[near, far.clone()], 600.0, 0.0, Some(&window), &[]);

        let mut sel = Selection::new();
        sel.request_reveal(&far.id);
        assert_eq!(
            sel.take_reveal(Some(&lanes)),
            None,
            "focus dropped the layer; there is no row to scroll to"
        );
        assert!(
            !sel.has_pending_reveal(),
            "the request is still consumed — otherwise it retries forever"
        );
    }

    /// The rule that keeps a selected layer reachable: it is pinned through
    /// the filter, and marked so the view can say why it is there.
    #[test]
    fn the_selection_is_pinned_through_the_filter() {
        let near = layer("near", 0, 100.0, Some(5.0));
        let far = layer("far", 1, 500.0, Some(5.0));
        let all = vec![near.clone(), far.clone()];
        let window = TimelineViewport {
            center: 100.0,
            span: 30.0,
            total: 600.0,
        };

        let mut sel = Selection::new();
        sel.select(&far.id);

        let visible = crate::visible_layers(&all, Some(&window), &sel.pinned_ids());
        assert!(
            visible.iter().any(|l| l.id == far.id),
            "the selection survives a window that excludes it"
        );
        assert!(sel.is_pinned_outside_window(&far, &window, true));
        assert!(
            !sel.is_pinned_outside_window(&near, &window, true),
            "a layer inside the window is not pinned-outside"
        );
        assert!(
            !sel.is_pinned_outside_window(&far, &window, false),
            "fitted: no window, so nothing is pinned outside it"
        );
    }

    #[test]
    fn nothing_is_pinned_when_nothing_is_selected() {
        let sel = Selection::new();
        assert!(sel.pinned_ids().is_empty());
    }

    #[test]
    fn reconcile_keeps_a_valid_selection_and_replaces_a_deleted_one() {
        let a = layer("a", 0, 0.0, Some(5.0));
        let b = layer("b", 1, 10.0, Some(5.0));
        let mut sel = Selection::new();
        sel.select(&b.id);

        sel.reconcile(&[a.clone(), b.clone()]);
        assert_eq!(sel.selected(), Some("b"), "a valid selection is untouched");

        // b deleted: fall back to the first layer rather than leaving the
        // inspector pointed at something that no longer exists.
        sel.reconcile(std::slice::from_ref(&a));
        assert_eq!(sel.selected(), Some("a"));

        sel.reconcile(&[]);
        assert_eq!(sel.selected(), None, "an empty project selects nothing");
    }

    #[test]
    fn clearing_drops_a_queued_reveal_too() {
        let mut sel = Selection::new();
        sel.select_new("x");
        sel.clear();
        assert_eq!(sel.selected(), None);
        assert!(
            !sel.has_pending_reveal(),
            "a scroll to a layer nobody selected any more is not owed"
        );
    }

    #[test]
    fn kind_does_not_affect_selection_rules() {
        // Guards against a future refactor deciding backgrounds are special
        // here; they are special to *delete*, not to selection.
        let bg: ProjectLayer = serde_json::from_str(
            r#"{"id": "bg", "name": "bg", "sortIndex": 0, "kind": "background",
                "isEnabled": true, "startTime": 0, "keyframes": []}"#,
        )
        .unwrap();
        assert_eq!(bg.kind, ProjectLayerKind::Background);
        let mut sel = Selection::new();
        sel.select(&bg.id);
        assert_eq!(sel.pinned_ids(), vec!["bg".to_string()]);
    }
}
