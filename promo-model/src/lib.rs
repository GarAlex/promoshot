//! promo-model: the project data model (RecordingProject, layers, keyframes,
//! resources, composition settings, exports) as serde types, byte-compatible
//! with the Swift app's `metadata.json`.
//!
//! P0 ships the schema-version anchor only; the full model port is Phase 1
//! (with round-trip fixtures harvested from real projects as the parity gate).

/// The `metadata.json` schema this crate targets. Bumped only when the Swift
/// app changes its persisted format (both sides decode older payloads
/// tolerantly, mirroring the Swift decoders).
pub const METADATA_SCHEMA: u32 = 1;

/// Core library version, surfaced through the FFI for the host gate test.
pub fn core_version() -> &'static str {
    concat!("promo-core ", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_string_is_stable_prefix() {
        assert!(core_version().starts_with("promo-core "));
    }
}
