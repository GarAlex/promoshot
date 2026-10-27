//! An SVG's element overrides (SVG-PLAN.md slice 2), as document edits.
//!
//! `setVectorOverride` and `resetVectorOverride` change a vector
//! resource's `svg.overrides` — never the file. What an element may change
//! is the core's classification of the file (promo-svg), which reading
//! the file needs; and new words for a text need outlines made with this
//! machine's fonts. Both are I/O, which this crate does not do: the host
//! supplies a [`VectorHost`] (the headless tools and the apps' document
//! both use promo-author's, over the project's `Resources/`).

use promo_model::vector::{VectorBlock, VectorOverride, KEY};
use promo_model::{ProjectMetadata, ProjectResource, ProjectResourceKind};
use serde_json::Value;

/// What the document asks of the project's files for an SVG edit.
pub trait VectorHost: Send {
    /// `o` checked against the file's elements as an editor's edit is:
    /// its node resolved (an address, or the author's name) and every key
    /// inside that element's tier — the resolved address, or the reason
    /// the element shows for refusing it.
    fn check(&mut self, resource: &ProjectResource, o: &VectorOverride) -> Result<String, String>;

    /// Outlines for the file with `texts` (address → words) in place of
    /// its words: the `svg` block's new `outlines`, `sourceHash`,
    /// `missingFonts` (and `size`, `handles`) as a JSON object.
    fn outline(
        &mut self,
        resource: &ProjectResource,
        texts: &[(String, String)],
    ) -> Result<Value, String>;
}

fn merge_patch(target: &mut Value, patch: &Value) {
    match patch {
        Value::Object(entries) => {
            if !target.is_object() {
                *target = Value::Object(Default::default());
            }
            let map = target.as_object_mut().expect("object");
            for (key, value) in entries {
                if value.is_null() {
                    map.remove(key);
                } else {
                    merge_patch(map.entry(key.clone()).or_insert(Value::Null), value);
                }
            }
        }
        other => *target = other.clone(),
    }
}

fn texts(block: &VectorBlock) -> Vec<(String, String)> {
    block
        .overrides
        .iter()
        .filter_map(|o| o.text.clone().map(|t| (o.node.clone(), t)))
        .collect()
}

fn vector<'a>(meta: &'a mut ProjectMetadata, id: &str) -> Result<&'a mut ProjectResource, String> {
    let resource = meta
        .resources
        .iter_mut()
        .flatten()
        .find(|r| r.id == id)
        .ok_or_else(|| format!("no resource with id {id}"))?;
    if resource.kind != ProjectResourceKind::Vector {
        return Err(format!(
            "resource {id} is a {:?}, not an SVG (vector) — overrides edit an SVG's elements",
            resource.kind
        ));
    }
    Ok(resource)
}

/// Writes `block` back, outlining again first when the words changed.
fn store(
    resource: &mut ProjectResource,
    mut block: VectorBlock,
    words_before: &[(String, String)],
    host: Option<&mut (dyn VectorHost + '_)>,
) -> Result<(), String> {
    let words = texts(&block);
    if words != words_before {
        let host = host.ok_or(
            "new words for an SVG's text are outlined with this machine's \
                               fonts, which needs the project's folder",
        )?;
        let made = host.outline(resource, &words)?;
        for key in ["outlines", "sourceHash", "missingFonts", "size", "handles"] {
            match made.get(key) {
                Some(v) => {
                    let mut wire = serde_json::to_value(&block).map_err(|e| e.to_string())?;
                    wire[key] = v.clone();
                    block = serde_json::from_value(wire).map_err(|e| e.to_string())?;
                }
                None if key == "missingFonts" => block.missing_fonts = None,
                None => {}
            }
        }
    }
    let wire = serde_json::to_value(&block).map_err(|e| e.to_string())?;
    resource.extra.insert(KEY.to_string(), wire);
    Ok(())
}

/// `setVectorOverride`: a merge patch over the element's override.
pub(crate) fn set(
    meta: &mut ProjectMetadata,
    host: Option<&mut (dyn VectorHost + '_)>,
    resource_id: &str,
    node: &str,
    patch: &Value,
) -> Result<(), String> {
    if !patch.is_object() {
        return Err(
            "`patch` must be an object of override keys — visible, opacity, transform, \
                    order, fill, stroke, fillStops, strokeStops, strokeWidth, text"
                .into(),
        );
    }
    if patch.get("node").is_some() {
        return Err("`node` is the command's, not the patch's".into());
    }
    let host =
        host.ok_or("editing an SVG's elements reads the file, which needs the project's folder")?;
    let resource = vector(meta, resource_id)?;
    let block = promo_model::vector::block(resource).unwrap_or_default();
    let before = texts(&block);
    // The element, by address or by the author's name; a locked one
    // refuses here with its reason.
    let probe = VectorOverride {
        node: node.to_string(),
        ..Default::default()
    };
    let address = host.check(resource, &probe)?;
    let mut block = block;
    let at = block.overrides.iter().position(|o| o.node == address);
    let mut wire = match at {
        Some(i) => serde_json::to_value(&block.overrides[i]).map_err(|e| e.to_string())?,
        None => serde_json::json!({ "node": address }),
    };
    merge_patch(&mut wire, patch);
    wire["node"] = Value::from(address.clone());
    let merged: VectorOverride = serde_json::from_value(wire)
        .map_err(|e| format!("override rejected by the format: {e}"))?;
    host.check(resource, &merged)?;
    match (at, merged.is_empty()) {
        (Some(i), true) => {
            block.overrides.remove(i);
        }
        (Some(i), false) => block.overrides[i] = merged,
        (None, true) => {}
        (None, false) => block.overrides.push(merged),
    }
    store(resource, block, &before, Some(host))
}

/// `resetVectorOverride`: one element back to the file's look, or —
/// no node — every element.
pub(crate) fn reset(
    meta: &mut ProjectMetadata,
    mut host: Option<&mut (dyn VectorHost + '_)>,
    resource_id: &str,
    node: Option<&str>,
) -> Result<(), String> {
    let resource = vector(meta, resource_id)?;
    let mut block = promo_model::vector::block(resource).unwrap_or_default();
    let before = texts(&block);
    match node {
        None => block.overrides.clear(),
        Some(node) => {
            // The address, or the author's name the import kept.
            let named = block
                .handles
                .iter()
                .filter(|(_, name)| name.as_str() == node)
                .map(|(address, _)| address.clone())
                .collect::<Vec<_>>();
            let address = match host.as_deref_mut() {
                _ if block.overrides.iter().any(|o| o.node == node) => node.to_string(),
                _ if named.len() == 1 => named[0].clone(),
                Some(h) => h
                    .check(
                        resource,
                        &VectorOverride {
                            node: node.to_string(),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_else(|_| node.to_string()),
                None => node.to_string(),
            };
            let had = block.overrides.len();
            block.overrides.retain(|o| o.node != address);
            if block.overrides.len() == had {
                return Err(format!(
                    "the SVG has no override for \"{node}\" — nothing to reset"
                ));
            }
        }
    }
    store(resource, block, &before, host)
}
