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
/// Served to agents as a generated JSON Schema (`command_schema`), so the
/// enum IS the tool contract: a variant added here reaches every front end
/// and every agent the day it lands.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
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
    /// this enum's. When the playable length of a video or audio resource
    /// changes, the layers that played all of it follow the new length, a
    /// layer you shortened yourself keeps its length until the trim caps
    /// it, looped and beyond-end layers are left alone, and the background
    /// stretches over the content.
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
    /// Re-points a layer at a resource — the "repoint the layers first"
    /// that deleteResource's refusal asks for, and the way an existing
    /// background layer gains its plate. The resource must exist and its
    /// kind must MATCH the layer's (an editor must not aim a video layer
    /// at a caption). Absent resourceID clears the pointer, and only on a
    /// BACKGROUND layer — a background without a plate paints the
    /// settings ground, but any other kind without a resource renders as
    /// a hole that looks like a choice.
    #[serde(rename_all = "camelCase")]
    SetLayerResource {
        #[serde(rename = "layerID")]
        layer_id: String,
        #[serde(default, rename = "resourceID")]
        resource_id: Option<String>,
    },
    /// Replaces the composition settings wholesale — the settings form's
    /// command, the same whole-object contract every resource editor
    /// uses: what settings can hold is the format's say, not this enum's.
    /// A paletteResourceID naming no palette resource is refused (an
    /// editor must not mint a dangling theme pointer), and when one IS
    /// selected the materialized palette is refreshed from the resource
    /// after the replace, so the copy cannot drift from the authority.
    #[serde(rename_all = "camelCase")]
    UpdateSettings {
        settings: Box<promo_model::CompositionSettings>,
    },
    /// Renames the project. Blank is refused — a nameless project is a
    /// hole in every list that shows one.
    #[serde(rename_all = "camelCase")]
    RenameProject { name: String },
    /// Selects (or, absent, deselects) the palette resource — the THEME —
    /// the project follows. Selection is the one moment the theme's whole
    /// look lands: the entries materialize into `settings.palette`,
    /// factory-default settings colours are pointed at the roles the theme
    /// states, and its `captionStyle` typography is folded over fields
    /// nobody moved off the default (see [`crate::theme`] for the ported
    /// Mac rules). Deselecting clears only the pointer — the materialized
    /// copy stays, so nothing on screen changes.
    #[serde(rename_all = "camelCase")]
    SelectPalette {
        #[serde(default, rename = "paletteResourceID")]
        palette_resource_id: Option<String>,
    },
    /// The long tail through one door: a JSON MERGE PATCH (RFC 7386) over
    /// the layer's wire form, re-parsed by the format — so `{"transitionIn":
    /// {"kind": "wipe", "duration": 0.5}}` adds a wipe, `{"fadeIn": null}`
    /// removes a fade, and a value the format refuses is refused here with
    /// the format's own reason. Only what the patch names changes (D5's
    /// update discipline). `keyframes` are NOT patchable — `upsertKeyframe`
    /// and `deleteKeyframe` own them, and a merge would clobber hand-made
    /// motion — nor are `id`, `kind` and `sortIndex` (`moveLayer` owns
    /// order).
    #[serde(rename_all = "camelCase")]
    UpdateLayer {
        #[serde(rename = "layerID")]
        layer_id: String,
        patch: serde_json::Value,
    },
    /// The merge-patch twin of [`Command::UpdateResource`]: trims, speed,
    /// loop, a frame, speech text — only the fields named change, and the
    /// result passes through `updateResource`'s own validation (a selected
    /// theme's materialized copy refreshes). `id` and `kind` are not
    /// patchable.
    #[serde(rename_all = "camelCase")]
    PatchResource {
        #[serde(rename = "resourceID")]
        resource_id: String,
        patch: serde_json::Value,
    },
    /// The merge-patch twin of [`Command::UpdateSettings`]: one field of
    /// the composition — a canvas size, a caption default, a palette entry
    /// — without restating the rest. Delegates to `updateSettings`, so a
    /// dangling `paletteResourceID` is refused the same way.
    #[serde(rename_all = "camelCase")]
    PatchSettings { patch: serde_json::Value },
    /// The project's markers and chapters (rung 20), replaced whole. The
    /// list is short and a ruler edit — add, drag, rename, delete — is one
    /// step either way; a drag's successive steps merge like any other
    /// same-kind burst. Sorted by time on the way in. Refused for a time
    /// that is negative or not a number, and for an empty or repeated id.
    #[serde(rename_all = "camelCase")]
    SetMarkers { markers: Vec<promo_model::Marker> },
    /// A command addressed INSIDE a composition resource: the same
    /// command, run on the composition's layers as if they were the
    /// document's — with the project's resources and settings, the
    /// composition's canvas — then written back into the resource. One
    /// document, one history; the change log names the resource. Refused
    /// if the edit would make the composition contain itself or nest too
    /// deep.
    #[serde(rename_all = "camelCase")]
    InComposition {
        #[serde(rename = "resourceID")]
        resource_id: String,
        command: Box<Command>,
    },
}

/// RFC 7386: objects merge key by key, `null` deletes, anything else
/// replaces.
/// The Mac editor's `replaceResource` rule, ported: when a video or audio
/// resource's PLAYBACK length changes (a trim, a held frame, a speed), the
/// layers that were playing the whole of it follow the new length, and a
/// layer already shortened by hand is only clamped if it now overruns.
/// A layer on a cut plays the cut, not the resource; a looped layer, or
/// one with a beyond-end policy, is stretched on purpose and keeps its
/// duration. Then the background keeps covering the content. Without
/// this, an agent's `updateResource` left layers playing past a trim the
/// app would have followed.
fn retime_layers_for_resource(
    meta: &mut ProjectMetadata,
    previous: &promo_model::ProjectResource,
    updated: &promo_model::ProjectResource,
) {
    use promo_model::{ProjectLayerKind, ProjectResourceKind};
    let (old_len, new_len, kind) = match (previous.kind, updated.kind) {
        (ProjectResourceKind::Video, ProjectResourceKind::Video) => (
            promo_timeline::effective_video_playback_duration(previous),
            promo_timeline::effective_video_playback_duration(updated),
            ProjectLayerKind::Video,
        ),
        (ProjectResourceKind::Audio, ProjectResourceKind::Audio) => (
            promo_timeline::effective_trimmed_media_duration(previous),
            promo_timeline::effective_trimmed_media_duration(updated),
            ProjectLayerKind::Audio,
        ),
        _ => return,
    };
    if old_len <= 0.0 || (new_len - old_len).abs() <= 0.000_1 {
        return;
    }
    let layers = meta.layers.get_or_insert_with(Vec::new);
    for layer in layers.iter_mut() {
        if layer.resource_id.as_deref() != Some(updated.id.as_str()) || layer.kind != kind {
            continue;
        }
        if layer.media_cut_id.is_some() {
            continue;
        }
        if layer.beyond_end.is_some() || updated.is_looped() {
            continue;
        }
        let current = layer.duration.unwrap_or(old_len);
        if current >= old_len - 0.05 || current > new_len {
            layer.duration = Some(new_len.max(0.1));
        }
    }
    extend_background_over_content(meta);
}

/// The Mac editor's `extendBackgroundOverContent`: every background layer
/// lasts at least as long as the content above it.
fn extend_background_over_content(meta: &mut ProjectMetadata) {
    let Some(layers) = meta.layers.as_mut() else {
        return;
    };
    let content_end = layers
        .iter()
        .filter(|l| l.kind != promo_model::ProjectLayerKind::Background)
        .fold(0.1f64, |end, l| {
            end.max(l.start_time + l.duration.unwrap_or(0.0))
        });
    for layer in layers
        .iter_mut()
        .filter(|l| l.kind == promo_model::ProjectLayerKind::Background)
    {
        layer.duration = Some(layer.duration.unwrap_or(0.0).max(content_end));
    }
}

fn merge_patch(target: &mut serde_json::Value, patch: &serde_json::Value) {
    match patch {
        serde_json::Value::Object(entries) => {
            if !target.is_object() {
                *target = serde_json::Value::Object(Default::default());
            }
            let map = target.as_object_mut().expect("object");
            for (key, value) in entries {
                if value.is_null() {
                    map.remove(key);
                } else {
                    merge_patch(
                        map.entry(key.clone()).or_insert(serde_json::Value::Null),
                        value,
                    );
                }
            }
        }
        other => *target = other.clone(),
    }
}

/// Refuses a patch that is not an object or that names a protected key.
fn checked_patch<'a>(
    patch: &'a serde_json::Value,
    protected: &[(&str, &str)],
) -> Result<&'a serde_json::Map<String, serde_json::Value>, String> {
    let map = patch
        .as_object()
        .ok_or("patch must be a JSON object of the fields to change")?;
    for (key, why) in protected {
        if map.contains_key(*key) {
            return Err(format!("`{key}` is not patchable — {why}"));
        }
    }
    Ok(map)
}

/// The Command enum as a JSON Schema, generated from the type itself — the
/// tool contract agents fill against (`promo_apply`), the same way
/// `promo_schema_types` serves the format.
pub fn command_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(Command)).unwrap_or(serde_json::Value::Null)
}

/// What one version bump touched — the narrow thing a front end observes,
/// so a rename redraws one row and not every layer (EDITOR-PLAN §8: decided
/// before any command lands). Computed by DIFFING the document before and
/// after, never by trusting a command's word for what it changed: undo,
/// redo and a group of commands all answer the same way.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Changes {
    /// Layers whose JSON differs (edited, added or removed), by id.
    pub layers: Vec<String>,
    /// Resources whose JSON differs, by id.
    pub resources: Vec<String>,
    /// The layer ORDER changed (a move, an add, a delete).
    pub order: bool,
    /// `compositionSettings` changed.
    pub settings: bool,
    /// Something outside layers, resources and settings (the name, the
    /// legacy fields) changed.
    pub project: bool,
}

impl Changes {
    fn union(&mut self, other: &Changes) {
        for id in &other.layers {
            if !self.layers.contains(id) {
                self.layers.push(id.clone());
            }
        }
        for id in &other.resources {
            if !self.resources.contains(id) {
                self.resources.push(id.clone());
            }
        }
        self.order |= other.order;
        self.settings |= other.settings;
        self.project |= other.project;
    }

    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
            && self.resources.is_empty()
            && !self.order
            && !self.settings
            && !self.project
    }
}

/// How many version bumps the change log remembers before an old
/// `changes_since` answers "everything".
const CHANGE_LOG_DEPTH: usize = 256;

