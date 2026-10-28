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
//! - `overrides` (slice 2): edits to the file's elements, applied to the
//!   parsed tree at render time — the file itself is never rewritten, and
//!   removing every override gives back exactly its picture
//!   ([`VectorOverride`]).
//! - `handles` (slice 2): the author's names for elements the file gives
//!   no `id` — `{"badge/rect[0]": "Badge fill"}` — minted on import, so a
//!   tool may name a node the way the file's author did.
//! - Any other key: kept as written, not read.
//!
//! Motion (slice 3) is not in the block: a layer keyframe's `nodes`
//! ([`NodeKey`]) keys an element on that layer, over the block's
//! overrides as the base.
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
    /// Edits to the file's elements, in the order they were first made.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<VectorOverride>,
    /// The author's names for elements without an `id`: address → name.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub handles: std::collections::BTreeMap<String, String>,
    /// Everything else, as written.
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

/// One element's edits (SVG-PLAN.md slice 2). `node` is the element's
/// address: its `id`, else its path from the nearest ancestor with one
/// (`badge/rect[0]`, `g[2]/path[0]` — the tag and its index among siblings
/// of that tag). Every other key is optional and absent means "as drawn".
///
/// - Any element (tier A): `visible`, `opacity` (0–1, the element's own,
///   replacing what the file gives it), `transform` (about the element's
///   own centre: `move` in the picture's CSS pixels, `scale`, `rotate` in
///   degrees) and `order` (its place among its siblings, 0 = drawn first).
/// - A painted shape (tier B): `fill` and `stroke` colours (`RRGGBB` or
///   `RRGGBBAA`), `fillStops` / `strokeStops` (one colour per stop of the
///   shape's gradient, offsets kept), `strokeWidth`.
/// - Text: `text`, its words — outlined again on the machine that edits
///   it, so drawing still needs no font.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorOverride {
    pub node: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<NodeTransform>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_stops: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke_stops: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke_width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Keys a later build adds, kept as written.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl VectorOverride {
    /// Whether it changes the picture's tree at render time (`text` is
    /// in the outlines instead).
    pub fn edits_tree(&self) -> bool {
        self.visible.is_some()
            || self.opacity.is_some()
            || self.transform.as_ref().is_some_and(|t| !t.is_identity())
            || self.order.is_some()
            || self.edits_paint()
    }

    /// Whether it changes a painted shape's paint (tier B).
    pub fn edits_paint(&self) -> bool {
        self.fill.is_some()
            || self.stroke.is_some()
            || self.fill_stops.is_some()
            || self.stroke_stops.is_some()
            || self.stroke_width.is_some()
    }

    /// Whether it says nothing at all beyond its node.
    pub fn is_empty(&self) -> bool {
        !self.edits_tree()
            && self.transform.is_none()
            && self.text.is_none()
            && self.extra.is_empty()
    }
}

/// A move, scale and turn about an element's own centre.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NodeTransform {
    /// `[dx, dy]` in the picture's CSS pixels.
    #[serde(default, rename = "move", skip_serializing_if = "Option::is_none")]
    pub offset: Option<[f64; 2]>,
    /// One factor, or `[sx, sy]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<NodeScale>,
    /// Degrees, clockwise on screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotate: Option<f64>,
}

impl NodeTransform {
    /// `(sx, sy)`, 1 when absent.
    pub fn scales(&self) -> (f64, f64) {
        match self.scale {
            Some(NodeScale::Uniform(s)) => (s, s),
            Some(NodeScale::Axes([x, y])) => (x, y),
            None => (1.0, 1.0),
        }
    }

    /// Whether it moves nothing.
    pub fn is_identity(&self) -> bool {
        let (sx, sy) = self.scales();
        self.offset.is_none_or(|[x, y]| x == 0.0 && y == 0.0)
            && sx == 1.0
            && sy == 1.0
            && self.rotate.is_none_or(|r| r == 0.0)
    }
}

/// A scale: one factor for both axes, or one each.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum NodeScale {
    Uniform(f64),
    Axes([f64; 2]),
}

