//! Stage 2, slice 1: the core owns the document.
//!
//! `Document` holds the parsed project and a version counter; every
//! mutation is a [`Command`], applied here and nowhere else, which is what
//! makes undo a stack instead of a retrofit. History is whole-document
//! snapshots — projects are small JSON, and a snapshot per command is
//! simple and obviously correct; command inversion waits for a measurement
//! that says snapshots are too heavy (the plan's own words).
//!
//! The first command group is the layer editor's: rename, enable, timing,
//! audio focus, delete, reorder. Groups migrate here from the front ends
//! one at a time; a front end that needs a mutation this enum lacks adds
//! it HERE first.

use promo_model::ProjectMetadata;
use serde::Deserialize;

/// One editor mutation. Crosses as JSON (`{"kind": "renameLayer", ...}`);
/// commands are rare and tiny, which is why JSON is fine where the
/// per-frame path is not.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Command {
    #[serde(rename_all = "camelCase")]
    RenameLayer {
        #[serde(rename = "layerID")]
        layer_id: String,
        name: String,
    },
    #[serde(rename_all = "camelCase")]
    SetLayerEnabled {
        #[serde(rename = "layerID")]
        layer_id: String,
        enabled: bool,
    },
    /// `duration` absent = the layer runs to the composition's end.
    #[serde(rename_all = "camelCase")]
    SetLayerTiming {
        #[serde(rename = "layerID")]
        layer_id: String,
        start_time: f64,
        #[serde(default)]
        duration: Option<f64>,
    },
    #[serde(rename_all = "camelCase")]
    SetLayerAudioFocus {
        #[serde(rename = "layerID")]
        layer_id: String,
        focus: bool,
    },
    /// Points the layer at one of its resource's mediaCuts (absent clears
    /// it — the layer plays the resource's own trim again). Naming a cut
    /// the layer's resource does not hold is an error: render-time
    /// degradation exists for a cut that later VANISHES, but an editor
    /// must never mint a dangling pointer on purpose.
    #[serde(rename_all = "camelCase")]
    SetLayerMediaCut {
        #[serde(rename = "layerID")]
        layer_id: String,
        #[serde(default, rename = "mediaCutID")]
        media_cut_id: Option<String>,
    },
    /// The image twin of [`Command::SetLayerMediaCut`]: aims the layer at
    /// one of its resource's imageCuts (a crop, whose pixels are the cut's
    /// own staged file). Same contract — absent clears, a cut the resource
    /// does not hold is refused.
    #[serde(rename_all = "camelCase")]
    SetLayerImageCut {
        #[serde(rename = "layerID")]
        layer_id: String,
        #[serde(default, rename = "imageCutID")]
        image_cut_id: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    DeleteLayer {
        #[serde(rename = "layerID")]
        layer_id: String,
    },
    /// Moves the layer to `index` in sort order (clamped), then renumbers
    /// every layer's sortIndex sequentially — the order is the truth,
    /// the numbers its spelling.
    #[serde(rename_all = "camelCase")]
    MoveLayer {
        #[serde(rename = "layerID")]
        layer_id: String,
        index: usize,
    },
    /// Inserts or replaces one keyframe — matched by its id, the whole
    /// object crossing as the model's own type so the format, not this
    /// enum, says what a keyframe can carry. The layer's keyframes are
    /// re-sorted by time afterwards: their order is time's spelling.
    #[serde(rename_all = "camelCase")]
    UpsertKeyframe {
        #[serde(rename = "layerID")]
        layer_id: String,
        /// Boxed: a keyframe is the widest thing a command can carry, and
        /// every other variant should not pay its size.
        keyframe: Box<promo_model::ProjectLayerKeyframe>,
    },
    #[serde(rename_all = "camelCase")]
    DeleteKeyframe {
        #[serde(rename = "layerID")]
        layer_id: String,
        #[serde(rename = "keyframeID")]
        keyframe_id: String,
    },
    /// Adds a resource entry. The HOST has already staged the file into
    /// Resources/ (I/O is the host's); the whole entry crosses as the
    /// model's own type. A duplicate id is an error.
    #[serde(rename_all = "camelCase")]
    AddResource {
        resource: Box<promo_model::ProjectResource>,
    },
    /// Adds a layer on TOP of the stack (end of sort order, like every
    /// editor's new layer), renumbering sortIndex sequentially; place it
    /// elsewhere with moveLayer. A duplicate id is an error, and a
    /// resourceID naming no resource in the document is too — a layer
    /// pointing at nothing renders as a hole that looks like a choice.
    #[serde(rename_all = "camelCase")]
    AddLayer {
        layer: Box<promo_model::ProjectLayer>,
    },
    /// Replaces one resource entry wholesale, matched by id — the resource
    /// editors' command, the same whole-object contract the keyframe
    /// carries: what a caption or a trim can hold is the format's say, not
    /// this enum's.
    #[serde(rename_all = "camelCase")]
    UpdateResource {
        resource: Box<promo_model::ProjectResource>,
    },
    /// Removes a resource entry. Refused while any layer references it —
    /// deleting the material out from under a layer leaves a hole that
    /// looks like a choice; delete or repoint the layers first. The FILE in
    /// Resources/ is the host's to clean up.
    #[serde(rename_all = "camelCase")]
    DeleteResource {
        #[serde(rename = "resourceID")]
        resource_id: String,
    },
}

