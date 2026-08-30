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
