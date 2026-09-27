//! Carrying what a lossy writer could not read (review 2026-09-27, P2-45).
//!
//! The Mac and iOS apps edit a Swift copy of the format and hand the core
//! its encoding after every edit — an encoding with no key the Swift model
//! does not read, and the fallback in place of every enum value it does not
//! know. The core document keeps what it read: [`ProjectMetadata::carry_unread_from`]
//! puts it back into the new copy — an `extra` key the new copy lacks, an
//! unknown enum value where the new copy wrote that enum's fallback.
//! Entities match by id (keyframes within their layer, cuts within their
//! resource, effects by position); a new entity carries nothing, and one
//! the new copy removed stays removed.
//!
//! [`ProjectMetadata::mirror_unread_from`] is the other direction: a file
//! another writer changed is the latest word on what the apps cannot see,
//! so its unread content replaces the document's — a key it removed goes.
//!
//! Some bag keys are not unread at all: the apps' model reads and writes
//! them ([`APP_PROJECT_KEYS`], [`APP_RESOURCE_KEYS`]). The apps' copy is
//! their authority — carrying one back would undo the app removing it.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::{
    LayerTransition, Placement, ProjectLayer, ProjectMetadata, ProjectResource, ResourceFrame,
    Tolerant,
};

/// Project keys this model keeps in its bag that the apps' Swift model
/// reads and writes: the author's handles.
pub const APP_PROJECT_KEYS: &[&str] = &["handles"];
/// Resource keys this model keeps in its bag that the apps' Swift model
/// reads and writes: a library reference, the theme plate, a resource's
/// own motion, where it came from, a narration's speech. Checked against
/// the Swift coding keys on 2026-09-27 (no layer, keyframe, cut, export or
/// settings key is one); the app's suite fails when a key it writes lands
/// in a bag unnamed here.
pub const APP_RESOURCE_KEYS: &[&str] = &[
    "libraryID",
    "motionKeyframes",
    "origin",
    "speech",
    "themePlateID",
];

/// Fill: put back what the new copy lacks. Mirror: take the source's
/// unread content as it is, removals included.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Fill,
    Mirror,
}

fn carry_map(into: &mut Map<String, Value>, from: &Map<String, Value>, app: &[&str], mode: Mode) {
    let unread = |key: &str| !app.contains(&key);
    if mode == Mode::Mirror {
        into.retain(|key, _| !unread(key) || from.contains_key(key));
    }
    for (key, value) in from {
        if !unread(key) {
            continue;
        }
        match mode {
            Mode::Fill => {
                into.entry(key.clone()).or_insert_with(|| value.clone());
            }
            Mode::Mirror => {
                into.insert(key.clone(), value.clone());
            }
        }
    }
}

/// Fill: an unknown value the lossy side rewrote as the fallback comes
/// back. Mirror: where both say the same thing to this build, the source's
/// exact value stands — its unknown value, or its explicit fallback.
fn keep<E: Tolerant>(into: &mut E, from: E, mode: Mode) {
    match mode {
        Mode::Fill if from.is_unknown() && *into == from.as_known() => *into = from,
        Mode::Mirror if into.as_known() == from.as_known() => *into = from,
        _ => {}
    }
}

fn keep_opt<E: Tolerant>(into: &mut Option<E>, from: Option<E>, mode: Mode) {
    if let (Some(value), Some(old)) = (into.as_mut(), from) {
        keep(value, old, mode);
    }
}

fn carry_transition(
    into: &mut Option<LayerTransition>,
    from: &Option<LayerTransition>,
    mode: Mode,
) {
    if let (Some(value), Some(old)) = (into.as_mut(), from.as_ref()) {
        keep(&mut value.kind, old.kind, mode);
        keep_opt(&mut value.from, old.from, mode);
        keep_opt(&mut value.easing, old.easing, mode);
    }
}

fn carry_placement(into: &mut Option<Placement>, from: &Option<Placement>, mode: Mode) {
    if let (Some(value), Some(old)) = (into.as_mut(), from.as_ref()) {
        keep_opt(&mut value.anchor, old.anchor, mode);
        keep_opt(&mut value.mode, old.mode, mode);
    }
}

