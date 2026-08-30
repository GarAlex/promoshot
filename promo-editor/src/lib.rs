//! promo-editor: the editor layer shared by every front end.
//!
//! App state and commands — no rendering, no I/O, no platform crates — so the
//! Mac app (through `promo-ffi`), an egui app and a Windows app all drive the
//! SAME behaviour rather than each reimplementing it. See
//! `../docs/EDITOR-PLAN.md`; this is Stage 1, whose slices move only derived and
//! ephemeral state and leave document ownership where it is.

pub mod author;
pub mod document;
pub mod selection;
pub mod timeline;
pub mod transport;

pub use document::{Command, Document};

pub use selection::{RevealAnchor, Selection};
pub use timeline::{
    lane_count, pack, row_id_containing, visible_layers, TimelineLane, TimelineLanePolicy,
    TimelineViewport,
};
pub use transport::{Effect, Transport, TransportState};
