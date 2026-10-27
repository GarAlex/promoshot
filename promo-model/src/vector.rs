//! The `svg` block on a `vector` resource (core-private docs/SVG-PLAN.md):
//! an SVG file kept exactly as it arrived in `Resources/`, drawn by the core
//! (`promo-svg`, usvg + resvg) at the size it shows on screen. The block
//! rides in the resource's bag beside `filename`, the way `midi` does
//! ([`crate::unread::APP_RESOURCE_KEYS`] names it), so the apps own it and
//! every writer keeps it.
//!
//! ```json
//! "svg": { "size": [240, 80], "outlines": "logo.svg",
//!          "sourceHash": "<sha256 of Resources/logo.svg>",
//!          "missingFonts": ["Brand Sans"] }
//! ```
//!
//! - `size`: the file's own size in CSS pixels (its `width`/`height`, or
//!   its `viewBox` when those are absent) — what a layer's fit rules use as
//!   the picture's natural size. The import also stamps `pixelWidth` and
//!   `pixelHeight` with it, so a reader that knows only pictures lays it
//!   out the same.
//! - `outlines`: present only when the file has `<text>`. The name of the
//!   derived file in `Resources/.svg/` — the file as usvg parsed it with
//!   the fonts the importing machine had, text turned into outline paths
//!   and nothing else changed — so drawing never needs a font. The render
//!   reads it in place of the original while `sourceHash` still names the
//!   original's bytes; otherwise it draws the original with no fonts (text
//!   then draws nothing, and validate says so).
//! - `missingFonts`: the families the import could not find (their text is
//!   in a fallback face). Validate names them.
//! - `overrides` (slice 2) and any other key: kept as written, not read.
//!
//! [`VectorBlock`] is the typed reading of the bag value; writers edit the
//! value itself.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The bag key the block lives under.
pub const KEY: &str = "svg";

/// The hidden folder in `Resources/` that keeps the derived outline files.
/// Like `.midi/` and `.beats/` it is not a resource: the inventory lists
/// files only.
pub const OUTLINES_DIR: &str = ".svg";

/// The reader that decodes the `vector` kind.
pub const VECTOR_READER: i64 = 51;

/// The `svg` block.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorBlock {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outlines: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing_fonts: Option<Vec<String>>,
    /// Everything else — `overrides` (slice 2) among it — as written.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl VectorBlock {
    /// The natural size, when the block states a usable one.
    pub fn natural_size(&self) -> Option<(f64, f64)> {
        let [w, h] = self.size?;
        (w > 0.0 && h > 0.0 && w.is_finite() && h.is_finite()).then_some((w, h))
    }
}

/// The block a resource carries, when it carries one that decodes.
pub fn block(resource: &crate::ProjectResource) -> Option<VectorBlock> {
    serde_json::from_value(resource.extra.get(KEY)?.clone()).ok()
}

/// Whether a project holds a vector resource — what stamps rung 51.
pub fn carries_vector(meta: &crate::ProjectMetadata) -> bool {
    meta.resources
        .iter()
        .flatten()
        .any(|r| r.kind == crate::ProjectResourceKind::Vector)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(resources: &str) -> crate::ProjectMetadata {
        crate::ProjectMetadata::from_json(&format!(
            r#"{{"id":"P","name":"P","createdAt":0,"state":"recorded","trimStart":0,
                "trimEnd":2,"videoDuration":2,"subtitles":[],"compositionSettings":{{}},
                "resources":[{resources}]}}"#
        ))
        .unwrap()
    }

    /// A project is stamped 51 exactly when it holds a vector resource.
    #[test]
    fn a_vector_resource_stamps_51() {
        assert_eq!(VECTOR_READER, crate::READER_VERSION);
        let plain = project(
            r#"{"id":"I","kind":"image","filename":"a.png","displayName":"A","addedAt":0,
                "imageCuts":[],"disabledAudioTrackIndices":[]}"#,
        );
        assert!(plain.minimum_reader_version() < VECTOR_READER);
        let vector = project(
            r#"{"id":"V","kind":"vector","filename":"logo.svg","displayName":"Logo","addedAt":0,
                "imageCuts":[],"disabledAudioTrackIndices":[],
                "svg":{"size":[240,80],"overrides":[{"node":"badge","opacity":0.5}]}}"#,
        );
        assert_eq!(vector.minimum_reader_version(), VECTOR_READER);
        let resource = &vector.resources.as_ref().unwrap()[0];
        let read = block(resource).unwrap();
        assert_eq!(read.natural_size(), Some((240.0, 80.0)));
        // `overrides` is slice 2's: kept, not read.
        assert!(read.extra.contains_key("overrides"));
        // And the block round-trips through the model untouched.
        let json: Value = serde_json::from_str(&vector.to_json().unwrap()).unwrap();
        assert_eq!(
            json.pointer("/resources/0/svg/overrides/0/node"),
            Some(&Value::from("badge"))
        );
    }

    /// An `.svg` in `Resources/` is adopted as a vector.
    #[test]
    fn the_inventory_adopts_an_svg_as_a_vector() {
        let meta = project("");
        let listed = crate::effective_resources(&meta, &["logo.svg".to_string()]);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].resource.kind, crate::ProjectResourceKind::Vector);
    }
}