pub struct Document {
    meta: ProjectMetadata,
    version: u64,
    /// Canonical-JSON snapshots taken BEFORE each applied command.
    undo: Vec<String>,
    redo: Vec<String>,
    /// (version reached, what that bump touched), newest last.
    log: Vec<(u64, Changes)>,
    /// Per-layer revision: bumped whenever a version touches the layer.
    layer_revisions: std::collections::HashMap<String, u64>,
    /// The file the apps last read this project from (opened, or adopted
    /// after another writer changed it): what it holds unread is carried
    /// into every replace and save for what the document has none of —
    /// the entities that file brought, before the apps' copy hands them in
    /// (review 2026-09-27, P2-45).
    unread_source: Option<ProjectMetadata>,
    /// That file as written, for what no struct keeps: a key at any depth
    /// the format does not read (`promo_model::unread::keep_unread`).
    unread_source_json: Option<serde_json::Value>,
}

impl Document {
    pub fn open(json: &str) -> Result<Self, String> {
        // A flat stage (rung 30) is read forever and written no more: the
        // document works on the one-layer form, so the next save writes it.
        Ok(Self {
            meta: ProjectMetadata::from_json(json)
                .map_err(|e| e.to_string())?
                .lifted(),
            version: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            log: Vec::new(),
            layer_revisions: std::collections::HashMap::new(),
            unread_source: None,
            unread_source_json: None,
        })
    }

    /// Everything that changed after `version` — the union of every bump
    /// since. A `version` older than the log remembers, or from another
    /// document, answers "everything": every layer and resource named,
    /// order, settings and project all set — the honest answer when the
    /// narrow one cannot be known.
    pub fn changes_since(&self, version: u64) -> Changes {
        if version >= self.version {
            return Changes::default();
        }
        let floor = self
            .log
            .first()
            .map(|(v, _)| *v)
            .unwrap_or(self.version + 1);
        if version + 1 < floor {
            return self.everything();
        }
        let mut union = Changes::default();
        for (v, changes) in &self.log {
            if *v > version {
                union.union(changes);
            }
        }
        union
    }

    /// The revision a layer is at: 0 until something touches it.
    pub fn layer_revision(&self, layer_id: &str) -> u64 {
        self.layer_revisions.get(layer_id).copied().unwrap_or(0)
    }

    fn everything(&self) -> Changes {
        Changes {
            layers: self
                .meta
                .layers
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(|l| l.id.clone())
                .collect(),
            resources: self
                .meta
                .resources
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(|r| r.id.clone())
                .collect(),
            order: true,
            settings: true,
            project: true,
        }
    }

    /// Diffs two documents entity by entity.
    fn diff(before: &ProjectMetadata, after: &ProjectMetadata) -> Changes {
        use std::collections::BTreeMap;
        let layers_of = |m: &ProjectMetadata| -> BTreeMap<String, String> {
            m.layers
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(|l| (l.id.clone(), serde_json::to_string(l).unwrap_or_default()))
                .collect()
        };
        let resources_of = |m: &ProjectMetadata| -> BTreeMap<String, String> {
            m.resources
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(|r| (r.id.clone(), serde_json::to_string(r).unwrap_or_default()))
                .collect()
        };
        let (lb, la) = (layers_of(before), layers_of(after));
        let (rb, ra) = (resources_of(before), resources_of(after));
        let mut changes = Changes::default();
        for id in lb.keys().chain(la.keys()) {
            if lb.get(id) != la.get(id) && !changes.layers.contains(id) {
                changes.layers.push(id.clone());
            }
        }
        for id in rb.keys().chain(ra.keys()) {
            if rb.get(id) != ra.get(id) && !changes.resources.contains(id) {
                changes.resources.push(id.clone());
            }
        }
        let order = |m: &ProjectMetadata| -> Vec<String> {
            m.layers
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(|l| l.id.clone())
                .collect()
        };
        changes.order = order(before) != order(after);
        changes.settings = serde_json::to_string(&before.composition_settings).ok()
            != serde_json::to_string(&after.composition_settings).ok();
        // Everything else: compare the documents with layers, resources
        // and settings blanked.
        let rest = |m: &ProjectMetadata| -> String {
            let mut copy = m.clone();
            copy.layers = None;
            copy.resources = None;
            copy.composition_settings = Default::default();
            copy.to_json().unwrap_or_default()
        };
        changes.project = rest(before) != rest(after);
        changes
    }

    /// Records what a bump from `before` to the current document touched.
    fn note(&mut self, before: &ProjectMetadata) {
        let changes = Self::diff(before, &self.meta);
        for id in &changes.layers {
            *self.layer_revisions.entry(id.clone()).or_insert(0) += 1;
        }
        self.log.push((self.version, changes));
        if self.log.len() > CHANGE_LOG_DEPTH {
            self.log.remove(0);
        }
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
        // Anchored layers follow what moved — the Mac reconciles after
        // every edit through the same resolver; a document the core owns
        // must not wait for the app to do it.
        promo_timeline::resolve_attachments(&mut edited);
        let before = std::mem::replace(&mut self.meta, edited);
        self.undo.push(snapshot);
        self.redo.clear();
        self.version += 1;
        self.note(&before);
        Ok(())
    }

    /// Applies several commands as ONE undo step — what a single user
    /// gesture that mints two things (a resource and its layer) deserves.
    /// Atomic: a failure anywhere applies nothing, records nothing.
    pub fn apply_group(&mut self, commands: &[Command]) -> Result<(), String> {
        self.apply_group_merging(commands, false)
    }

    /// `apply_group`, and when `merge_with_last` the step JOINS the
    /// previous undo step instead of starting one: a drag that writes
    /// thirty times is one Ctrl+Z, as the apps' gesture-coalescing rule
    /// has it — the caller decides the window and the kind, the document
    /// only keeps a single boundary. With nothing to join it is a plain
    /// step. The change log still sees every apply, so a reader that
    /// follows the log is not affected by how undo groups them.
    pub fn apply_group_merging(
        &mut self,
        commands: &[Command],
        merge_with_last: bool,
    ) -> Result<(), String> {
        if commands.is_empty() {
            return Ok(());
        }
        let snapshot = self.to_json()?;
        let mut edited = self.meta.clone();
        for command in commands {
            Self::run(&mut edited, command)?;
        }
        promo_timeline::resolve_attachments(&mut edited);
        let before = std::mem::replace(&mut self.meta, edited);
        if !merge_with_last || self.undo.is_empty() {
            self.undo.push(snapshot);
        }
        self.redo.clear();
        self.version += 1;
        self.note(&before);
        Ok(())
    }

    /// Replaces the whole document as ONE undo step — the step an edit
    /// takes when it was not spoken as commands: a file adopted from disk
    /// after an assistant wrote it, a legacy path that wrote the Swift
    /// model directly while both writers exist. Validated by the format's
    /// own parser; the change log names what differs, so a reader follows
    /// it like any other step. Identical content is not a step.
    ///
    /// The one writer that replaces is the apps' Swift model, which cannot
    /// read everything the document holds: what it dropped — keys the
    /// format carries unread, enum values it rewrote as their fallback — is
    /// carried over from the document (review 2026-09-27, P2-45).
    pub fn replace(&mut self, json: &str) -> Result<bool, String> {
        let meta = ProjectMetadata::from_json(json)
            .map_err(|e| e.to_string())?
            .lifted();
        self.take(meta)
    }