/// One element's values on a layer keyframe (SVG-PLAN.md slice 3,
/// `ProjectLayerKeyframe::nodes`): the keys an override takes that can
/// move — tier A's `visible`, `opacity` and `transform`, a painted
/// shape's `fill`, `stroke`, `fillStops`, `strokeStops` and
/// `strokeWidth`. `order` and `text` are not keyed (an order has no
/// halfway, and words are outlined on the machine that edits them): they
/// stay the resource's static overrides. Any other key is kept as written.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NodeKey {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<NodeTransform>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_stops: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke_stops: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke_width: Option<f64>,
    /// Keys a later build adds, kept as written.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl NodeKey {
    /// Whether it keys nothing at all.
    pub fn is_empty(&self) -> bool {
        self.visible.is_none()
            && self.opacity.is_none()
            && self
                .transform
                .as_ref()
                .is_none_or(|t| t.offset.is_none() && t.scale.is_none() && t.rotate.is_none())
            && self.fill.is_none()
            && self.stroke.is_none()
            && self.fill_stops.is_none()
            && self.stroke_stops.is_none()
            && self.stroke_width.is_none()
            && self.extra.is_empty()
    }

    /// Why a key it carries cannot be keyed, when one cannot: `order`
    /// (an element has no place halfway between two) and `text` (words
    /// are outlined on the machine that edits them) stay the resource's,
    /// and any other unknown key is named.
    pub fn refused_key(&self) -> Option<String> {
        let name = self.extra.keys().next()?;
        Some(match name.as_str() {
            "order" => "order is not keyed — an element has no place halfway between two; \
                        set it on the SVG with setVectorOverride"
                .to_string(),
            "text" => "words are not keyed — they are outlined on the machine that edits \
                       them; set them on the SVG with setVectorOverride"
                .to_string(),
            other => format!(
                "a keyframe keys visible, opacity, transform, fill, stroke, fillStops, \
                 strokeStops and strokeWidth — not \"{other}\""
            ),
        })
    }

    /// As an override of `node` with exactly these keys — what a tier
    /// check reads.
    pub fn as_override(&self, node: &str) -> VectorOverride {
        VectorOverride {
            node: node.to_string(),
            visible: self.visible,
            opacity: self.opacity,
            transform: self.transform.clone(),
            fill: self.fill.clone(),
            stroke: self.stroke.clone(),
            fill_stops: self.fill_stops.clone(),
            stroke_stops: self.stroke_stops.clone(),
            stroke_width: self.stroke_width,
            extra: self.extra.clone(),
            ..Default::default()
        }
    }
}

/// Every element a layer keys, with the keyframe times that key it:
/// `(address, keys)` — what inspect and validate list.
pub fn keyed_nodes(layer: &crate::ProjectLayer) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut out: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for k in &layer.keyframes {
        for (address, key) in k.nodes.iter().flatten() {
            let keys = out.entry(address.clone()).or_default();
            let wire = serde_json::to_value(key).unwrap_or_default();
            for name in wire.as_object().into_iter().flat_map(|m| m.keys()) {
                if !keys.contains(name) {
                    keys.push(name.clone());
                }
            }
        }
    }
    out
}

/// The overrides a vector resource carries (none when the block is absent
/// or does not decode).
pub fn overrides(resource: &crate::ProjectResource) -> Vec<VectorOverride> {
    block(resource).map(|b| b.overrides).unwrap_or_default()
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
        // `overrides` decode as slice 2 reads them.
        assert_eq!(read.overrides.len(), 1);
        assert_eq!(read.overrides[0].node, "badge");
        assert_eq!(read.overrides[0].opacity, Some(0.5));
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

    /// An override keeps every key it does not read, and its transform
    /// takes one scale or two.
    #[test]
    fn an_override_round_trips() {
        let raw = serde_json::json!({
            "node": "badge/rect[0]", "fill": "FF6A00", "fillRole": "accent",
            "transform": {"move": [4.0, -2.0], "scale": [1.5, 2.0], "rotate": 12.0}
        });
        let o: VectorOverride = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(o.transform.as_ref().unwrap().scales(), (1.5, 2.0));
        assert!(o.edits_tree() && o.edits_paint());
        assert_eq!(serde_json::to_value(&o).unwrap(), raw);
        let uniform: NodeTransform =
            serde_json::from_value(serde_json::json!({"scale": 2})).unwrap();
        assert_eq!(uniform.scales(), (2.0, 2.0));
        assert!(NodeTransform::default().is_identity());
        let words: VectorOverride =
            serde_json::from_value(serde_json::json!({"node": "tagline", "text": "Hi"})).unwrap();
        assert!(!words.edits_tree() && !words.is_empty());
    }
}
