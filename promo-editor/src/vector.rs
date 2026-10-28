//! An SVG's element overrides (SVG-PLAN.md slice 2), as document edits.
//!
//! `setVectorOverride` and `resetVectorOverride` change a vector
//! resource's `svg.overrides` — never the file. What an element may change
//! is the core's classification of the file (promo-svg), which reading
//! the file needs; and new words for a text need outlines made with this
//! machine's fonts. Both are I/O, which this crate does not do: the host
//! supplies a [`VectorHost`] (the headless tools and the apps' document
//! both use promo-author's, over the project's `Resources/`).

use promo_model::vector::{NodeKey, VectorBlock, VectorOverride, KEY};
use promo_model::{ProjectMetadata, ProjectResource, ProjectResourceKind};
use serde_json::Value;

/// What the document asks of the project's files for an SVG edit.
pub trait VectorHost: Send {
    /// `o` checked against the file's elements as an editor's edit is:
    /// its node resolved (an address, or the author's name) and every key
    /// inside that element's tier — the resolved address, or the reason
    /// the element shows for refusing it.
    fn check(&mut self, resource: &ProjectResource, o: &VectorOverride) -> Result<String, String>;

    /// `key` — a layer keyframe's values for element `node` (slice 3) —
    /// checked as an editor's edit is: the resolved address, or the
    /// reason. The keys a keyframe cannot carry (`order`, `text`, anything
    /// unknown) are refused first; the rest are the override's check.
    fn check_key(
        &mut self,
        resource: &ProjectResource,
        node: &str,
        key: &NodeKey,
    ) -> Result<String, String> {
        if let Some(why) = key.refused_key() {
            return Err(why);
        }
        self.check(resource, &key.as_override(node))
    }

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

// ---------------------------------------------------------------------------
// Keyed elements (SVG-PLAN.md slice 3): `nodes` on a layer's keyframes.

use crate::document::Command;

/// Whether `command` keys an SVG's elements — the commands that need the
/// project's files: the two key commands, and a keyframe upsert that
/// carries `nodes`.
pub(crate) fn keys_nodes(command: &Command) -> bool {
    match command {
        Command::SetVectorNodeKey { .. } | Command::RemoveVectorNodeKey { .. } => true,
        Command::UpsertKeyframe { keyframe, .. } => keyframe.nodes.is_some(),
        _ => false,
    }
}

/// Runs a command [`keys_nodes`] names on `meta`.
pub(crate) fn run_key(
    meta: &mut ProjectMetadata,
    host: Option<&mut (dyn VectorHost + '_)>,
    command: &Command,
) -> Result<(), String> {
    match command {
        Command::SetVectorNodeKey {
            layer_id,
            node,
            time,
            keyframe_id,
            patch,
            transition_duration,
            transition_percent,
            easing,
        } => set_key(
            meta,
            host,
            layer_id,
            node,
            (*time, keyframe_id.as_deref()),
            patch,
            (*transition_duration, *transition_percent, *easing),
        ),
        Command::RemoveVectorNodeKey {
            layer_id,
            node,
            time,
            keyframe_id,
            keys,
        } => remove_key(
            meta,
            host,
            layer_id,
            node,
            (*time, keyframe_id.as_deref()),
            keys.as_deref(),
        ),
        Command::UpsertKeyframe { layer_id, keyframe } => {
            upsert_checked(meta, host, layer_id, keyframe)
        }
        other => crate::document::Document::run(meta, other),
    }
}

/// Two keyframe times this close are one place on the layer.
const SAME_TIME: f64 = 1e-3;

fn find_layer<'a>(
    layers: &'a mut [promo_model::ProjectLayer],
    id: &str,
) -> Option<&'a mut promo_model::ProjectLayer> {
    for layer in layers.iter_mut() {
        if layer.id == id {
            return Some(layer);
        }
        if let Some(found) = layer.members.as_deref_mut().and_then(|m| find_layer(m, id)) {
            return Some(found);
        }
    }
    None
}

fn layer<'a>(
    meta: &'a mut ProjectMetadata,
    id: &str,
) -> Result<&'a mut promo_model::ProjectLayer, String> {
    find_layer(meta.layers.get_or_insert_with(Vec::new), id)
        .ok_or_else(|| format!("no layer with id {id}"))
}

/// The vector the layer shows at `local` seconds of its own.
fn shown_vector(
    meta: &ProjectMetadata,
    layer: &promo_model::ProjectLayer,
    local: f64,
) -> Result<ProjectResource, String> {
    let resources = meta.resources.as_deref().unwrap_or(&[]);
    let id = promo_timeline::layer_resource_id(layer, layer.start_time + local, resources)
        .or(layer.resource_id.as_deref());
    let name = if layer.name.is_empty() {
        &layer.id
    } else {
        &layer.name
    };
    resources
        .iter()
        .find(|r| Some(r.id.as_str()) == id && r.kind == ProjectResourceKind::Vector)
        .cloned()
        .ok_or_else(|| {
            format!(
                "the layer \"{name}\" shows no SVG at {local}s — an element is keyed on an \
                 image layer showing a vector resource"
            )
        })
}

