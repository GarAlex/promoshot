//! promo-editor: the editor layer shared by every front end.
//!
//! App state and commands — no rendering, no I/O, no platform crates — so the
//! Mac app (through `promo-ffi`), an egui app and a Windows app all drive the
//! SAME behaviour rather than each reimplementing it. See
//! `../EDITOR-PLAN.md`; this is Stage 1, whose slices move only derived and
//! ephemeral state and leave document ownership where it is.

pub mod timeline;

pub use timeline::{
    lane_count, pack, row_id_containing, visible_layers, TimelineLane, TimelineLanePolicy,
    TimelineViewport,
};