pub struct Document {
    meta: ProjectMetadata,
    version: u64,
    /// Canonical-JSON snapshots taken BEFORE each applied command.
    undo: Vec<String>,
    redo: Vec<String>,
}

impl Document {
    pub fn open(json: &str) -> Result<Self, String> {
        Ok(Self {
            meta: ProjectMetadata::from_json(json).map_err(|e| e.to_string())?,
            version: 0,
            undo: Vec::new(),
            redo: Vec::new(),
        })
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn meta(&self) -> &ProjectMetadata {
        &self.meta
    }

    pub fn to_json(&self) -> Result<String, String> {
        self.meta.to_json().map_err(|e| e.to_string())
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Applies one command. A command that names a layer the document does
    /// not hold is an ERROR, never a no-op — silently editing nothing is
    /// an answer that looks like an answer. A failed command leaves the
    /// document, the history and the version exactly as they were.
    pub fn apply(&mut self, command: &Command) -> Result<(), String> {
        let snapshot = self.to_json()?;
        let mut edited = self.meta.clone();
        Self::run(&mut edited, command)?;
        self.meta = edited;
        self.undo.push(snapshot);
        self.redo.clear();
        self.version += 1;
        Ok(())
    }

    /// Applies several commands as ONE undo step — what a single user
    /// gesture that mints two things (a resource and its layer) deserves.
    /// Atomic: a failure anywhere applies nothing, records nothing.
    pub fn apply_group(&mut self, commands: &[Command]) -> Result<(), String> {
        if commands.is_empty() {
            return Ok(());
        }
        let snapshot = self.to_json()?;
        let mut edited = self.meta.clone();
        for command in commands {
            Self::run(&mut edited, command)?;
        }
        self.meta = edited;
        self.undo.push(snapshot);
        self.redo.clear();
        self.version += 1;
        Ok(())
    }

    pub fn undo(&mut self) -> bool {
        let Some(snapshot) = self.undo.pop() else {
            return false;
        };
        if let Ok(current) = self.to_json() {
            if let Ok(meta) = ProjectMetadata::from_json(&snapshot) {
                self.redo.push(current);
                self.meta = meta;
                self.version += 1;
                return true;
            }
        }
        false
    }

    pub fn redo(&mut self) -> bool {
        let Some(snapshot) = self.redo.pop() else {
            return false;
        };
        if let Ok(current) = self.to_json() {
            if let Ok(meta) = ProjectMetadata::from_json(&snapshot) {
                self.undo.push(current);
                self.meta = meta;
                self.version += 1;
                return true;
            }
        }
        false
    }

    fn run(meta: &mut ProjectMetadata, command: &Command) -> Result<(), String> {
        let layers = meta.layers.get_or_insert_with(Vec::new);
        fn find<'a>(
            layers: &'a mut [promo_model::ProjectLayer],
            id: &str,
        ) -> Result<&'a mut promo_model::ProjectLayer, String> {
            layers
                .iter_mut()
                .find(|l| l.id == id)
                .ok_or_else(|| format!("no layer with id {id}"))
        }
        match command {
            Command::RenameLayer { layer_id, name } => {
                find(layers, layer_id)?.name = name.clone();
            }
            Command::SetLayerEnabled { layer_id, enabled } => {
                find(layers, layer_id)?.is_enabled = *enabled;
            }
            Command::SetLayerTiming {
                layer_id,
                start_time,
                duration,
            } => {
                if !start_time.is_finite() || *start_time < 0.0 {
                    return Err(format!("bad start time {start_time}"));
                }
                if let Some(d) = duration {
                    if !d.is_finite() || *d <= 0.0 {
                        return Err(format!("bad duration {d}"));
                    }
                }
                let layer = find(layers, layer_id)?;
                layer.start_time = *start_time;
                layer.duration = *duration;
            }
            Command::SetLayerAudioFocus { layer_id, focus } => {
                find(layers, layer_id)?.audio_focus = Some(*focus);
            }
            Command::SetLayerMediaCut {
                layer_id,
                media_cut_id,
            } => {
                if let Some(cut_id) = media_cut_id {
                    let resource_id = find(layers, layer_id)?
                        .resource_id
                        .clone()
                        .ok_or_else(|| format!("layer {layer_id} has no resource to cut"))?;
                    let holds_cut = meta
                        .resources
                        .as_deref()
                        .unwrap_or(&[])
                        .iter()
                        .find(|r| r.id == resource_id)
                        .is_some_and(|r| r.media_cuts.iter().any(|c| c.id == *cut_id));
                    if !holds_cut {
                        return Err(format!("resource {resource_id} has no media cut {cut_id}"));
                    }
                }
                let layers = meta.layers.get_or_insert_with(Vec::new);
                find(layers, layer_id)?.media_cut_id = media_cut_id.clone();
            }
            Command::SetLayerImageCut {
                layer_id,
                image_cut_id,
            } => {
                if let Some(cut_id) = image_cut_id {
                    let resource_id = find(layers, layer_id)?
                        .resource_id
                        .clone()
                        .ok_or_else(|| format!("layer {layer_id} has no resource to cut"))?;
                    let holds_cut = meta
                        .resources
                        .as_deref()
                        .unwrap_or(&[])
                        .iter()
                        .find(|r| r.id == resource_id)
                        .is_some_and(|r| r.image_cuts.iter().any(|c| c.id == *cut_id));
                    if !holds_cut {
                        return Err(format!("resource {resource_id} has no image cut {cut_id}"));
                    }
                }
                let layers = meta.layers.get_or_insert_with(Vec::new);
                find(layers, layer_id)?.image_cut_id = image_cut_id.clone();
            }
            Command::DeleteLayer { layer_id } => {
                let before = layers.len();
                layers.retain(|l| l.id != *layer_id);
                if layers.len() == before {
                    return Err(format!("no layer with id {layer_id}"));
                }
            }
            Command::UpsertKeyframe { layer_id, keyframe } => {
                if !keyframe.time.is_finite() || keyframe.time < 0.0 {
                    return Err(format!("bad keyframe time {}", keyframe.time));
                }
                let layer = find(layers, layer_id)?;
                match layer.keyframes.iter_mut().find(|k| k.id == keyframe.id) {
                    Some(existing) => *existing = (**keyframe).clone(),
                    None => layer.keyframes.push((**keyframe).clone()),
                }
                layer.keyframes.sort_by(|a, b| {
                    a.time
                        .partial_cmp(&b.time)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            }
            Command::DeleteKeyframe {
                layer_id,
                keyframe_id,
            } => {
                let layer = find(layers, layer_id)?;
                let before = layer.keyframes.len();
                layer.keyframes.retain(|k| k.id != *keyframe_id);
                if layer.keyframes.len() == before {
                    return Err(format!("no keyframe with id {keyframe_id}"));
                }
            }
            Command::AddResource { resource } => {
                let resources = meta.resources.get_or_insert_with(Vec::new);
                if resources.iter().any(|r| r.id == resource.id) {
                    return Err(format!("a resource with id {} already exists", resource.id));
                }
                resources.push((**resource).clone());
            }
            Command::AddLayer { layer } => {
                if layers.iter().any(|l| l.id == layer.id) {
                    return Err(format!("a layer with id {} already exists", layer.id));
                }
                if let Some(resource_id) = &layer.resource_id {
                    let known = meta
                        .resources
                        .as_deref()
                        .unwrap_or(&[])
                        .iter()
                        .any(|r| r.id == *resource_id);
                    if !known {
                        return Err(format!("layer references unknown resource {resource_id}"));
                    }
                }
                let layers = meta.layers.get_or_insert_with(Vec::new);
                layers.sort_by_key(|l| l.sort_index);
                layers.push((**layer).clone());
                for (i, layer) in layers.iter_mut().enumerate() {
                    layer.sort_index = i as i64;
                }
            }
            Command::UpdateResource { resource } => {
                let resources = meta.resources.get_or_insert_with(Vec::new);
                match resources.iter_mut().find(|r| r.id == resource.id) {
                    Some(existing) => *existing = (**resource).clone(),
                    None => return Err(format!("no resource with id {}", resource.id)),
                }
            }
            Command::DeleteResource { resource_id } => {
                let referenced = layers.iter().any(|l| {
                    l.resource_id.as_deref() == Some(resource_id.as_str())
                        || l.keyframes
                            .iter()
                            .any(|k| k.resource_id.as_deref() == Some(resource_id.as_str()))
                });
                if referenced {
                    return Err(format!(
                        "resource {resource_id} is still referenced by a layer; delete or repoint the layers first"
                    ));
                }
                let resources = meta.resources.get_or_insert_with(Vec::new);
                let before = resources.len();
                resources.retain(|r| r.id != *resource_id);
                if resources.len() == before {
                    return Err(format!("no resource with id {resource_id}"));
                }
            }
            Command::MoveLayer { layer_id, index } => {
                layers.sort_by_key(|l| l.sort_index);
                let from = layers
                    .iter()
                    .position(|l| l.id == *layer_id)
                    .ok_or_else(|| format!("no layer with id {layer_id}"))?;
                let layer = layers.remove(from);
                let to = (*index).min(layers.len());
                layers.insert(to, layer);
                for (i, layer) in layers.iter_mut().enumerate() {
                    layer.sort_index = i as i64;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> Document {
        let spec: crate::author::AuthorSpec = serde_json::from_value(serde_json::json!({
            "name": "Doc", "createdAt": 1000.0,
            "slides": [
                {"filename": "a.png", "kind": "image", "pixelWidth": 100.0, "pixelHeight": 100.0},
                {"filename": "b.png", "kind": "image", "pixelWidth": 100.0, "pixelHeight": 100.0},
            ],
        }))
        .unwrap();
        Document::open(&crate::author::author(&spec).unwrap()).unwrap()
    }

    fn layer_id(doc: &Document, index: usize) -> String {
        doc.meta().layers.as_ref().unwrap()[index].id.clone()
    }

    fn command(value: serde_json::Value) -> Command {
        serde_json::from_value(value).unwrap()
    }

    /// The plan's own undo gate: apply N commands, undo N, and the JSON is
    /// byte-identical to where it started.
    #[test]
    fn n_commands_undone_return_the_exact_document() {
        let mut doc = doc();
        let original = doc.to_json().unwrap();
        let slide = layer_id(&doc, 1);
        for cmd in [
            serde_json::json!({"kind": "renameLayer", "layerID": slide, "name": "First"}),
            serde_json::json!({"kind": "setLayerEnabled", "layerID": slide, "enabled": false}),
            serde_json::json!({"kind": "setLayerTiming", "layerID": slide,
                               "startTime": 1.5, "duration": 2.0}),
            serde_json::json!({"kind": "moveLayer", "layerID": slide, "index": 2}),
        ] {
            doc.apply(&command(cmd)).unwrap();
        }
        assert_eq!(doc.version(), 4);
        for _ in 0..4 {
            assert!(doc.undo());
        }
        assert_eq!(doc.to_json().unwrap(), original);
        assert!(!doc.can_undo());
        assert!(doc.can_redo());
    }

    #[test]
    fn redo_replays_and_a_new_command_clears_it() {
        let mut doc = doc();
        let slide = layer_id(&doc, 1);
        doc.apply(&command(serde_json::json!(
            {"kind": "renameLayer", "layerID": slide, "name": "One"})))
            .unwrap();
        let renamed = doc.to_json().unwrap();
        assert!(doc.undo());
        assert!(doc.redo());
        assert_eq!(doc.to_json().unwrap(), renamed);
        assert!(doc.undo());
        doc.apply(&command(serde_json::json!(
            {"kind": "renameLayer", "layerID": slide, "name": "Two"})))
            .unwrap();
        assert!(
            !doc.can_redo(),
            "a new command forks history; stale redo must die"
        );
    }

    /// A command that misses is an error and a no-op — the document,
    /// version and history untouched.
    #[test]
    fn a_command_naming_no_layer_changes_nothing() {
        let mut doc = doc();
        let before = doc.to_json().unwrap();
        let err = doc.apply(&command(serde_json::json!(
            {"kind": "renameLayer", "layerID": "GHOST", "name": "x"})));
        assert!(err.is_err());
        assert_eq!(doc.version(), 0);
        assert!(!doc.can_undo());
        assert_eq!(doc.to_json().unwrap(), before);
    }

    #[test]
    fn move_renumbers_every_sort_index() {
        let mut doc = doc();
        let background = layer_id(&doc, 0);
        doc.apply(&command(serde_json::json!(
            {"kind": "moveLayer", "layerID": background, "index": 99})))
            .unwrap();
        let layers = doc.meta().layers.as_ref().unwrap();
        let mut sorted: Vec<i64> = layers.iter().map(|l| l.sort_index).collect();
        sorted.sort();
        assert_eq!(sorted, vec![0, 1, 2]);
        let last = layers.iter().max_by_key(|l| l.sort_index).unwrap();
        assert_eq!(last.id, background, "clamped to the end, not dropped");
    }

    /// The keyframe group: an upsert with a fresh id inserts, the same id
    /// replaces, order follows time, delete removes — and the whole dance
    /// undoes to the exact original.
    #[test]
    fn keyframes_upsert_sorted_edit_in_place_and_delete() {
        let mut doc = doc();
        let original = doc.to_json().unwrap();
        let slide = layer_id(&doc, 1);
        doc.apply(&command(serde_json::json!({
            "kind": "upsertKeyframe", "layerID": slide,
            "keyframe": {"id": "K2", "time": 2.0, "transitionDuration": 0.5,
                          "zoom": 2.0, "easing": "easeInOut"},
        })))
        .unwrap();
        doc.apply(&command(serde_json::json!({
            "kind": "upsertKeyframe", "layerID": slide,
            "keyframe": {"id": "K1", "time": 1.0, "transitionDuration": 0.0,
                          "opacity": 0.5},
        })))
        .unwrap();
        {
            let layer = &doc.meta().layers.as_ref().unwrap()[1];
            let times: Vec<f64> = layer.keyframes.iter().map(|k| k.time).collect();
            assert!(
                times.windows(2).all(|w| w[0] <= w[1]),
                "sorted by time: {times:?}"
            );
            assert_eq!(layer.keyframes.len(), 3, "the authored keyframe plus two");
        }
        // Same id, new values: replaced, not duplicated.
        doc.apply(&command(serde_json::json!({
            "kind": "upsertKeyframe", "layerID": slide,
            "keyframe": {"id": "K2", "time": 2.5, "transitionDuration": 0.5,
                          "zoom": 3.0},
        })))
        .unwrap();
        {
            let layer = &doc.meta().layers.as_ref().unwrap()[1];
            assert_eq!(layer.keyframes.len(), 3);
            let k2 = layer.keyframes.iter().find(|k| k.id == "K2").unwrap();
            assert_eq!(k2.zoom, Some(3.0));
        }
        doc.apply(&command(serde_json::json!({
            "kind": "deleteKeyframe", "layerID": slide, "keyframeID": "K1",
        })))
        .unwrap();
        assert!(doc
            .apply(&command(serde_json::json!({
                "kind": "deleteKeyframe", "layerID": slide, "keyframeID": "K1"})))
            .is_err());
        assert!(doc
            .apply(&command(serde_json::json!({
                "kind": "upsertKeyframe", "layerID": slide,
                "keyframe": {"id": "KX", "time": -1.0, "transitionDuration": 0.0}})))
            .is_err());
        for _ in 0..4 {
            assert!(doc.undo());
        }
        assert_eq!(doc.to_json().unwrap(), original);
    }

    /// One gesture, one undo step — and a group with a bad member applies
    /// none of its good ones.
    #[test]
    fn a_group_is_one_undo_step_and_fails_whole() {
        let mut doc = doc();
        let original = doc.to_json().unwrap();
        let slide = layer_id(&doc, 1);

        doc.apply_group(&[
            command(serde_json::json!({
                "kind": "renameLayer", "layerID": slide, "name": "Grouped"})),
            command(serde_json::json!({
                "kind": "setLayerEnabled", "layerID": slide, "enabled": false})),
        ])
        .unwrap();
        assert_eq!(doc.version(), 1, "one gesture, one version bump");
        assert!(doc.undo());
        assert_eq!(
            doc.to_json().unwrap(),
            original,
            "one undo unwinds the pair"
        );

        let err = doc.apply_group(&[
            command(serde_json::json!({
                "kind": "renameLayer", "layerID": slide, "name": "Half"})),
            command(serde_json::json!({
                "kind": "renameLayer", "layerID": "GHOST", "name": "x"})),
        ]);
        assert!(err.is_err());
        assert_eq!(
            doc.to_json().unwrap(),
            original,
            "a failing group applies nothing at all"
        );
        assert!(doc.can_redo(), "the undone pair is still redoable");
    }

    /// The add group: a resource arrives whole, a layer lands on top of
    /// the stack, a layer naming an unknown resource is refused, and the
    /// pair undoes to the exact original.
    #[test]
    fn adds_land_on_top_and_refuse_dangling_references() {
        let mut doc = doc();
        let original = doc.to_json().unwrap();

        // A caption layer with no resource must be refused too? No — a
        // caption NEEDS its resource; but a layer with NO resourceID at
        // all (a background) is fine. First: dangling reference refused.
        assert!(doc
            .apply(&command(serde_json::json!({
                "kind": "addLayer",
                "layer": {"id": "L-NEW", "name": "Cap", "sortIndex": 0,
                          "kind": "caption", "isEnabled": true,
                          "startTime": 0.0, "resourceID": "R-GHOST",
                          "keyframes": []}})))
            .is_err());

        doc.apply(&command(serde_json::json!({
            "kind": "addResource",
            "resource": {"id": "R-CAP", "kind": "caption", "filename": "",
                          "displayName": "Words", "addedAt": 0.0,
                          "captionText": "Hello"},
        })))
        .unwrap();
        // Same id again: refused.
        assert!(doc
            .apply(&command(serde_json::json!({
                "kind": "addResource",
                "resource": {"id": "R-CAP", "kind": "caption", "filename": "",
                              "displayName": "Twice", "addedAt": 0.0}})))
            .is_err());

        doc.apply(&command(serde_json::json!({
            "kind": "addLayer",
            "layer": {"id": "L-CAP", "name": "Cap", "sortIndex": 0,
                      "kind": "caption", "isEnabled": true,
                      "startTime": 1.0, "duration": 3.0,
                      "resourceID": "R-CAP", "keyframes": []},
        })))
        .unwrap();
        {
            let layers = doc.meta().layers.as_ref().unwrap();
            let top = layers.iter().max_by_key(|l| l.sort_index).unwrap();
            assert_eq!(top.id, "L-CAP", "a new layer lands on top of the stack");
            let indices: Vec<i64> = {
                let mut sorted: Vec<i64> = layers.iter().map(|l| l.sort_index).collect();
                sorted.sort();
                sorted
            };
            assert_eq!(indices, vec![0, 1, 2, 3], "renumbered sequentially");
        }
        assert!(doc.undo());
        assert!(doc.undo());
        assert_eq!(doc.to_json().unwrap(), original);
    }

    /// The resource editors' pair: update replaces wholesale, delete is
    /// refused while a layer still points at the material — and the whole
    /// dance undoes to the original.
    #[test]
    fn resources_update_wholesale_and_refuse_referenced_deletes() {
        let mut doc = doc();
        let original = doc.to_json().unwrap();
        let resource_id = doc.meta().resources.as_ref().unwrap()[0].id.clone();
        let layer_id = layer_id(&doc, 1);

        // Update: the whole object crosses; a new displayName arrives.
        let mut edited = doc.meta().resources.as_ref().unwrap()[0].clone();
        edited.display_name = "Retitled".into();
        doc.apply(&Command::UpdateResource {
            resource: Box::new(edited),
        })
        .unwrap();
        assert_eq!(
            doc.meta().resources.as_ref().unwrap()[0].display_name,
            "Retitled"
        );
        // An unknown id is an error, not an insert — adds have their own
        // command, and an editor must never create what it meant to edit.
        let mut ghost = doc.meta().resources.as_ref().unwrap()[0].clone();
        ghost.id = "R-GHOST".into();
        assert!(doc
            .apply(&Command::UpdateResource {
                resource: Box::new(ghost)
            })
            .is_err());

        // Delete: refused while referenced, allowed once the layer is gone.
        assert!(doc
            .apply(&command(serde_json::json!({
                "kind": "deleteResource", "resourceID": resource_id})))
            .is_err());
        doc.apply(&command(serde_json::json!({
            "kind": "deleteLayer", "layerID": layer_id})))
            .unwrap();
        doc.apply(&command(serde_json::json!({
            "kind": "deleteResource", "resourceID": resource_id})))
            .unwrap();
        for _ in 0..3 {
            assert!(doc.undo());
        }
        assert_eq!(doc.to_json().unwrap(), original);
    }

    /// The cuts pointer: a layer aims at one of its resource's mediaCuts,
    /// clears back to the resource's own trim, and can never be aimed at a
    /// cut that is not there — render-time degradation covers a cut that
    /// VANISHES, not an editor minting a dangling pointer on purpose.
    #[test]
    fn set_layer_media_cut_points_clears_and_refuses_ghosts() {
        let mut doc = doc();
        let original = doc.to_json().unwrap();
        doc.apply_group(&[
            command(serde_json::json!({"kind": "addResource", "resource": {
                "id": "V-CUT", "kind": "video", "filename": "clip.mp4",
                "displayName": "Clip", "addedAt": 0,
                "mediaCuts": [{"id": "C1", "name": "Intro",
                                "trimStart": 1.0, "trimEnd": 3.0}]}})),
            command(serde_json::json!({"kind": "addLayer", "layer": {
                "id": "L-CUT", "name": "Clip", "sortIndex": 99, "kind": "video",
                "isEnabled": true, "startTime": 0.0, "duration": 2.0,
                "resourceID": "V-CUT", "keyframes": []}})),
        ])
        .unwrap();

        doc.apply(&command(serde_json::json!({
            "kind": "setLayerMediaCut", "layerID": "L-CUT", "mediaCutID": "C1"})))
            .unwrap();
        let cut_of = |doc: &Document| doc.meta().layers.as_ref().unwrap()[3].media_cut_id.clone();
        assert_eq!(cut_of(&doc), Some("C1".into()));

        // Absent mediaCutID clears the pointer.
        doc.apply(&command(serde_json::json!({
            "kind": "setLayerMediaCut", "layerID": "L-CUT"})))
            .unwrap();
        assert_eq!(cut_of(&doc), None);

        // A cut the resource does not hold, and a layer with no resource
        // to cut, are both errors and no-ops.
        assert!(doc
            .apply(&command(serde_json::json!({
                "kind": "setLayerMediaCut", "layerID": "L-CUT",
                "mediaCutID": "GHOST"})))
            .is_err());
        let background = layer_id(&doc, 0);
        assert!(doc
            .apply(&command(serde_json::json!({
                "kind": "setLayerMediaCut", "layerID": background,
                "mediaCutID": "C1"})))
            .is_err());

        for _ in 0..3 {
            assert!(doc.undo());
        }
        assert_eq!(doc.to_json().unwrap(), original);
    }

    /// The image twin: same pointer contract as the media cut, against
    /// imageCuts (a crop with staged pixels) instead of a time range.
    #[test]
    fn set_layer_image_cut_points_clears_and_refuses_ghosts() {
        let mut doc = doc();
        let slide = layer_id(&doc, 1);
        let resource_id = doc.meta().layers.as_ref().unwrap()[1]
            .resource_id
            .clone()
            .expect("wizard slides show a resource");
        let mut with_cut = doc
            .meta()
            .resources
            .as_ref()
            .unwrap()
            .iter()
            .find(|r| r.id == resource_id)
            .unwrap()
            .clone();
        with_cut.image_cuts = vec![serde_json::from_value(serde_json::json!({
            "id": "IC1", "rect": [[0.1, 0.1], [0.5, 0.5]],
            "filename": "crop.png", "createdAt": 0
        }))
        .unwrap()];
        doc.apply(&Command::UpdateResource {
            resource: Box::new(with_cut),
        })
        .unwrap();

        doc.apply(&command(serde_json::json!({
            "kind": "setLayerImageCut", "layerID": slide, "imageCutID": "IC1"})))
            .unwrap();
        assert_eq!(
            doc.meta().layers.as_ref().unwrap()[1].image_cut_id,
            Some("IC1".into())
        );
        doc.apply(&command(serde_json::json!({
            "kind": "setLayerImageCut", "layerID": slide})))
            .unwrap();
        assert_eq!(doc.meta().layers.as_ref().unwrap()[1].image_cut_id, None);
        assert!(doc
            .apply(&command(serde_json::json!({
                "kind": "setLayerImageCut", "layerID": slide,
                "imageCutID": "GHOST"})))
            .is_err());
    }

    #[test]
    fn delete_and_bad_timing_hold_the_line() {
        let mut doc = doc();
        let slide = layer_id(&doc, 1);
        assert!(doc
            .apply(&command(serde_json::json!(
                {"kind": "setLayerTiming", "layerID": slide, "startTime": -1.0})))
            .is_err());
        doc.apply(&command(serde_json::json!(
            {"kind": "deleteLayer", "layerID": slide})))
            .unwrap();
        assert_eq!(doc.meta().layers.as_ref().unwrap().len(), 2);
        assert!(doc
            .apply(&command(serde_json::json!(
                {"kind": "deleteLayer", "layerID": slide})))
            .is_err());
    }
}