/// Whether a keyframe says nothing but where it is and how it ramps.
fn is_bare(k: &promo_model::ProjectLayerKeyframe) -> bool {
    if k.wait {
        return false;
    }
    let wire = serde_json::to_value(k).unwrap_or_default();
    wire.as_object().is_some_and(|m| {
        m.keys().all(|key| {
            matches!(
                key.as_str(),
                "id" | "time"
                    | "transitionDuration"
                    | "transitionPercent"
                    | "easing"
                    | "overshoot"
                    | "timeSpecs"
            )
        })
    })
}

fn sort(keyframes: &mut [promo_model::ProjectLayerKeyframe]) {
    keyframes.sort_by(|a, b| {
        a.time
            .partial_cmp(&b.time)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// `setVectorNodeKey`.
#[allow(clippy::too_many_arguments)]
fn set_key(
    meta: &mut ProjectMetadata,
    host: Option<&mut (dyn VectorHost + '_)>,
    layer_id: &str,
    node: &str,
    (time, keyframe_id): (Option<f64>, Option<&str>),
    patch: &Value,
    (ramp, percent, easing): (Option<f64>, Option<f64>, Option<promo_model::Easing>),
) -> Result<(), String> {
    if !patch.is_object() {
        return Err(
            "`patch` must be an object of keys — visible, opacity, transform, fill, stroke, \
             fillStops, strokeStops, strokeWidth"
                .into(),
        );
    }
    if patch.get("node").is_some() {
        return Err("`node` is the command's, not the patch's".into());
    }
    if let Some(t) = time {
        if !(t.is_finite() && t >= 0.0) {
            return Err(format!("bad keyframe time {t}"));
        }
    }
    for (name, value) in [("transitionDuration", ramp), ("transitionPercent", percent)] {
        if let Some(v) = value {
            if !(v.is_finite() && v >= 0.0) {
                return Err(format!("{name} {v} must be zero or more"));
            }
        }
    }
    let host =
        host.ok_or("keying an SVG's elements reads the file, which needs the project's folder")?;
    let snapshot = layer(meta, layer_id)?.clone();
    // The keyframe: named, else the one at `time`, else a new one.
    let at = match keyframe_id.and_then(|id| snapshot.keyframes.iter().position(|k| k.id == id)) {
        Some(i) => Some(i),
        None => time.and_then(|t| {
            snapshot
                .keyframes
                .iter()
                .position(|k| (k.time - t).abs() < SAME_TIME)
        }),
    };
    let when = match (at, time) {
        (Some(i), _) => snapshot.keyframes[i].time,
        (None, Some(t)) => t,
        (None, None) => {
            return Err(match keyframe_id {
                Some(id) => format!("no keyframe {id} on the layer — give `time` to make one"),
                None => "give `time` (the layer's own seconds) or `keyframeID`".into(),
            })
        }
    };
    let resource = shown_vector(meta, &snapshot, when)?;
    let probe = VectorOverride {
        node: node.to_string(),
        ..Default::default()
    };
    let address = host.check(&resource, &probe)?;
    let held = at
        .and_then(|i| snapshot.keyframes[i].nodes.as_ref()?.get(&address).cloned())
        .unwrap_or_default();
    let mut wire = serde_json::to_value(&held).map_err(|e| e.to_string())?;
    merge_patch(&mut wire, patch);
    let merged: NodeKey =
        serde_json::from_value(wire).map_err(|e| format!("key rejected by the format: {e}"))?;
    if !merged.is_empty() {
        host.check_key(&resource, &address, &merged)?;
    }
    let layer = layer(meta, layer_id)?;
    let i = match at {
        Some(i) => i,
        None => {
            if merged.is_empty() {
                return Ok(());
            }
            let mut id = keyframe_id
                .map(str::to_string)
                .unwrap_or_else(|| format!("node-{when:.3}"));
            while layer.keyframes.iter().any(|k| k.id == id) {
                id.push('b');
            }
            let fresh: promo_model::ProjectLayerKeyframe = serde_json::from_value(
                serde_json::json!({"id": id, "time": when, "transitionDuration": 0.5}),
            )
            .map_err(|e| e.to_string())?;
            layer.keyframes.push(fresh);
            layer.keyframes.len() - 1
        }
    };
    let k = &mut layer.keyframes[i];
    let nodes = k.nodes.get_or_insert_with(Default::default);
    if merged.is_empty() {
        nodes.remove(&address);
    } else {
        nodes.insert(address, merged);
    }
    if nodes.is_empty() {
        k.nodes = None;
    }
    if let Some(v) = ramp {
        k.transition_duration = v;
        promo_model::musical::forget(&mut k.time_specs, "transitionDuration");
    }
    if let Some(v) = percent {
        k.transition_percent = Some(v.min(100.0));
    }
    if let Some(e) = easing {
        k.easing = Some(e);
    }
    if is_bare(k) {
        layer.keyframes.remove(i);
    }
    sort(&mut layer.keyframes);
    Ok(())
}

/// `removeVectorNodeKey`.
fn remove_key(
    meta: &mut ProjectMetadata,
    host: Option<&mut (dyn VectorHost + '_)>,
    layer_id: &str,
    node: &str,
    (time, keyframe_id): (Option<f64>, Option<&str>),
    keys: Option<&[String]>,
) -> Result<(), String> {
    let snapshot = layer(meta, layer_id)?.clone();
    // The address as written, else the name resolved through the file.
    let written = |a: &str| {
        snapshot
            .keyframes
            .iter()
            .any(|k| k.nodes.as_ref().is_some_and(|n| n.contains_key(a)))
    };
    let address = if written(node) {
        node.to_string()
    } else {
        let resolved = host.and_then(|h| {
            let resource = shown_vector(meta, &snapshot, time.unwrap_or(0.0)).ok()?;
            h.check(
                &resource,
                &VectorOverride {
                    node: node.to_string(),
                    ..Default::default()
                },
            )
            .ok()
        });
        resolved.unwrap_or_else(|| node.to_string())
    };
    let layer = layer(meta, layer_id)?;
    let mut removed = false;
    for k in layer.keyframes.iter_mut() {
        let chosen = match (keyframe_id, time) {
            (Some(id), _) => k.id == id,
            (None, Some(t)) => (k.time - t).abs() < SAME_TIME,
            (None, None) => true,
        };
        let Some(nodes) = k.nodes.as_mut().filter(|_| chosen) else {
            continue;
        };
        let Some(key) = nodes.get(&address) else {
            continue;
        };
        match keys {
            None => {
                nodes.remove(&address);
                removed = true;
            }
            Some(names) => {
                let mut wire = serde_json::to_value(key).map_err(|e| e.to_string())?;
                let map = wire.as_object_mut().expect("a key is an object");
                for name in names {
                    removed |= map.remove(name).is_some();
                }
                let left: NodeKey = serde_json::from_value(wire).map_err(|e| e.to_string())?;
                if left.is_empty() {
                    nodes.remove(&address);
                } else {
                    nodes.insert(address.clone(), left);
                }
            }
        }
        if nodes.is_empty() {
            k.nodes = None;
        }
    }
    if !removed {
        return Err(format!(
            "the layer keys nothing of \"{node}\" there — nothing to remove"
        ));
    }
    layer.keyframes.retain(|k| !is_bare(k));
    Ok(())
}

/// `upsertKeyframe` carrying `nodes`: each element's keys the keyframe
/// changes checked as `setVectorNodeKey` checks them, names resolved to
/// addresses; then the plain upsert.
fn upsert_checked(
    meta: &mut ProjectMetadata,
    host: Option<&mut (dyn VectorHost + '_)>,
    layer_id: &str,
    keyframe: &promo_model::ProjectLayerKeyframe,
) -> Result<(), String> {
    let snapshot = layer(meta, layer_id)?.clone();
    let before = snapshot
        .keyframes
        .iter()
        .find(|k| k.id == keyframe.id)
        .and_then(|k| k.nodes.clone())
        .unwrap_or_default();
    let mut keyframe = keyframe.clone();
    let wanted = keyframe.nodes.take().unwrap_or_default();
    let changed: Vec<_> = wanted
        .iter()
        .filter(|(a, k)| before.get(*a) != Some(*k))
        .collect();
    let mut resolved = std::collections::BTreeMap::new();
    if !changed.is_empty() {
        let host = host
            .ok_or("keying an SVG's elements reads the file, which needs the project's folder")?;
        let resource = shown_vector(meta, &snapshot, keyframe.time)?;
        for (name, key) in &changed {
            let address = host.check_key(&resource, name, key)?;
            resolved.insert(name.to_string(), address);
        }
    }
    let nodes: std::collections::BTreeMap<String, NodeKey> = wanted
        .into_iter()
        .filter(|(_, k)| !k.is_empty())
        .map(|(a, k)| (resolved.get(&a).cloned().unwrap_or(a), k))
        .collect();
    keyframe.nodes = (!nodes.is_empty()).then_some(nodes);
    crate::document::Document::run(
        meta,
        &Command::UpsertKeyframe {
            layer_id: layer_id.to_string(),
            keyframe: Box::new(keyframe),
        },
    )
}
