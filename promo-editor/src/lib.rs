//! promo-editor: the document's edit vocabulary, shared by every front end.
//!
//! Commands with undo (`document`), the wizard's arrangement (`author`) and
//! the theme rules (`theme`) — no rendering, no I/O, no platform crates.
//! The MCP servers' `promo_apply` and `promo_slideshow` are built on this
//! crate, which is why it is public; the editor's UI brain (lanes,
//! selection, transport) lives beside the apps, in `promo-editor-ui`.

pub mod author;
pub mod document;
mod theme;

pub use document::{command_schema, Changes, Command, Document};
