//! MIDI-sequenced sound for PromoShot (core-private docs/MIDI.md): the
//! resource's `midi` block — step lanes, notes in beats, an effect preset
//! or an imported `.mid` — expanded to timed events ([`score`]), rendered
//! by rustysynth with a SoundFont bank to 48 kHz stereo 24-bit WAV
//! ([`render`]), a loop folded to its exact length, and settled into the
//! project under its receipt ([`settle`]). One renderer for every host:
//! the headless server and CLI here, the apps over promo-ffi, so a take
//! sounds the same everywhere.
//!
//! The bank lives in the sounds folder ([`sounds`]) for headless use; the
//! apps hand their bundled copy's path to [`Bank::open`].

pub mod effects;
#[cfg(feature = "generate")]
pub mod generate;
pub mod gm;
pub mod layer;
pub mod render;
pub mod score;
pub mod settle;
pub mod smf;
pub mod sounds;
pub mod tempo;

pub use render::{Bank, Take};
pub use settle::{
    findings, import_mid, mark_context, preview, settle, settle_doc, settle_used, stale, used_midi,
    Imported, LayerInfo, Settled, TrackInfo,
};

/// The bank to render with: `path` when given, else the installed default
/// (`PROMOSHOT_SOUNDS` or the per-user sounds folder); the error says how
/// to install one.
pub fn bank(path: Option<&std::path::Path>) -> Result<std::sync::Arc<Bank>, String> {
    match path {
        Some(path) => Bank::open(path),
        None => match sounds::installed_default() {
            Some(path) => Bank::open(&path),
            None => Err(sounds::missing_bank_message()),
        },
    }
}