    /// `replace` for a writer that knows what it changed: the top-level
    /// layers and resources `json` names swap in whole, matched by id, and
    /// its `compositionSettings` and `updatedAt` when present — carried
    /// like any replace, everything else as the document holds it. The
    /// apps' sync after an edit, without encoding and parsing the whole
    /// project each time (review 2026-09-27, P3-46). A layer or resource
    /// the document does not have is refused: that is a structural change,
    /// and the writer replaces the whole document instead.
    pub fn replace_parts(&mut self, json: &str) -> Result<bool, String> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Parts {
            #[serde(default)]
            layers: Vec<promo_model::ProjectLayer>,
            #[serde(default)]
            resources: Vec<promo_model::ProjectResource>,
            composition_settings: Option<promo_model::CompositionSettings>,
            updated_at: Option<f64>,
        }
        let parts: Parts = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let mut meta = self.meta.clone();
        for layer in parts.layers {
            let slot = meta
                .layers
                .iter_mut()
                .flatten()
                .find(|l| l.id == layer.id)
                .ok_or_else(|| format!("no layer {} to replace", layer.id))?;
            *slot = layer;
        }
        for resource in parts.resources {
            let slot = meta
                .resources
                .iter_mut()
                .flatten()
                .find(|r| r.id == resource.id)
                .ok_or_else(|| format!("no resource {} to replace", resource.id))?;
            *slot = resource;
        }
        if let Some(settings) = parts.composition_settings {
            meta.composition_settings = settings;
        }
        if parts.updated_at.is_some() {
            meta.updated_at = parts.updated_at;
        }
        self.take(meta.lifted())
    }

    /// The lossy writer's new copy as the document: carried, and a step
    /// when it changed anything but the save's timestamp.
    fn take(&mut self, mut meta: ProjectMetadata) -> Result<bool, String> {
        self.carry_unread_into(&mut meta);
        if meta == self.meta {
            return Ok(false);
        }
        // A save stamps `updatedAt`; that is not an edit. Taken without a
        // step, or ⌘Z after a save undid nothing anyone could see — the
        // timestamp — and the person pressed it again.
        let mut same_but_time = meta.clone();
        same_but_time.updated_at = self.meta.updated_at;
        if same_but_time == self.meta {
            self.meta.updated_at = meta.updated_at;
            return Ok(false);
        }
        let snapshot = self.to_json()?;
        let before = std::mem::replace(&mut self.meta, meta);
        self.undo.push(snapshot);
        self.redo.clear();
        self.version += 1;
        self.note(&before);
        Ok(true)
    }

    /// Takes the unread content of `json` — the file the apps just read the
    /// project from: the one it was opened from, or one another writer
    /// changed — as the document's, without a step: nothing the person did
    /// changed. The file is the latest word on what the apps cannot see, so
    /// a key it removed goes and a value it changed is its value; what the
    /// apps read stays theirs. The file is kept as the source for entities
    /// the document has not got yet — only when it holds something this
    /// build does not read: a file the format reads whole has nothing to
    /// put back, and a library of such projects should not hold each one
    /// twice more. True when the document changed.
    pub fn absorb_unread(&mut self, json: &str) -> Result<bool, String> {
        let source = ProjectMetadata::from_json(json)
            .map_err(|e| e.to_string())?
            .lifted();
        let before = self.meta.clone();
        self.meta.mirror_unread_from(&source);
        let value: Option<serde_json::Value> = serde_json::from_str(json).ok();
        if value
            .as_ref()
            .is_some_and(|v| promo_model::unread::first_unread(v).is_some())
        {
            self.unread_source = Some(source);
            self.unread_source_json = value;
        } else {
            self.unread_source = None;
            self.unread_source_json = None;
        }
        Ok(self.meta != before)
    }

    /// The file a save writes from a lossy writer's saved form: `json`,
    /// with what the document carries unread put back, stamped with the
    /// reader version the result needs — the core's encoder and ladder
    /// (review 2026-09-27, P2-45: the apps' saves used to be Swift's own
    /// encoding, which dropped every field Swift does not model).
    pub fn saved_json_from(&self, json: &str) -> Result<String, String> {
        let mut meta = ProjectMetadata::from_json(json).map_err(|e| e.to_string())?;
        self.carry_unread_into(&mut meta);
        meta.min_reader_version = Some(meta.minimum_reader_version());
        let written = meta.to_json().map_err(|e| e.to_string())?;
        // Below the bags: a key the format does not read, at any depth, as
        // the file held it (the lossless reader, review 2026-09-27, P3-44).
        let Some(source) = &self.unread_source_json else {
            return Ok(written);
        };
        let mut value: serde_json::Value =
            serde_json::from_str(&written).map_err(|e| e.to_string())?;
        let kept = promo_model::unread::keep_unread(&mut value, source);
        // What it carries unread, a reader before the lossless one would
        // drop: the stamp keeps such readers out (rung 48).
        let raised = promo_model::unread::floor_stamp(&mut value, source);
        if !kept && !raised {
            return Ok(written);
        }
        serde_json::to_string(&value).map_err(|e| e.to_string())
    }

    /// The document's unread content into a lossy copy, then the last
    /// file's for whatever the document holds none of.
    fn carry_unread_into(&self, meta: &mut ProjectMetadata) {
        meta.carry_unread_from(&self.meta);
        if let Some(source) = &self.unread_source {
            meta.carry_unread_from(source);
        }
    }

    pub fn undo(&mut self) -> bool {
        let Some(snapshot) = self.undo.pop() else {
            return false;
        };
        if let Ok(current) = self.to_json() {
            if let Ok(meta) = ProjectMetadata::from_json(&snapshot) {
                self.redo.push(current);
                let before = std::mem::replace(&mut self.meta, meta);
                self.version += 1;
                self.note(&before);
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
                let before = std::mem::replace(&mut self.meta, meta);
                self.version += 1;
                self.note(&before);
                return true;
            }
        }
        false
    }

    /// Runs `command` on the composition's layers as a document of their
    /// own — the project's resources and settings, the composition's
    /// canvas — and writes the layers back. Attachments inside resolve
    /// here too, and the nesting rules are checked on the result.
    fn run_in_composition(
        meta: &mut ProjectMetadata,
        resource_id: &str,
        command: &Command,
    ) -> Result<(), String> {
        let resources = meta.resources.get_or_insert_with(Vec::new);
        let index = resources
            .iter()
            .position(|r| r.id == resource_id)
            .ok_or_else(|| format!("no resource with id {resource_id}"))?;
        let composition = resources[index]
            .composition
            .clone()
            .ok_or_else(|| format!("resource {resource_id} is not a composition"))?;
        let mut inner = meta.clone();
        inner.layers = Some(composition.layers.clone());
        inner.composition_settings.canvas_width = composition.canvas_width;
        inner.composition_settings.canvas_height = composition.canvas_height;
        Self::run(&mut inner, command)?;
        promo_timeline::resolve_attachments(&mut inner);
        let mut updated = composition;
        updated.layers = inner.layers.unwrap_or_default();
        meta.resources.as_mut().expect("resources")[index].composition = Some(updated);
        let problems = promo_model::nesting::problems(meta);
        if let Some(problem) = problems
            .iter()
            .find(|p| p.contains("contains itself") || p.contains("nests deeper than"))
        {
            return Err(problem.clone());
        }
        Ok(())
    }

    fn run(meta: &mut ProjectMetadata, command: &Command) -> Result<(), String> {
        if let Command::InComposition {
            resource_id,
            command,
        } = command
        {
            return Self::run_in_composition(meta, resource_id, command);
        }
        let layers = meta.layers.get_or_insert_with(Vec::new);
        // A stage layer's members (rung 33) are layers too, one depth
        // down: a command that names a member finds it inside its stage.
        fn find<'a>(
            layers: &'a mut [promo_model::ProjectLayer],
            id: &str,
        ) -> Result<&'a mut promo_model::ProjectLayer, String> {
            for layer in layers.iter_mut() {
                if layer.id == id {
                    return Ok(layer);
                }
                if let Some(members) = layer.members.as_mut() {
                    if let Some(member) = members.iter_mut().find(|m| m.id == id) {
                        return Ok(member);
                    }
                }
            }
            Err(format!("no layer with id {id}"))
        }
        // The survivors of a delete close the gap — the Mac editor
        // renumbers after a delete, and `MoveLayer` already renumbers, so
        // a document never carries a hole in its z-order.
        fn renumber(layers: &mut [promo_model::ProjectLayer]) {
            let mut order: Vec<usize> = (0..layers.len()).collect();
            order.sort_by_key(|&i| layers[i].sort_index);
            for (rank, i) in order.into_iter().enumerate() {
                layers[i].sort_index = rank as i64;
            }
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
                if layers.len() != before {
                    renumber(layers);
                } else {
                    // A stage member (rung 33): dropped from its stage, the
                    // other members closing the gap.
                    let mut dropped = false;
                    for layer in layers.iter_mut() {
                        if let Some(members) = layer.members.as_mut() {
                            let n = members.len();
                            members.retain(|m| m.id != *layer_id);
                            if members.len() != n {
                                dropped = true;
                                renumber(members);
                            }
                        }
                    }
                    if !dropped {
                        return Err(format!("no layer with id {layer_id}"));
                    }
                    // A camera that was looking AT the departed member has
                    // nothing to look at: clear the gaze rather than leave a
                    // pointer at a member the stage no longer has. The
                    // engine falls back to the stage centre either way; this
                    // keeps the file honest about it.
                    for layer in layers.iter_mut() {
                        for keyframe in layer.keyframes.iter_mut() {
                            let names_it = keyframe.camera.as_ref().is_some_and(|c| {
                                matches!(
                                    c.target.as_ref(),
                                    Some(promo_model::CameraTarget::Member { member }) if member == layer_id
                                )
                            });
                            if names_it {
                                if let Some(camera) = keyframe.camera.as_mut() {
                                    camera.target = None;
                                }
                            }
                        }
                    }
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
                let previous = match resources.iter_mut().find(|r| r.id == resource.id) {
                    Some(existing) => std::mem::replace(existing, (**resource).clone()),
                    None => return Err(format!("no resource with id {}", resource.id)),
                };
                retime_layers_for_resource(meta, &previous, resource);
                // Editing the SELECTED theme refreshes its materialized
                // copy at once — the Mac does this on every save, and a
                // copy that goes stale until the next open is a colour
                // that edits right and renders wrong. Not the selection
                // moment, so typography is left alone.
                if meta.composition_settings.palette_resource_id.as_deref()
                    == Some(resource.id.as_str())
                    && resource.kind == promo_model::ProjectResourceKind::Palette
                {
                    crate::theme::sync_selected_palette(
                        &mut meta.composition_settings,
                        resource,
                        false,
                    );
                }
            }
            Command::DeleteResource { resource_id } => {
                // Every way a project can POINT at a resource, not just the
                // two a layer states directly: a mask, a route on a member
                // or on the camera (rung 40), a picture worn by a slot
                // (rung 38), and the two bodies a morph flies between
                // (rung 39). Missing one lets a delete succeed and the
                // project quietly render something else.
                let id = resource_id.as_str();
                let points_at = |l: &promo_model::ProjectLayer| -> bool {
                    l.resource_id.as_deref() == Some(id)
                        || l.mask_resource_id.as_deref() == Some(id)
                        || l.keyframes.iter().any(|k| {
                            k.resource_id.as_deref() == Some(id)
                                || k.motion_path
                                    .as_ref()
                                    .is_some_and(|m| m.path_resource_id == id)
                                || k.camera.as_ref().is_some_and(|c| {
                                    c.motion_path
                                        .as_ref()
                                        .is_some_and(|m| m.path_resource_id == id)
                                })
                        })
                };
                let referenced = promo_model::nesting::all_layers(meta)
                    .iter()
                    .any(|l| points_at(l))
                    || meta.resources.iter().flatten().any(|r| {
                        r.materials
                            .iter()
                            .flat_map(|m| m.values())
                            .any(|b| b.resource_id() == Some(id))
                            || r.particles
                                .as_ref()
                                .and_then(|p| p.morph.as_ref())
                                .is_some_and(|m| m.from == id || m.to == id)
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
            Command::SetLayerResource {
                layer_id,
                resource_id,
            } => {
                let layer_kind = find(layers, layer_id)?.kind;
                if let Some(rid) = resource_id {
                    let wanted = match layer_kind {
                        promo_model::ProjectLayerKind::Background => {
                            promo_model::ProjectResourceKind::Background
                        }
                        // A stage layer (rung 33) plays nothing of its own;
                        // media is bound on a member.
                        promo_model::ProjectLayerKind::Stage => {
                            return Err(format!(
                                "layer {layer_id} is a stage; a stage plays no resource of its \
                                 own — set the resource on one of its members"
                            ));
                        }
                        promo_model::ProjectLayerKind::Model => {
                            promo_model::ProjectResourceKind::Model
                        }
                        promo_model::ProjectLayerKind::Video => {
                            promo_model::ProjectResourceKind::Video
                        }
                        promo_model::ProjectLayerKind::Image => {
                            promo_model::ProjectResourceKind::Image
                        }
                        promo_model::ProjectLayerKind::Caption => {
                            promo_model::ProjectResourceKind::Caption
                        }
                        promo_model::ProjectLayerKind::Drawing => {
                            promo_model::ProjectResourceKind::Drawing
                        }
                        promo_model::ProjectLayerKind::Audio => {
                            promo_model::ProjectResourceKind::Audio
                        }
                    };
                    let resource = meta
                        .resources
                        .as_deref()
                        .unwrap_or(&[])
                        .iter()
                        .find(|r| r.id == *rid)
                        .ok_or_else(|| format!("no resource with id {rid}"))?;
                    if resource.kind != wanted {
                        return Err(format!(
                            "resource {rid} is {:?}, not what a {layer_kind:?} layer shows",
                            resource.kind
                        ));
                    }
                } else if layer_kind != promo_model::ProjectLayerKind::Background {
                    return Err(format!(
                        "only a background layer may show nothing (it paints the settings ground); a {layer_kind:?} layer without a resource is a hole"
                    ));
                }
                let layers = meta.layers.get_or_insert_with(Vec::new);
                let layer = find(layers, layer_id)?;
                if layer.resource_id != *resource_id {
                    // Cut pointers belong to the OLD resource; carrying them
                    // to a new one leaves ids nobody defines.
                    layer.media_cut_id = None;
                    layer.image_cut_id = None;
                }
                layer.resource_id = resource_id.clone();
            }
            Command::UpdateSettings { settings } => {
                if let Some(id) = settings.palette_resource_id.as_deref() {
                    let is_palette =
                        meta.resources.as_deref().unwrap_or(&[]).iter().any(|r| {
                            r.id == id && r.kind == promo_model::ProjectResourceKind::Palette
                        });
                    if !is_palette {
                        return Err(format!("paletteResourceID {id} names no palette resource"));
                    }
                }
                meta.composition_settings = (**settings).clone();
                if let Some(id) = meta.composition_settings.palette_resource_id.clone() {
                    if let Some(resource) = meta
                        .resources
                        .as_deref()
                        .unwrap_or(&[])
                        .iter()
                        .find(|r| r.id == id)
                        .cloned()
                    {
                        crate::theme::sync_selected_palette(
                            &mut meta.composition_settings,
                            &resource,
                            false,
                        );
                    }
                }
            }
            Command::RenameProject { name } => {
                if name.trim().is_empty() {
                    return Err("a project needs a name".into());
                }
                meta.name = name.clone();
            }
            Command::SelectPalette {
                palette_resource_id,
            } => match palette_resource_id {
                None => meta.composition_settings.palette_resource_id = None,
                Some(id) => {
                    let resource = meta
                        .resources
                        .as_deref()
                        .unwrap_or(&[])
                        .iter()
                        .find(|r| r.id == *id)
                        .ok_or_else(|| format!("no resource with id {id}"))?
                        .clone();
                    if resource.kind != promo_model::ProjectResourceKind::Palette {
                        return Err(format!("resource {id} is not a palette"));
                    }
                    meta.composition_settings.palette_resource_id = Some(id.clone());
                    crate::theme::sync_selected_palette(
                        &mut meta.composition_settings,
                        &resource,
                        true,
                    );
                }
            },
            Command::UpdateLayer { layer_id, patch } => {
                checked_patch(
                    patch,
                    &[
                        ("keyframes", "upsertKeyframe and deleteKeyframe own them"),
                        ("id", "a layer's id is its identity"),
                        ("kind", "a layer never changes kind; add another"),
                        ("sortIndex", "moveLayer owns the order"),
                    ],
                )?;
                let layer = find(layers, layer_id)?;
                let mut wire = serde_json::to_value(&*layer).map_err(|e| e.to_string())?;
                merge_patch(&mut wire, patch);
                let patched: promo_model::ProjectLayer = serde_json::from_value(wire)
                    .map_err(|e| format!("patch rejected by the format: {e}"))?;
                if let Some(resource_id) = &patched.resource_id {
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
                *find(meta.layers.get_or_insert_with(Vec::new), layer_id)? = patched;
            }
            Command::InComposition { .. } => {
                unreachable!("dispatched to run_in_composition before the layer borrow")
            }
            Command::PatchResource { resource_id, patch } => {
                checked_patch(
                    patch,
                    &[
                        ("id", "a resource's id is its identity"),
                        ("kind", "a resource never changes kind; add another"),
                    ],
                )?;
                let existing = meta
                    .resources
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .find(|r| r.id == *resource_id)
                    .ok_or_else(|| format!("no resource with id {resource_id}"))?;
                let mut wire = serde_json::to_value(existing).map_err(|e| e.to_string())?;
                merge_patch(&mut wire, patch);
                let patched: promo_model::ProjectResource = serde_json::from_value(wire)
                    .map_err(|e| format!("patch rejected by the format: {e}"))?;
                return Self::run(
                    meta,
                    &Command::UpdateResource {
                        resource: Box::new(patched),
                    },
                );
            }
            Command::PatchSettings { patch } => {
                checked_patch(patch, &[])?;
                let mut wire =
                    serde_json::to_value(&meta.composition_settings).map_err(|e| e.to_string())?;
                merge_patch(&mut wire, patch);
                let patched: promo_model::CompositionSettings = serde_json::from_value(wire)
                    .map_err(|e| format!("patch rejected by the format: {e}"))?;
                return Self::run(
                    meta,
                    &Command::UpdateSettings {
                        settings: Box::new(patched),
                    },
                );
            }
            Command::SetMarkers { markers } => {
                let mut seen = std::collections::HashSet::new();
                for marker in markers {
                    if !marker.time.is_finite() || marker.time < 0.0 {
                        return Err(format!(
                            "marker {} has time {}; markers sit at or after 0",
                            marker.id, marker.time
                        ));
                    }
                    if marker.id.trim().is_empty() || !seen.insert(marker.id.as_str()) {
                        return Err(format!("marker id {:?} is empty or repeated", marker.id));
                    }
                }
                let mut sorted = markers.clone();
                sorted.sort_by(|a, b| {
                    a.time
                        .partial_cmp(&b.time)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                meta.markers = if sorted.is_empty() {
                    None
                } else {
                    Some(sorted)
                };
            }
            Command::MoveLayer { layer_id, index } => {
                // The z-order is `sortIndex`, not array position: the move
                // rewrites sort indices IN PLACE and leaves every element
                // where it sits — what the apps' reorder does, so a file the
                // app and an agent both edit does not churn its array order.
                let moving = layers
                    .iter()
                    .position(|l| l.id == *layer_id)
                    .ok_or_else(|| format!("no layer with id {layer_id}"))?;
                let mut order: Vec<usize> = (0..layers.len()).collect();
                order.sort_by_key(|&i| layers[i].sort_index);
                order.retain(|&i| i != moving);
                let to = (*index).min(order.len());
                order.insert(to, moving);
                for (rank, i) in order.into_iter().enumerate() {
                    layers[i].sort_index = rank as i64;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The merge-patch door changes exactly what it names: a wipe arrives,
    /// the name and every keyframe stay, and the undo gate still returns
    /// the byte-identical document.
    #[test]
    fn update_layer_patches_only_what_it_names() {
        let mut doc = doc();
        let original = doc.to_json().unwrap();
        let slide = layer_id(&doc, 1);
        let before = doc.meta().layers.as_ref().unwrap()[1].clone();
        doc.apply(&command(serde_json::json!({
            "kind": "updateLayer", "layerID": slide,
            "patch": { "transitionIn": { "kind": "wipe", "duration": 0.5 } }
        })))
        .unwrap();
        let after = &doc.meta().layers.as_ref().unwrap()[1];
        let wire = serde_json::to_value(after).unwrap();
        assert_eq!(wire["transitionIn"]["kind"], "wipe");
        assert_eq!(after.name, before.name, "unnamed fields stay");
        assert_eq!(
            after.keyframes.len(),
            before.keyframes.len(),
            "keyframes stay"
        );
        assert!(doc.undo());
        assert_eq!(
            doc.to_json().unwrap(),
            original,
            "the undo gate holds for patches"
        );
    }

    /// What a patch may not touch is refused with the reason, never
    /// silently merged.
    #[test]
    fn update_layer_refuses_keyframes_and_identity() {
        let mut doc = doc();
        let slide = layer_id(&doc, 1);
        for (patch, expect) in [
            (serde_json::json!({ "keyframes": [] }), "upsertKeyframe"),
            (serde_json::json!({ "id": "other" }), "identity"),
            (serde_json::json!({ "sortIndex": 0 }), "moveLayer"),
            (serde_json::json!({ "kind": "caption" }), "kind"),
            (
                serde_json::json!({ "resourceID": "ghost" }),
                "unknown resource",
            ),
            (serde_json::json!([1, 2]), "JSON object"),
        ] {
            let err = doc
                .apply(&command(serde_json::json!({
                    "kind": "updateLayer", "layerID": slide, "patch": patch
                })))
                .unwrap_err();
            assert!(err.contains(expect), "{patch}: {err}");
        }
        let err = doc
            .apply(&command(serde_json::json!({
                "kind": "updateLayer", "layerID": "ghost", "patch": {}
            })))
            .unwrap_err();
        assert!(err.contains("no layer"), "{err}");
    }

    /// Resource and settings patches ride the whole-object commands'
    /// validation: a trim lands, a dangling theme pointer is refused.
    #[test]
    fn resource_and_settings_patches_delegate_to_their_validation() {
        let mut doc = doc();
        let resource_id = doc.meta().resources.as_ref().unwrap()[0].id.clone();
        doc.apply(&command(serde_json::json!({
            "kind": "patchResource", "resourceID": resource_id,
            "patch": { "trimStart": 0.5 }
        })))
        .unwrap();
        let wire = serde_json::to_value(&doc.meta().resources.as_ref().unwrap()[0]).unwrap();
        assert_eq!(wire["trimStart"], 0.5);

        doc.apply(&command(serde_json::json!({
            "kind": "patchSettings", "patch": { "canvasWidth": 1280.0 }
        })))
        .unwrap();
        assert_eq!(doc.meta().composition_settings.canvas_width, 1280.0);
        let err = doc
            .apply(&command(serde_json::json!({
                "kind": "patchSettings", "patch": { "paletteResourceID": "ghost" }
            })))
            .unwrap_err();
        assert!(err.contains("names no palette"), "{err}");
    }

    /// The schema served to agents names every variant — a variant added
    /// to the enum without reaching the schema would be a tool nobody can
    /// call.
    #[test]
    fn the_command_schema_names_every_kind() {
        let text = command_schema().to_string();
        for kind in [
            "renameLayer",
            "setLayerEnabled",
            "setLayerTiming",
            "setLayerAudioFocus",
            "setLayerMediaCut",
            "setLayerImageCut",
            "deleteLayer",
            "moveLayer",
            "upsertKeyframe",
            "deleteKeyframe",
            "addResource",
            "addLayer",
            "updateResource",
            "deleteResource",
            "setLayerResource",
            "updateSettings",
            "renameProject",
            "selectPalette",
            "updateLayer",
            "patchResource",
            "patchSettings",
        ] {
            assert!(
                text.contains(&format!("\"{kind}\"")),
                "schema lacks `{kind}`"
            );
        }
        assert!(text.contains("layerID"), "the wire spelling of ids");
    }

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

    /// The theme commands end to end: selecting a palette materializes its
    /// entries and points default colours at the roles; EDITING the
    /// selected palette refreshes the materialized copy at once; a
    /// non-palette refuses; deselecting clears only the pointer; and the
    /// whole dance undoes to the exact original.
    #[test]
    fn select_palette_materializes_refreshes_on_edit_and_undoes() {
        let mut doc = doc();
        let original = doc.to_json().unwrap();
        doc.apply(&command(
            serde_json::json!({"kind": "addResource", "resource": {
            "id": "T", "kind": "palette", "filename": "", "displayName": "Studio",
            "addedAt": 0, "palette": [{"name": "text", "colorHex": "F2F2F7"}]}}),
        ))
        .unwrap();
        doc.apply(&command(serde_json::json!({
            "kind": "selectPalette", "paletteResourceID": "T"})))
            .unwrap();
        let settings = |doc: &Document| doc.meta().composition_settings.clone();
        assert_eq!(settings(&doc).palette_resource_id.as_deref(), Some("T"));
        assert_eq!(settings(&doc).subtitle_color_hex, "@text");
        assert_eq!(settings(&doc).palette.unwrap()[0].color_hex, "F2F2F7");

        // Editing the selected theme refreshes the copy immediately.
        let mut edited = doc
            .meta()
            .resources
            .as_ref()
            .unwrap()
            .iter()
            .find(|r| r.id == "T")
            .unwrap()
            .clone();
        edited.palette.as_mut().unwrap()[0].color_hex = "AABBCC".into();
        doc.apply(&Command::UpdateResource {
            resource: Box::new(edited),
        })
        .unwrap();
        assert_eq!(settings(&doc).palette.unwrap()[0].color_hex, "AABBCC");

        // A non-palette is refused; deselecting clears only the pointer.
        let image_id = doc.meta().resources.as_ref().unwrap()[0].id.clone();
        assert!(doc
            .apply(&command(serde_json::json!({
                "kind": "selectPalette", "paletteResourceID": image_id})))
            .is_err());
        doc.apply(&command(serde_json::json!({"kind": "selectPalette"})))
            .unwrap();
        assert_eq!(settings(&doc).palette_resource_id, None);
        assert!(settings(&doc).palette.is_some(), "the copy stays");

        for _ in 0..4 {
            assert!(doc.undo());
        }
        assert_eq!(doc.to_json().unwrap(), original);
    }

    /// Re-pointing: kind must match, cut pointers die with the old
    /// resource, and only a background layer may be cleared.
    #[test]
    fn set_layer_resource_matches_kinds_and_drops_stale_cuts() {
        let mut doc = doc();
        let slide = layer_id(&doc, 1);
        let background = layer_id(&doc, 0);
        // A second image resource with a cut on the FIRST one's layer.
        let first = doc.meta().layers.as_ref().unwrap()[1]
            .resource_id
            .clone()
            .unwrap();
        let mut with_cut = doc
            .meta()
            .resources
            .as_ref()
            .unwrap()
            .iter()
            .find(|r| r.id == first)
            .unwrap()
            .clone();
        with_cut.image_cuts = vec![serde_json::from_value(serde_json::json!({
            "id": "IC", "rect": [[0.0, 0.0], [0.5, 0.5]],
            "filename": "c.png", "createdAt": 0}))
        .unwrap()];
        doc.apply(&Command::UpdateResource {
            resource: Box::new(with_cut),
        })
        .unwrap();
        doc.apply(&command(serde_json::json!({
            "kind": "setLayerImageCut", "layerID": slide, "imageCutID": "IC"})))
            .unwrap();
        doc.apply(&command(
            serde_json::json!({"kind": "addResource", "resource": {
            "id": "R2", "kind": "image", "filename": "other.png",
            "displayName": "Other", "addedAt": 0}}),
        ))
        .unwrap();

        // Repoint: the stale cut pointer dies with the old resource.
        doc.apply(&command(serde_json::json!({
            "kind": "setLayerResource", "layerID": slide, "resourceID": "R2"})))
            .unwrap();
        let layer = &doc.meta().layers.as_ref().unwrap()[1];
        assert_eq!(layer.resource_id.as_deref(), Some("R2"));
        assert_eq!(
            layer.image_cut_id, None,
            "cut ids belong to the old resource"
        );

        // Kind mismatch refused; clearing allowed only on background.
        doc.apply(&command(
            serde_json::json!({"kind": "addResource", "resource": {
            "id": "A1", "kind": "audio", "filename": "a.mp3",
            "displayName": "A", "addedAt": 0}}),
        ))
        .unwrap();
        assert!(doc
            .apply(&command(serde_json::json!({
                "kind": "setLayerResource", "layerID": slide, "resourceID": "A1"})))
            .is_err());
        assert!(doc
            .apply(&command(serde_json::json!({
                "kind": "setLayerResource", "layerID": slide})))
            .is_err());
        doc.apply(&command(serde_json::json!({
            "kind": "setLayerResource", "layerID": background})))
            .unwrap();
        assert_eq!(
            doc.meta().layers.as_ref().unwrap()[0].resource_id,
            None,
            "a background may show nothing; it paints the settings ground"
        );
    }

    /// The settings form's commands: wholesale settings replace (with the
    /// selected theme's copy refreshed and a dangling theme refused), a
    /// rename that refuses blank, and the whole dance undoes exactly.
    #[test]
    fn update_settings_and_rename_hold_the_theme_line() {
        let mut doc = doc();
        let original = doc.to_json().unwrap();
        doc.apply(&command(
            serde_json::json!({"kind": "addResource", "resource": {
            "id": "T", "kind": "palette", "filename": "", "displayName": "Studio",
            "addedAt": 0, "palette": [{"name": "text", "colorHex": "F2F2F7"}]}}),
        ))
        .unwrap();
        doc.apply(&command(serde_json::json!({
            "kind": "selectPalette", "paletteResourceID": "T"})))
            .unwrap();

        // Wholesale replace, carrying the pointer along — and even a stale
        // materialized copy in the payload comes back true, because the
        // resource is the authority and the command re-syncs from it.
        let mut settings = doc.meta().composition_settings.clone();
        settings.subtitle_font_size = 96.0;
        settings.palette = Some(vec![promo_model::PaletteColor {
            name: "text".into(),
            color_hex: "STALE0".into(),
        }]);
        doc.apply(&Command::UpdateSettings {
            settings: Box::new(settings),
        })
        .unwrap();
        let now = &doc.meta().composition_settings;
        assert_eq!(now.subtitle_font_size, 96.0);
        assert_eq!(
            now.palette.as_ref().unwrap()[0].color_hex,
            "F2F2F7",
            "the materialized copy follows the RESOURCE, not the payload"
        );

        // A dangling theme pointer is refused; so is a blank name.
        let mut dangling = doc.meta().composition_settings.clone();
        dangling.palette_resource_id = Some("GHOST".into());
        assert!(doc
            .apply(&Command::UpdateSettings {
                settings: Box::new(dangling)
            })
            .is_err());
        assert!(doc
            .apply(&command(
                serde_json::json!({"kind": "renameProject", "name": "  "})
            ))
            .is_err());
        doc.apply(&command(serde_json::json!({
            "kind": "renameProject", "name": "Retitled"})))
            .unwrap();
        assert_eq!(doc.meta().name, "Retitled");

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

    /// The change log names what a bump touched — one layer for a rename,
    /// the order and the new layer for an add, settings for a patch — and
    /// undo answers the same way. A version the log no longer remembers
    /// reads as "everything".
    #[test]
    fn changes_name_exactly_what_a_bump_touched() {
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../fixtures/projects/project-4.json"
        ))
        .unwrap();
        let mut doc = Document::open(&raw).unwrap();
        let first = doc.meta().layers.as_deref().unwrap()[0].id.clone();
        assert!(doc.changes_since(0).is_empty(), "nothing yet");

        doc.apply(&Command::RenameLayer {
            layer_id: first.clone(),
            name: "Renamed".into(),
        })
        .unwrap();
        let c = doc.changes_since(0);
        assert_eq!(c.layers, vec![first.clone()]);
        assert!(
            c.resources.is_empty() && !c.order && !c.settings && !c.project,
            "{c:?}"
        );
        assert_eq!(doc.layer_revision(&first), 1);

        doc.apply(&Command::PatchSettings {
            patch: serde_json::json!({"fps": 24}),
        })
        .unwrap();
        let c = doc.changes_since(1);
        assert!(c.layers.is_empty() && c.settings && !c.order, "{c:?}");
        let both = doc.changes_since(0);
        assert_eq!(both.layers, vec![first.clone()]);
        assert!(both.settings);

        doc.apply(&Command::AddLayer {
            layer: serde_json::from_value(serde_json::json!({
                "id": "NEW", "name": "New", "sortIndex": 99, "kind": "caption",
                "isEnabled": true, "startTime": 0, "duration": 1, "captionText": "x", "keyframes": []
            }))
            .unwrap(),
        })
        .unwrap();
        let c = doc.changes_since(2);
        assert_eq!(c.layers, vec!["NEW".to_string()]);
        assert!(c.order, "an add changes the order");

        assert!(doc.undo());
        let c = doc.changes_since(3);
        assert_eq!(
            c.layers,
            vec!["NEW".to_string()],
            "undo names the layer it removed"
        );
        assert!(c.order);
        assert_eq!(
            doc.layer_revision("NEW"),
            2,
            "touched twice: added, removed"
        );
        assert!(doc.changes_since(doc.version()).is_empty());

        // Beyond the log's memory: everything.
        for i in 0..300 {
            doc.apply(&Command::RenameLayer {
                layer_id: first.clone(),
                name: format!("n{i}"),
            })
            .unwrap();
        }
        let all = doc.changes_since(1);
        assert!(all.order && all.settings && all.project);
        assert!(
            all.layers.len() > 1,
            "every layer named: {}",
            all.layers.len()
        );
    }

    /// Deleting a layer closes the gap in the z-order, as the Mac editor
    /// does after a delete and as `MoveLayer` already does — so a
    /// document never carries a hole, and the app's delete and the core's
    /// agree byte for byte.
    #[test]
    fn deleting_a_layer_renumbers_the_survivors() {
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../fixtures/projects/project-4.json"
        ))
        .unwrap();
        let mut doc = Document::open(&raw).unwrap();
        let layers = doc.meta().layers.as_deref().unwrap();
        assert!(layers.len() >= 3);
        let mut by_sort: Vec<&promo_model::ProjectLayer> = layers.iter().collect();
        by_sort.sort_by_key(|l| l.sort_index);
        let middle = by_sort[1].id.clone();
        doc.apply(&Command::DeleteLayer { layer_id: middle })
            .unwrap();
        let mut after: Vec<i64> = doc
            .meta()
            .layers
            .as_deref()
            .unwrap()
            .iter()
            .map(|l| l.sort_index)
            .collect();
        after.sort();
        assert_eq!(
            after,
            (0..after.len() as i64).collect::<Vec<_>>(),
            "no hole: {after:?}"
        );
    }

    /// A move rewrites sortIndex in place: the array keeps its positions
    /// (the apps' reorder never moves elements either), so the app's file
    /// and the core's agree byte for byte after the same reorder.
    #[test]
    fn move_keeps_array_positions_and_renumbers_ranks() {
        let mut doc = doc();
        let before: Vec<String> = doc
            .meta()
            .layers
            .as_deref()
            .unwrap()
            .iter()
            .map(|l| l.id.clone())
            .collect();
        let last = layer_id(&doc, 2);
        doc.apply(&command(serde_json::json!(
            {"kind": "moveLayer", "layerID": last, "index": 0})))
            .unwrap();
        let layers = doc.meta().layers.as_deref().unwrap();
        let after: Vec<String> = layers.iter().map(|l| l.id.clone()).collect();
        assert_eq!(after, before, "no element moved");
        let ranks: Vec<i64> = layers.iter().map(|l| l.sort_index).collect();
        assert_eq!(ranks, vec![1, 2, 0], "the last layer now sorts first");
    }

    fn retime_doc(layers: serde_json::Value, resource: serde_json::Value) -> Document {
        Document::open(
            &serde_json::json!({
                "id": "P", "name": "t", "createdAt": 0, "state": "recorded",
                "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
                "compositionSettings": {"canvasWidth": 320, "canvasHeight": 180, "backgroundColorHex": "000000"},
                "resources": [resource], "layers": layers
            })
            .to_string(),
        )
        .unwrap()
    }

    fn duration_of(doc: &Document, id: &str) -> f64 {
        doc.meta()
            .layers
            .as_deref()
            .unwrap()
            .iter()
            .find(|l| l.id == id)
            .unwrap()
            .duration
            .unwrap()
    }

    /// The Mac's `replaceResource` rule, on its own test vectors: a held
    /// frame lengthens the layer that played the whole clip (and the
    /// background under it); a layer shortened by hand keeps its length;
    /// a looped layer and a beyond-end layer keep their stretch; a plain
    /// layer follows a trim.
    #[test]
    fn updating_a_resource_retimes_the_layers_that_played_all_of_it() {
        let video = |extra: serde_json::Value| {
            let mut r = serde_json::json!({"id": "R", "kind": "video", "filename": "v.mp4", "displayName": "v",
                "addedAt": 0, "duration": 10, "imageCuts": [], "disabledAudioTrackIndices": []});
            for (k, v) in extra.as_object().unwrap() {
                r[k] = v.clone();
            }
            r
        };
        // A 3 s held frame at 4 s: 10 → 13, and the background follows.
        let mut doc = retime_doc(
            serde_json::json!([
                {"id": "BG", "name": "bg", "sortIndex": 0, "kind": "background", "isEnabled": true, "startTime": 0, "duration": 10, "keyframes": []},
                {"id": "V", "name": "v", "sortIndex": 1, "kind": "video", "isEnabled": true, "startTime": 0, "duration": 10, "resourceID": "R", "keyframes": []}
            ]),
            video(serde_json::json!({})),
        );
        doc.apply(&Command::UpdateResource {
            resource: Box::new(serde_json::from_value(video(serde_json::json!({
                "trimKeyframes": [{"id": "K", "time": 4, "isIncluded": true, "extendedPauseDuration": 3}]
            }))).unwrap()),
        })
        .unwrap();
        assert!((duration_of(&doc, "V") - 13.0).abs() < 1e-9);
        assert!(
            (duration_of(&doc, "BG") - 13.0).abs() < 1e-9,
            "the background covers the content"
        );

        // Shortened by hand to 5: stays 5.
        let mut doc = retime_doc(
            serde_json::json!([
                {"id": "V", "name": "v", "sortIndex": 0, "kind": "video", "isEnabled": true, "startTime": 0, "duration": 5, "resourceID": "R", "keyframes": []}
            ]),
            video(serde_json::json!({})),
        );
        doc.apply(&Command::UpdateResource {
            resource: Box::new(serde_json::from_value(video(serde_json::json!({
                "trimKeyframes": [{"id": "K", "time": 7, "isIncluded": true, "extendedPauseDuration": 3}]
            }))).unwrap()),
        })
        .unwrap();
        assert!((duration_of(&doc, "V") - 5.0).abs() < 1e-9);

        // Looped and beyond-end layers keep their stretch; a plain one follows the trim.
        let clip = |extra: serde_json::Value| {
            let mut r = video(serde_json::json!({"trimStart": 0, "trimEnd": 4}));
            for (k, v) in extra.as_object().unwrap() {
                r[k] = v.clone();
            }
            r
        };
        let mut doc = retime_doc(
            serde_json::json!([
                {"id": "L", "name": "l", "sortIndex": 0, "kind": "video", "isEnabled": true, "startTime": 0, "duration": 30, "resourceID": "R", "keyframes": []},
                {"id": "H", "name": "h", "sortIndex": 1, "kind": "video", "isEnabled": true, "startTime": 0, "duration": 25, "resourceID": "R", "beyondEnd": "hold", "keyframes": []}
            ]),
            clip(serde_json::json!({"looped": true})),
        );
        doc.apply(&Command::UpdateResource {
            resource: Box::new(
                serde_json::from_value(clip(serde_json::json!({"looped": true, "trimEnd": 3})))
                    .unwrap(),
            ),
        })
        .unwrap();
        assert!(
            (duration_of(&doc, "L") - 30.0).abs() < 1e-9,
            "a looped layer is not governed by the trim"
        );
        assert!(
            (duration_of(&doc, "H") - 25.0).abs() < 1e-9,
            "beyondEnd lifts the content cap"
        );

        let mut doc = retime_doc(
            serde_json::json!([
                {"id": "L", "name": "l", "sortIndex": 0, "kind": "video", "isEnabled": true, "startTime": 0, "duration": 4, "resourceID": "R", "keyframes": []}
            ]),
            clip(serde_json::json!({})),
        );
        doc.apply(&Command::PatchResource {
            resource_id: "R".into(),
            patch: serde_json::json!({"trimEnd": 3}),
        })
        .unwrap();
        assert!(
            (duration_of(&doc, "L") - 3.0).abs() < 1e-9,
            "an untouched full-length layer keeps tracking the trim"
        );
    }

    /// Anchored layers follow what moved, after every step — the Mac
    /// reconciles after each edit through the same resolver.
    #[test]
    fn attached_layers_follow_a_timing_edit() {
        let mut doc = retime_doc(
            serde_json::json!([
                {"id": "A", "name": "a", "sortIndex": 0, "kind": "caption", "isEnabled": true, "startTime": 1, "duration": 3, "captionText": "a", "keyframes": []},
                {"id": "B", "name": "b", "sortIndex": 1, "kind": "caption", "isEnabled": true, "startTime": 0, "duration": 2, "captionText": "b",
                 "timing": {"start": {"from": "previousEnd", "offset": 0.5}}, "keyframes": []}
            ]),
            serde_json::json!({"id": "R", "kind": "image", "filename": "i.png", "displayName": "i", "addedAt": 0, "imageCuts": []}),
        );
        let start_of = |doc: &Document, id: &str| {
            doc.meta()
                .layers
                .as_deref()
                .unwrap()
                .iter()
                .find(|l| l.id == id)
                .unwrap()
                .start_time
        };
        // Opening resolves nothing (the file is the file); the first edit does.
        assert!(start_of(&doc, "B").abs() < 1e-9);
        doc.apply(&Command::SetLayerTiming {
            layer_id: "A".into(),
            start_time: 2.0,
            duration: Some(3.0),
        })
        .unwrap();
        assert!(
            (start_of(&doc, "B") - 5.5).abs() < 1e-9,
            "B sits half a second after A's new end: {}",
            start_of(&doc, "B")
        );
        assert!(
            doc.changes_since(0).layers.contains(&"B".to_string()),
            "the follower is a touched layer too"
        );
    }
    /// A merged step joins the previous undo step: a drag of many writes
    /// is one Ctrl+Z, and the redo line still clears. The first step
    /// of a document cannot join anything and stands alone.
    #[test]
    fn a_merged_step_joins_the_previous_undo_step() {
        let mut doc = retime_doc(
            serde_json::json!([
                {"id": "L", "name": "l", "sortIndex": 0, "kind": "video", "isEnabled": true, "startTime": 0, "duration": 4, "resourceID": "R", "keyframes": []}
            ]),
            serde_json::json!({"id": "R", "kind": "video", "filename": "v.mp4", "displayName": "v", "addedAt": 0, "duration": 4, "imageCuts": []}),
        );
        let start = doc.to_json().unwrap();
        let retime = |start_time: f64| Command::SetLayerTiming {
            layer_id: "L".into(),
            start_time,
            duration: Some(1.0),
        };
        // Nothing to join: the first merged step is a plain one.
        doc.apply_group_merging(&[retime(1.0)], true).unwrap();
        assert!(doc.can_undo());
        // A drag: the boundary stays where the drag began.
        doc.apply_group_merging(&[retime(2.0)], true).unwrap();
        doc.apply_group_merging(&[retime(3.0)], true).unwrap();
        assert_eq!(doc.version(), 3, "every apply is a version");
        assert!(doc.undo());
        assert_eq!(
            doc.to_json().unwrap(),
            start,
            "one Ctrl+Z undid the whole drag"
        );
        assert!(!doc.can_undo());
        assert!(doc.redo());
        let start_of = doc
            .meta()
            .layers
            .as_deref()
            .unwrap()
            .iter()
            .find(|l| l.id == "L")
            .unwrap()
            .start_time;
        assert!(
            (start_of - 3.0).abs() < 1e-9,
            "redo lands at the drag's end"
        );
        // A new step after redo, unmerged, then merged: two boundaries.
        doc.apply_group_merging(&[retime(4.0)], false).unwrap();
        doc.apply_group_merging(&[retime(5.0)], true).unwrap();
        assert!(doc.undo());
        assert!(doc.undo());
        assert_eq!(doc.to_json().unwrap(), start);
    }
    /// A whole-document replacement is one undo step that the log names
    /// by diff; the same content again is not a step.
    #[test]
    fn replacing_the_document_is_one_named_step() {
        let mut doc = retime_doc(
            serde_json::json!([
                {"id": "L", "name": "l", "sortIndex": 0, "kind": "video", "isEnabled": true, "startTime": 0, "duration": 4, "resourceID": "R", "keyframes": []}
            ]),
            serde_json::json!({"id": "R", "kind": "video", "filename": "v.mp4", "displayName": "v", "addedAt": 0, "duration": 4, "imageCuts": []}),
        );
        let start = doc.to_json().unwrap();
        assert!(!doc.replace(&start).unwrap(), "same content, no step");
        assert_eq!(doc.version(), 0);
        let mut edited: serde_json::Value = serde_json::from_str(&start).unwrap();
        edited["layers"][0]["name"] = serde_json::json!("renamed");
        assert!(doc.replace(&edited.to_string()).unwrap());
        assert_eq!(doc.version(), 1);
        let changes = doc.changes_since(0);
        assert_eq!(changes.layers, vec!["L".to_string()]);
        assert!(changes.resources.is_empty() && !changes.settings);
        assert!(doc.undo());
        assert_eq!(doc.to_json().unwrap(), start);
        assert!(
            doc.replace("{not json").is_err(),
            "the format's parser gates it"
        );
    }

    /// The Swift model's copy loses what it cannot read; the document keeps
    /// it through a replace, takes it in from the file without a step, and
    /// a save writes it back, stamped (review 2026-09-27, P2-45).
    #[test]
    fn what_the_swift_copy_cannot_read_survives_replace_and_save() {
        let file = serde_json::json!({
            "id": "P", "name": "P", "createdAt": 0, "state": "recorded", "trimStart": 0,
            "trimEnd": 0, "videoDuration": 0, "subtitles": [], "futureTop": "kept",
            "compositionSettings": {},
            "layers": [{ "id": "L", "name": "L", "sortIndex": 0, "kind": "image",
                         "isEnabled": true, "startTime": 0, "duration": 2,
                         "futureLayer": 1, "keyframes": [] }]
        });
        let mut lossy = file.clone();
        lossy.as_object_mut().unwrap().remove("futureTop");
        lossy["layers"][0]
            .as_object_mut()
            .unwrap()
            .remove("futureLayer");
        // Opened from the Swift copy, then given the file's unread keys:
        // no step for what nobody did.
        let mut doc = Document::open(&lossy.to_string()).unwrap();
        assert!(doc.absorb_unread(&file.to_string()).unwrap());
        assert!(!doc.can_undo(), "absorbing is not an edit");
        // A Swift-side rename replaces the document: the unread keys stay.
        lossy["layers"][0]["name"] = serde_json::json!("Renamed");
        assert!(doc.replace(&lossy.to_string()).unwrap());
        let now: serde_json::Value = serde_json::from_str(&doc.to_json().unwrap()).unwrap();
        assert_eq!(now["futureTop"], "kept");
        assert_eq!(now["layers"][0]["futureLayer"], 1);
        assert_eq!(now["layers"][0]["name"], "Renamed");
        // A save's timestamp alone is not a step.
        let mut saved_later = lossy.clone();
        saved_later["updatedAt"] = serde_json::json!(812_345_678.0);
        let steps = doc.can_undo();
        assert!(
            !doc.replace(&saved_later.to_string()).unwrap(),
            "no step for updatedAt"
        );
        assert_eq!(doc.can_undo(), steps);
        // And the save writes them, stamped by the core's ladder.
        let saved: serde_json::Value =
            serde_json::from_str(&doc.saved_json_from(&lossy.to_string()).unwrap()).unwrap();
        assert_eq!(saved["futureTop"], "kept");
        assert_eq!(saved["layers"][0]["futureLayer"], 1);
        assert!(saved["minReaderVersion"].as_u64().is_some());
    }

    /// The apps' save keeps what nobody reads at ANY depth: a key a newer
    /// build wrote inside a keyframe's placement is in the file after the
    /// Swift copy — which dropped it — is saved (review 2026-09-27, P3-44).
    #[test]
    fn the_apps_save_keeps_an_unread_key_deep_in_the_file() {
        let file = serde_json::json!({ "id": "P", "name": "P", "createdAt": 0,
            "state": "recorded", "trimStart": 0, "trimEnd": 0, "videoDuration": 0,
            "subtitles": [], "compositionSettings": {},
            "layers": [{ "id": "L", "name": "L", "sortIndex": 0, "kind": "image",
                         "isEnabled": true, "startTime": 0, "duration": 2,
                         "keyframes": [{ "id": "K", "time": 0, "transitionDuration": 0.5,
                                         "placement": { "anchor": "center", "future": 7 } }] }] });
        let mut lossy = file.clone();
        lossy["layers"][0]["keyframes"][0]["placement"]
            .as_object_mut()
            .unwrap()
            .remove("future");
        lossy["layers"][0]["name"] = serde_json::json!("Renamed");
        let mut doc = Document::open(&lossy.to_string()).unwrap();
        doc.absorb_unread(&file.to_string()).unwrap();
        let saved: serde_json::Value =
            serde_json::from_str(&doc.saved_json_from(&lossy.to_string()).unwrap()).unwrap();
        assert_eq!(saved["layers"][0]["keyframes"][0]["placement"]["future"], 7);
        assert_eq!(saved["layers"][0]["name"], "Renamed");
    }

    /// A file the format reads whole leaves no source behind (a library
    /// holds every project's document); one with anything unread keeps it,
    /// and a later file that dropped it lets it go.
    #[test]
    fn a_fully_read_file_keeps_no_source() {
        let plain = serde_json::json!({ "id": "P", "name": "P", "createdAt": 0,
            "state": "recorded", "trimStart": 0, "trimEnd": 0, "videoDuration": 0,
            "subtitles": [], "compositionSettings": {},
            "layers": [{ "id": "L", "name": "L", "sortIndex": 0, "kind": "image",
                         "isEnabled": true, "startTime": 0, "duration": 2, "keyframes": [] }] });
        let mut doc = Document::open(&plain.to_string()).unwrap();
        doc.absorb_unread(&plain.to_string()).unwrap();
        assert!(doc.unread_source.is_none() && doc.unread_source_json.is_none());

        let mut future = plain.clone();
        future["layers"][0]["future"] = serde_json::json!(1);
        doc.absorb_unread(&future.to_string()).unwrap();
        assert!(doc.unread_source.is_some() && doc.unread_source_json.is_some());

        doc.absorb_unread(&plain.to_string()).unwrap();
        assert!(
            doc.unread_source.is_none(),
            "the file dropped it: the source goes"
        );
        let saved: serde_json::Value =
            serde_json::from_str(&doc.saved_json_from(&plain.to_string()).unwrap()).unwrap();
        assert!(saved["layers"][0].get("future").is_none(), "{saved}");
    }

    /// Rung 48 through the apps' save: a file stamped for lossless readers
    /// that carries a key this build does not read stays stamped 48 though
    /// its features alone need less; once the entity holding the key is
    /// gone, the stamp comes back down.
    #[test]
    fn the_apps_save_keeps_the_lossless_stamp_while_it_carries_unread() {
        let file = serde_json::json!({ "id": "P", "name": "P", "createdAt": 0,
            "state": "recorded", "trimStart": 0, "trimEnd": 0, "videoDuration": 0,
            "subtitles": [], "compositionSettings": {}, "minReaderVersion": 48,
            "layers": [
                { "id": "L", "name": "L", "sortIndex": 0, "kind": "image",
                  "isEnabled": true, "startTime": 0, "duration": 2,
                  "keyframes": [{ "id": "K", "time": 0, "transitionDuration": 0.5,
                                  "placement": { "anchor": "center", "future": 7 } }] },
                { "id": "M", "name": "M", "sortIndex": 1, "kind": "image",
                  "isEnabled": true, "startTime": 0, "duration": 2, "keyframes": [] }] });
        let mut lossy = file.clone();
        lossy["layers"][0]["keyframes"][0]["placement"]
            .as_object_mut()
            .unwrap()
            .remove("future");
        lossy.as_object_mut().unwrap().remove("minReaderVersion");
        let mut doc = Document::open(&lossy.to_string()).unwrap();
        doc.absorb_unread(&file.to_string()).unwrap();
        let saved: serde_json::Value =
            serde_json::from_str(&doc.saved_json_from(&lossy.to_string()).unwrap()).unwrap();
        assert_eq!(saved["minReaderVersion"], 48);
        assert_eq!(saved["layers"][0]["keyframes"][0]["placement"]["future"], 7);

        let mut without = lossy.clone();
        without["layers"].as_array_mut().unwrap().remove(0);
        let saved: serde_json::Value =
            serde_json::from_str(&doc.saved_json_from(&without.to_string()).unwrap()).unwrap();
        assert!(saved["minReaderVersion"].as_i64().unwrap() < 48, "{saved}");
    }

    /// A writer that names only what it changed lands exactly where a whole
    /// replace would: the same document, one step, the unread keys of the
    /// swapped layer carried; a timestamp alone is no step; a layer the
    /// document lacks is refused (review 2026-09-27, P3-46).
    #[test]
    fn replacing_parts_lands_where_a_whole_replace_would() {
        let project = |name: &str, color: &str, updated: f64| {
            serde_json::json!({ "id": "P", "name": "P", "createdAt": 0, "state": "recorded",
                "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
                "updatedAt": updated,
                "compositionSettings": { "backgroundColorHex": color },
                "layers": [
                    { "id": "A", "name": name, "sortIndex": 0, "kind": "image",
                      "isEnabled": true, "startTime": 0, "duration": 2, "keyframes": [] },
                    { "id": "B", "name": "B", "sortIndex": 1, "kind": "image",
                      "isEnabled": true, "startTime": 0, "duration": 2, "keyframes": [] }
                ],
                "resources": [] })
        };
        let file = {
            let mut file = project("A", "000000", 1.0);
            file["layers"][0]["future"] = serde_json::json!(true);
            file
        };
        let mut whole = Document::open(&project("A", "000000", 1.0).to_string()).unwrap();
        whole.absorb_unread(&file.to_string()).unwrap();
        let mut parts = Document::open(&project("A", "000000", 1.0).to_string()).unwrap();
        parts.absorb_unread(&file.to_string()).unwrap();

        let edited = project("Title", "FF0000", 2.0);
        assert!(whole.replace(&edited.to_string()).unwrap());
        let changed = serde_json::json!({
            "layers": [edited["layers"][0].clone()],
            "compositionSettings": edited["compositionSettings"].clone(),
            "updatedAt": 2.0,
        });
        assert!(parts.replace_parts(&changed.to_string()).unwrap());
        assert_eq!(parts.to_json().unwrap(), whole.to_json().unwrap());
        let now: serde_json::Value = serde_json::from_str(&parts.to_json().unwrap()).unwrap();
        assert_eq!(now["layers"][0]["future"], true, "carried");
        assert!(parts.can_undo());

        let steps = parts.undo.len();
        let later = serde_json::json!({ "updatedAt": 3.0 });
        assert!(!parts.replace_parts(&later.to_string()).unwrap());
        assert_eq!(parts.undo.len(), steps, "a timestamp is no step");

        let stranger = serde_json::json!({ "layers": [{ "id": "Z", "name": "Z", "sortIndex": 0,
            "kind": "image", "isEnabled": true, "startTime": 0, "duration": 2, "keyframes": [] }] });
        assert!(parts.replace_parts(&stranger.to_string()).is_err());
        assert_eq!(parts.undo.len(), steps, "refused, untouched");
    }

    /// Another writer changed the file while the app had it open: the
    /// app's copy reads the new file, and the document takes the file's
    /// unread content — a key the writer removed stays gone through the
    /// app's next replace and save, and a layer the writer added keeps its
    /// unread keys whether the save comes before the app hands the layer
    /// in or after. A narration's speech the app removes is the app's
    /// removal (review 2026-09-27, P2-45).
    #[test]
    fn a_file_another_writer_changed_is_the_latest_word() {
        let layer = |id: &str, extra: serde_json::Value| {
            let mut layer = serde_json::json!({ "id": id, "name": id, "sortIndex": 0,
                "kind": "image", "isEnabled": true, "startTime": 0, "duration": 2,
                "keyframes": [] });
            for (key, value) in extra.as_object().unwrap() {
                layer[key] = value.clone();
            }
            layer
        };
        let project = |layers: Vec<serde_json::Value>, speech: bool| {
            let mut resource = serde_json::json!({ "id": "R", "kind": "audio",
                "filename": "a.mp3", "displayName": "A", "addedAt": 0 });
            if speech {
                resource["speech"] = serde_json::json!({ "text": "hello" });
            }
            serde_json::json!({ "id": "P", "name": "P", "createdAt": 0, "state": "recorded",
                "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
                "compositionSettings": {}, "layers": layers, "resources": [resource] })
        };
        // Opened: the file has an unread key on L.
        let file = project(vec![layer("L", serde_json::json!({ "future": 1 }))], true);
        let app = project(vec![layer("L", serde_json::json!({}))], true);
        let mut doc = Document::open(&app.to_string()).unwrap();
        doc.absorb_unread(&file.to_string()).unwrap();

        // Another writer removes L's key and adds M with one of its own.
        let file2 = project(
            vec![
                layer("L", serde_json::json!({})),
                layer("M", serde_json::json!({ "future": 2 })),
            ],
            true,
        );
        doc.absorb_unread(&file2.to_string()).unwrap();
        let app2 = project(
            vec![
                layer("L", serde_json::json!({})),
                layer("M", serde_json::json!({})),
            ],
            true,
        );
        // A save before the app's copy reached the document.
        let saved: serde_json::Value =
            serde_json::from_str(&doc.saved_json_from(&app2.to_string()).unwrap()).unwrap();
        assert!(saved["layers"][0].get("future").is_none(), "{saved}");
        assert_eq!(saved["layers"][1]["future"], 2, "{saved}");
        // The app hands its copy in, then saves.
        doc.replace(&app2.to_string()).unwrap();
        let now: serde_json::Value = serde_json::from_str(&doc.to_json().unwrap()).unwrap();
        assert!(now["layers"][0].get("future").is_none(), "{now}");
        assert_eq!(now["layers"][1]["future"], 2, "{now}");

        // The app removes the narration's speech: it stays removed.
        let app3 = project(
            vec![
                layer("L", serde_json::json!({})),
                layer("M", serde_json::json!({})),
            ],
            false,
        );
        doc.replace(&app3.to_string()).unwrap();
        let saved: serde_json::Value =
            serde_json::from_str(&doc.saved_json_from(&app3.to_string()).unwrap()).unwrap();
        assert!(saved["resources"][0].get("speech").is_none(), "{saved}");
        assert_eq!(saved["layers"][1]["future"], 2);
    }
    /// A command addressed inside a composition edits the composition's
    /// layers with every rule the document has — rename, retime, delete's
    /// renumbering — and the change log names the resource. An edit that
    /// would make the composition contain itself is refused.
    #[test]
    fn commands_inside_a_composition_edit_its_layers_and_name_the_resource() {
        let json = serde_json::json!({
            "id": "P", "name": "p", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
            "compositionSettings": {"canvasWidth": 1920, "canvasHeight": 1080},
            "resources": [
                {"id": "cap", "kind": "caption", "filename": "", "displayName": "w", "addedAt": 0, "captionText": "hi", "imageCuts": []},
                {"id": "C", "kind": "composition", "filename": "", "displayName": "Card", "addedAt": 0, "duration": 4,
                 "pixelWidth": 1280, "pixelHeight": 720, "imageCuts": [],
                 "composition": {"canvasWidth": 1280, "canvasHeight": 720, "layers": [
                    {"id": "A", "name": "a", "sortIndex": 0, "kind": "caption", "isEnabled": true, "startTime": 0, "duration": 2, "resourceID": "cap", "keyframes": []},
                    {"id": "B", "name": "b", "sortIndex": 1, "kind": "caption", "isEnabled": true, "startTime": 0, "duration": 2, "resourceID": "cap",
                     "timing": {"start": {"from": "previousEnd", "offset": 0.5}}, "keyframes": []},
                    {"id": "D", "name": "d", "sortIndex": 2, "kind": "caption", "isEnabled": true, "startTime": 1, "duration": 1, "resourceID": "cap", "keyframes": []}
                 ]}}
            ],
            "layers": [
                {"id": "P1", "name": "card", "sortIndex": 0, "kind": "video", "isEnabled": true, "startTime": 0, "duration": 4, "resourceID": "C", "keyframes": []}
            ]
        });
        let mut doc = Document::open(&json.to_string()).unwrap();
        let inside = |command: serde_json::Value| Command::InComposition {
            resource_id: "C".into(),
            command: Box::new(serde_json::from_value(command).unwrap()),
        };
        let nested = |doc: &Document| -> Vec<promo_model::ProjectLayer> {
            doc.meta().resources.as_deref().unwrap()[1]
                .composition
                .as_ref()
                .unwrap()
                .layers
                .clone()
        };

        doc.apply(&inside(
            serde_json::json!({"kind": "renameLayer", "layerID": "A", "name": "renamed"}),
        ))
        .unwrap();
        assert_eq!(nested(&doc)[0].name, "renamed");
        let changes = doc.changes_since(0);
        assert_eq!(
            changes.resources,
            vec!["C".to_string()],
            "the resource is what changed"
        );
        assert!(changes.layers.is_empty(), "no top-level layer was touched");

        // A retime inside: the anchored follower moves with it.
        doc.apply(&inside(serde_json::json!({"kind": "setLayerTiming", "layerID": "A", "startTime": 1.0, "duration": 3.0})))
            .unwrap();
        let layers = nested(&doc);
        assert!(
            (layers[1].start_time - 4.5).abs() < 1e-9,
            "B follows A's new end: {}",
            layers[1].start_time
        );

        // Delete renumbers the survivors inside.
        doc.apply(&inside(
            serde_json::json!({"kind": "deleteLayer", "layerID": "A"}),
        ))
        .unwrap();
        let layers = nested(&doc);
        assert_eq!(layers.len(), 2);
        assert_eq!(
            layers.iter().map(|l| l.sort_index).collect::<Vec<_>>(),
            vec![0, 1]
        );

        // One history: ⌘Z walks the nested edits back through the resource.
        assert!(doc.undo());
        assert_eq!(nested(&doc).len(), 3);

        // The composition may not be made to contain itself.
        let err = doc
            .apply(&inside(serde_json::json!({"kind": "updateLayer", "layerID": "D", "patch": {"resourceID": "C"}})))
            .unwrap_err();
        assert!(err.contains("contains itself"), "{err}");
        assert!(doc
            .apply(&Command::InComposition {
                resource_id: "cap".into(),
                command: Box::new(
                    serde_json::from_value(
                        serde_json::json!({"kind": "renameLayer", "layerID": "A", "name": "x"})
                    )
                    .unwrap()
                ),
            })
            .unwrap_err()
            .contains("not a composition"));
    }

    #[test]
    fn set_markers_sorts_logs_a_project_change_and_refuses_bad_ones() {
        let mut doc = doc();
        let markers: Vec<promo_model::Marker> = serde_json::from_value(serde_json::json!([
            {"id": "M2", "time": 4.0, "name": "Two", "kind": "chapter"},
            {"id": "M1", "time": 1.5, "name": "One"}
        ]))
        .unwrap();
        doc.apply(&Command::SetMarkers {
            markers: markers.clone(),
        })
        .unwrap();
        let ids: Vec<&str> = doc
            .meta()
            .markers
            .as_deref()
            .unwrap()
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(ids, vec!["M1", "M2"], "sorted by time on the way in");
        let c = doc.changes_since(0);
        assert!(
            c.project && c.layers.is_empty() && !c.settings && !c.order,
            "a marker edit is a project-level change: {c:?}"
        );
        // Undo restores the empty list, and the wire round-trips the kind.
        assert!(doc.undo());
        assert!(doc.meta().markers.is_none());
        assert!(doc.redo());
        assert_eq!(
            doc.meta().markers.as_deref().unwrap()[1].kind,
            promo_model::MarkerKind::Chapter
        );
        // Refusals: a negative time, a repeated id, an empty id.
        for bad in [
            serde_json::json!([{"id": "X", "time": -1.0}]),
            serde_json::json!([{"id": "X", "time": 1.0}, {"id": "X", "time": 2.0}]),
            serde_json::json!([{"id": " ", "time": 1.0}]),
        ] {
            let markers: Vec<promo_model::Marker> = serde_json::from_value(bad).unwrap();
            assert!(doc.apply(&Command::SetMarkers { markers }).is_err());
        }
        assert_eq!(
            doc.meta().markers.as_deref().unwrap().len(),
            2,
            "a refusal changes nothing"
        );
        // Empty clears the field rather than writing [].
        doc.apply(&Command::SetMarkers { markers: vec![] }).unwrap();
        assert!(doc.meta().markers.is_none());
        // The wire name.
        let cmd: Command = serde_json::from_value(serde_json::json!({
            "kind": "setMarkers", "markers": [{"id": "A", "time": 0.5, "name": "a"}]
        }))
        .unwrap();
        assert!(matches!(cmd, Command::SetMarkers { .. }));
    }

    /// A stage layer's members (rung 33) take layer commands by their own
    /// id: a keyframe lands on a member, a rename too, and deleting a member
    /// drops it from its stage while the stage and its siblings stay.
    #[test]
    fn a_stage_member_takes_layer_commands_by_its_id() {
        let raw = r#"{"id":"AAAAAAAA-0000-0000-0000-00000000AAAA","name":"v","createdAt":0,
            "state":"recorded","trimStart":0,"trimEnd":4,"videoDuration":4,"subtitles":[],
            "compositionSettings":{"canvasWidth":1920,"canvasHeight":1080},
            "resources":[{"id":"M","kind":"model","filename":"b.glb","displayName":"B","addedAt":0}],
            "layers":[
              {"id":"S","name":"bench","sortIndex":0,"kind":"stage","isEnabled":true,
               "startTime":0,"duration":4,"keyframes":[],
               "members":[
                 {"id":"A","name":"Left","sortIndex":0,"kind":"model","isEnabled":true,
                  "startTime":0,"duration":4,"resourceID":"M","keyframes":[]},
                 {"id":"B","name":"Right","sortIndex":1,"kind":"model","isEnabled":true,
                  "startTime":0,"duration":4,"resourceID":"M","keyframes":[]}]},
              {"id":"T","name":"Title","sortIndex":1,"kind":"caption","isEnabled":true,
               "startTime":0,"duration":4,"captionText":"Hi","keyframes":[]}]}"#;
        let mut doc = Document::open(raw).unwrap();
        doc.apply(&Command::RenameLayer {
            layer_id: "B".into(),
            name: "Far right".into(),
        })
        .expect("a member renames");
        let keyframe: promo_model::ProjectLayerKeyframe = serde_json::from_str(
            r#"{"id":"K","time":1.0,"transitionDuration":0,"stageOffset":[1.5,0]}"#,
        )
        .unwrap();
        doc.apply(&Command::UpsertKeyframe {
            layer_id: "B".into(),
            keyframe: Box::new(keyframe),
        })
        .expect("a member takes a keyframe");
        let stage = &doc.meta.layers.as_ref().unwrap()[0];
        let members = stage.members.as_ref().unwrap();
        assert_eq!(members[1].name, "Far right");
        assert_eq!(members[1].keyframes.len(), 1);
        doc.apply(&Command::DeleteLayer {
            layer_id: "A".into(),
        })
        .expect("a member deletes");
        let layers = doc.meta.layers.as_ref().unwrap();
        assert_eq!(layers.len(), 2, "the stage and the title stay");
        let members = layers[0].members.as_ref().unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].id, "B");
        assert_eq!(members[0].sort_index, 0, "the survivors close the gap");
        assert!(doc
            .apply(&Command::DeleteLayer {
                layer_id: "Z".into()
            })
            .is_err());
    }
}