fn carry_frame(into: &mut Option<ResourceFrame>, from: &Option<ResourceFrame>, mode: Mode) {
    if let (Some(value), Some(old)) = (into.as_mut(), from.as_ref()) {
        keep(&mut value.kind, old.kind, mode);
        keep(&mut value.material, old.material, mode);
    }
}

fn carry_layer(into: &mut ProjectLayer, from: &ProjectLayer, mode: Mode) {
    carry_map(&mut into.extra, &from.extra, &[], mode);
    keep_opt(&mut into.blend_mode, from.blend_mode, mode);
    carry_transition(&mut into.transition_in, &from.transition_in, mode);
    carry_transition(&mut into.transition_out, &from.transition_out, mode);
    for keyframe in &mut into.keyframes {
        let Some(old) = from.keyframes.iter().find(|k| k.id == keyframe.id) else {
            continue;
        };
        carry_map(&mut keyframe.extra, &old.extra, &[], mode);
        keep_opt(&mut keyframe.easing, old.easing, mode);
        carry_placement(&mut keyframe.placement, &old.placement, mode);
        carry_transition(&mut keyframe.transition, &old.transition, mode);
    }
}

fn carry_resource(into: &mut ProjectResource, from: &ProjectResource, mode: Mode) {
    carry_map(&mut into.extra, &from.extra, APP_RESOURCE_KEYS, mode);
    carry_frame(&mut into.frame, &from.frame, mode);
    for cut in &mut into.media_cuts {
        if let Some(old) = from.media_cuts.iter().find(|c| c.id == cut.id) {
            carry_map(&mut cut.extra, &old.extra, &[], mode);
        }
    }
    if let (Some(effects), Some(old)) = (into.audio_effects.as_mut(), from.audio_effects.as_ref()) {
        for (effect, previous) in effects.iter_mut().zip(old) {
            keep(&mut effect.kind, previous.kind, mode);
        }
    }
}

/// Every layer of a document by id: the project's, the stage members', and
/// the compositions' — ids are unique in the file.
fn layers_by_id(meta: &ProjectMetadata) -> HashMap<&str, &ProjectLayer> {
    crate::nesting::all_layers(meta)
        .into_iter()
        .map(|layer| (layer.id.as_str(), layer))
        .collect()
}

fn carry_layers(into: &mut [ProjectLayer], from: &HashMap<&str, &ProjectLayer>, mode: Mode) {
    for layer in into {
        if let Some(old) = from.get(layer.id.as_str()) {
            carry_layer(layer, old, mode);
        }
        if let Some(members) = layer.members.as_mut() {
            carry_layers(members, from, mode);
        }
    }
}

impl ProjectMetadata {
    /// Puts back what a lossy copy of `from` dropped: keys this model
    /// carries unread, and enum values it does not know that the copy
    /// rewrote as their fallback. `self` is the new copy; its own edits win.
    pub fn carry_unread_from(&mut self, from: &ProjectMetadata) {
        self.carry(from, Mode::Fill);
    }

    /// Takes `from`'s unread content as this document's, for every entity
    /// both hold: its unread keys exactly (a key it removed goes, a value it
    /// changed is its value) and its enum values where both mean the same
    /// to this build. What the apps' model reads is left alone.
    pub fn mirror_unread_from(&mut self, from: &ProjectMetadata) {
        self.carry(from, Mode::Mirror);
    }

    fn carry(&mut self, from: &ProjectMetadata, mode: Mode) {
        carry_map(&mut self.extra, &from.extra, APP_PROJECT_KEYS, mode);
        carry_map(
            &mut self.composition_settings.extra,
            &from.composition_settings.extra,
            &[],
            mode,
        );
        let old_layers = layers_by_id(from);
        carry_layers(
            self.layers.as_deref_mut().unwrap_or(&mut []),
            &old_layers,
            mode,
        );
        let old_resources: HashMap<&str, &ProjectResource> = from
            .resources
            .iter()
            .flatten()
            .map(|r| (r.id.as_str(), r))
            .collect();
        for resource in self.resources.iter_mut().flatten() {
            if let Some(old) = old_resources.get(resource.id.as_str()) {
                carry_resource(resource, old, mode);
            }
            if let Some(composition) = resource.composition.as_mut() {
                carry_layers(&mut composition.layers, &old_layers, mode);
            }
        }
        for marker in self.markers.iter_mut().flatten() {
            if let Some(old) = from.markers.iter().flatten().find(|m| m.id == marker.id) {
                keep(&mut marker.kind, old.kind, mode);
            }
        }
        for export in self.exports.iter_mut().flatten() {
            if let Some(old) = from.exports.iter().flatten().find(|e| e.id == export.id) {
                carry_map(&mut export.extra, &old.extra, &[], mode);
                keep(&mut export.kind, old.kind, mode);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn meta(value: Value) -> ProjectMetadata {
        ProjectMetadata::from_json(&value.to_string()).unwrap()
    }

    fn document(layer_extra: bool) -> Value {
        let mut layer = json!({
            "id": "L", "name": "L", "sortIndex": 0, "kind": "image", "isEnabled": true,
            "startTime": 0, "duration": 4, "resourceID": "R", "blendMode": "hue",
            "transitionIn": { "kind": "spiral", "duration": 0.5 },
            "keyframes": [{ "id": "K", "time": 0, "easing": "bounce",
                            "placement": { "anchor": "nowhere" }, "futureKeyframe": 1 }]
        });
        if layer_extra {
            layer["futureLayer"] = json!("kept");
        }
        json!({
            "id": "P", "name": "P", "createdAt": 0, "state": "recorded", "trimStart": 0,
            "trimEnd": 0, "videoDuration": 0, "subtitles": [],
            "compositionSettings": { "futureSetting": true },
            "futureTop": [1],
            "markers": [{ "id": "M", "time": 1, "kind": "bookmark" }],
            "layers": [layer],
            "resources": [{ "id": "R", "kind": "image", "filename": "a.png", "displayName": "A",
                            "addedAt": 0, "futureResource": 2,
                            "frame": { "kind": "hologram", "material": "carbon" } }]
        })
    }

    /// What a lossy copy drops comes back from the document it was made
    /// from; the copy's own edits stand.
    #[test]
    fn a_lossy_copy_gets_back_what_it_could_not_read() {
        let original = meta(document(true));
        // The copy a model without those keys or values writes: extras
        // gone, unknown values replaced by their fallbacks, one real edit.
        let mut copy = document(false);
        copy.as_object_mut().unwrap().remove("futureTop");
        copy["compositionSettings"] = json!({});
        let layer = &mut copy["layers"][0];
        layer["name"] = json!("Renamed");
        layer["blendMode"] = json!("normal");
        layer["transitionIn"]["kind"] = json!("fade");
        let keyframe = &mut layer["keyframes"][0];
        keyframe.as_object_mut().unwrap().remove("futureKeyframe");
        keyframe["easing"] = json!("linear");
        keyframe["placement"]["anchor"] = json!("center");
        copy["markers"][0]["kind"] = json!("marker");
        copy["resources"][0]
            .as_object_mut()
            .unwrap()
            .remove("futureResource");
        copy["resources"][0]["frame"] = json!({ "kind": "none", "material": "spaceBlack" });

        let mut carried = meta(copy);
        carried.carry_unread_from(&original);
        let out: Value = serde_json::from_str(&carried.to_json().unwrap()).unwrap();
        assert_eq!(out["futureTop"], json!([1]));
        assert_eq!(out["compositionSettings"]["futureSetting"], true);
        let layer = &out["layers"][0];
        assert_eq!(layer["name"], "Renamed", "the copy's own edit stands");
        assert_eq!(layer["futureLayer"], "kept");
        assert_eq!(layer["blendMode"], "hue");
        assert_eq!(layer["transitionIn"]["kind"], "spiral");
        assert_eq!(layer["keyframes"][0]["futureKeyframe"], 1);
        assert_eq!(layer["keyframes"][0]["easing"], "bounce");
        assert_eq!(layer["keyframes"][0]["placement"]["anchor"], "nowhere");
        assert_eq!(out["markers"][0]["kind"], "bookmark");
        assert_eq!(out["resources"][0]["futureResource"], 2);
        assert_eq!(out["resources"][0]["frame"]["kind"], "hologram");
        assert_eq!(out["resources"][0]["frame"]["material"], "carbon");
    }

    /// A value the copy CHANGED to something other than the fallback is an
    /// edit, and stands; an entity the copy removed stays removed.
    #[test]
    fn an_edit_or_a_removal_is_not_undone() {
        let original = meta(document(true));
        let mut copy = document(false);
        copy["layers"][0]["blendMode"] = json!("multiply");
        copy["layers"][0]["keyframes"] = json!([]);
        copy["markers"] = json!([]);
        let mut carried = meta(copy);
        carried.carry_unread_from(&original);
        let out: Value = serde_json::from_str(&carried.to_json().unwrap()).unwrap();
        assert_eq!(out["layers"][0]["blendMode"], "multiply");
        assert_eq!(out["layers"][0]["keyframes"], json!([]));
        assert!(out.get("markers").is_none() || out["markers"] == json!([]));
    }

    /// What the apps' model reads is the apps' to remove: a narration's
    /// speech the app deleted, the handles it rewrote, stay as the app
    /// wrote them — the carry is only for what the app cannot see.
    #[test]
    fn the_apps_own_keys_are_theirs() {
        let mut with = document(true);
        with["handles"] = json!({ "L": "card" });
        with["resources"][0]["speech"] = json!({ "text": "hi" });
        let original = meta(with.clone());
        let mut copy = with;
        copy["handles"] = json!({ "L": "deck" });
        copy["resources"][0]
            .as_object_mut()
            .unwrap()
            .remove("speech");
        let mut carried = meta(copy);
        carried.carry_unread_from(&original);
        let out: Value = serde_json::from_str(&carried.to_json().unwrap()).unwrap();
        assert!(
            out["resources"][0].get("speech").is_none(),
            "the app removed it: {}",
            out["resources"][0]
        );
        assert_eq!(out["handles"], json!({ "L": "deck" }));
        assert_eq!(out["resources"][0]["futureResource"], 2, "still carried");
    }

    /// A file another writer changed is the latest word on the unread
    /// content: a key it removed goes, a value it changed is its value, an
    /// unknown value it set to the fallback is the fallback — and what the
    /// apps read is left to the apps.
    #[test]
    fn a_mirror_takes_the_files_unread_content() {
        let mut with_speech = document(true);
        with_speech["resources"][0]["speech"] = json!({ "text": "mine" });
        let mut doc = meta(with_speech);
        let mut file = document(false);
        file["layers"][0]["keyframes"][0]["futureKeyframe"] = json!(2);
        file["layers"][0]["blendMode"] = json!("normal");
        file["layers"][0]["transitionIn"]["kind"] = json!("wipe");
        file["resources"][0]["frame"]["kind"] = json!("bezel");
        file["compositionSettings"] = json!({ "otherSetting": 1 });
        doc.mirror_unread_from(&meta(file));
        let out: Value = serde_json::from_str(&doc.to_json().unwrap()).unwrap();
        let layer = &out["layers"][0];
        assert!(
            layer.get("futureLayer").is_none(),
            "removed by the file: {layer}"
        );
        assert_eq!(layer["keyframes"][0]["futureKeyframe"], 2);
        assert_eq!(layer["blendMode"], "normal", "the file's explicit fallback");
        assert_eq!(
            layer["transitionIn"]["kind"], "spiral",
            "a known value differs from the document's: the apps' sync decides it"
        );
        assert_eq!(out["compositionSettings"]["otherSetting"], 1);
        assert!(out["compositionSettings"].get("futureSetting").is_none());
        assert_eq!(
            out["resources"][0]["speech"]["text"], "mine",
            "the apps' key"
        );
        assert_eq!(
            out["resources"][0]["frame"]["kind"], "bezel",
            "both unknown to this build, so both mean the fallback: the file's own value"
        );
    }
}
