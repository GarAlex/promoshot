//! A project in several languages (core-private docs/LOCALIZATION.md).
//!
//! The project lists its languages in `languages`, and a resource carries
//! what differs per language in `localized`: one merge patch per language
//! over the resource itself, so anything that is a resource's parameter can
//! differ and nothing else can. Layers, keyframes and the timeline stay
//! shared.
//!
//! [`flatten`] turns "this project in German" into an ordinary project
//! before timing is resolved, so nothing downstream learns languages exist.
//! Both keys live in the bags, as `speech` does, and an older lossless
//! reader ignores them and shows the source, which is the right fallback.

use serde_json::{Map, Value};

use crate::{ProjectMetadata, ProjectResource};

/// The project's key, `{"source": "en", "targets": ["de", "ja"]}`.
pub const LANGUAGES_KEY: &str = "languages";
/// The resource's key, `{"de": {…patch…}, "ja": {…patch…}}`.
pub const LOCALIZED_KEY: &str = "localized";

/// What a layer's pointer and the file's bookkeeping rest on: a language
/// may not change them.
const IDENTITY_KEYS: &[&str] = &["id", "kind", "addedAt", "libraryID", LOCALIZED_KEY];

/// About the entry, not the resource: which source text it was made from
/// and who wrote it. Dropped rather than patched.
const ENTRY_KEYS: &[&str] = &["sourceHash", "by"];

/// Facts about one file. An entry naming another file without restating
/// them gets none, not the source file's: trims are seconds of the file
/// they cut, a size is the size of one picture.
const FILE_BOUND_KEYS: &[&str] = &[
    "duration",
    "trimStart",
    "trimEnd",
    "trimKeyframes",
    "mediaCuts",
    "imageCuts",
    "pointer",
    "activity",
    "pixelWidth",
    "pixelHeight",
    "videoNaturalWidth",
    "videoNaturalHeight",
];

/// Facts about one take's bytes, in `speech`: a language's own script makes
/// a new take, and these say nothing about it unless its entry says them.
const TAKE_BOUND_SPEECH: &[&str] = &["renderedHash", "placeholder", "wordFixes"];

/// The project's languages, as `languages` says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Languages {
    pub source: String,
    pub targets: Vec<String>,
}

impl Languages {
    /// Source first, then each target.
    pub fn all(&self) -> Vec<&str> {
        std::iter::once(self.source.as_str())
            .chain(self.targets.iter().map(String::as_str))
            .collect()
    }
}

/// RFC 7386: only the keys named change, objects merge, `null` removes.
pub fn merge_patch(target: &mut Value, patch: &Value) {
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

/// The list in the file; `None` when it has none or it cannot be read
/// ([`problems`] says which).
pub fn languages(meta: &ProjectMetadata) -> Option<Languages> {
    let value = meta.extra.get(LANGUAGES_KEY)?;
    let source = value.get("source")?.as_str()?.to_string();
    let targets = value
        .get("targets")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Some(Languages { source, targets })
}

fn entries(resource: &ProjectResource) -> Option<&Map<String, Value>> {
    resource.extra.get(LOCALIZED_KEY)?.as_object()
}

/// `resource` with `entry` applied. A bad entry is an error, never half a
/// resource.
fn patched(resource: &ProjectResource, entry: &Value) -> Result<ProjectResource, String> {
    let Some(patch) = entry.as_object() else {
        return Err("its entry is not an object".to_string());
    };
    if let Some(key) = IDENTITY_KEYS.iter().find(|key| patch.contains_key(**key)) {
        return Err(format!(
            "\"{key}\" cannot differ per language: a layer points at the resource by it"
        ));
    }
    let mut doc = serde_json::to_value(resource).map_err(|e| e.to_string())?;
    let map = doc.as_object_mut().expect("a resource is an object");
    map.remove(LOCALIZED_KEY);
    let new_file = patch
        .get("filename")
        .and_then(Value::as_str)
        .is_some_and(|name| name != resource.filename);
    if new_file {
        for key in FILE_BOUND_KEYS {
            map.remove(*key);
        }
        // The language's file is the project's, wherever the source's lives.
        map.remove("libraryID");
    }
    let mut patch = patch.clone();
    for key in ENTRY_KEYS {
        patch.remove(*key);
    }
    // A voice chosen before there is a take changes nothing yet, and words
    // with no file of their own say nothing about what plays: the source's
    // take plays, so its words are the ones anchors and captions count.
    if new_file && entry_script(entry).is_some() {
        if let Some(speech) = map.get_mut("speech").and_then(Value::as_object_mut) {
            for key in TAKE_BOUND_SPEECH {
                speech.remove(*key);
            }
        }
    } else {
        patch.remove("speech");
    }
    let source_overrides = doc["svg"]["overrides"].as_array().cloned();
    merge_patch(&mut doc, &Value::Object(patch));
    // A list has no keys for a merge patch to name, and a language answers
    // element by element: the ones it says nothing about keep the source's.
    if let Some(mut merged) = source_overrides {
        if let Some(own) = entry["svg"]["overrides"].as_array() {
            for said in own {
                match merged.iter_mut().find(|o| o["node"] == said["node"]) {
                    Some(override_) => merge_patch(override_, said),
                    None => merged.push(said.clone()),
                }
            }
            doc["svg"]["overrides"] = Value::Array(merged);
        }
    }
    serde_json::from_value(doc).map_err(|e| e.to_string())
}

/// The words a language's entry has its narration say.
fn entry_script(entry: &Value) -> Option<&str> {
    entry["speech"]["text"]
        .as_str()
        .filter(|text| !text.trim().is_empty())
}

/// The files a resource's languages name, each once: a replacement, and any
/// cut of it that has a file of its own.
pub fn files(resource: &ProjectResource) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for entry in entries(resource).into_iter().flat_map(Map::values) {
        let cuts = entry["imageCuts"].as_array().into_iter().flatten();
        for name in std::iter::once(&entry["filename"])
            .chain(cuts.map(|cut| &cut["filename"]))
            .filter_map(Value::as_str)
            .filter(|name| !name.is_empty())
        {
            if !names.iter().any(|known| known == name) {
                names.push(name.to_string());
            }
        }
    }
    names
}

/// The language whose entry on `resource` points at resource `id`: its own
/// composition, the picture its slots wear. What a delete has to ask, because
/// a pointer that only one language has is still a pointer.
pub fn language_pointing_at(resource: &ProjectResource, id: &str) -> Option<String> {
    fn points(node: &Value, parent: Option<&str>, id: &str) -> bool {
        match node {
            Value::Object(map) => map.iter().any(|(key, value)| {
                let pointer = key == "resourceID"
                    || key.ends_with("ResourceID")
                    || key == "themePlateID"
                    || (parent == Some("morph") && (key == "from" || key == "to"));
                (pointer && value.as_str() == Some(id)) || points(value, Some(key), id)
            }),
            Value::Array(items) => items.iter().any(|item| points(item, parent, id)),
            _ => false,
        }
    }
    entries(resource)?
        .iter()
        .find(|(_, entry)| points(entry, None, id))
        .map(|(language, _)| language.clone())
}

/// The composition a language has of its own on `resource`, whole: what its
/// entry says over the source's. `None` when it shares the source's.
pub fn own_composition(resource: &ProjectResource, language: &str) -> Option<crate::Composition> {
    let entry = entries(resource)?.get(language)?;
    entry.get("composition")?;
    patched(resource, entry).ok()?.composition
}

/// Gives `language` its own copy of a composition, as that language shows it
/// now: words and sizes already its own. Nothing is translated into a copy a
/// language owns, so this is the one moment they carry over.
pub fn fork_composition(
    meta: &mut ProjectMetadata,
    resource_id: &str,
    language: &str,
    by: &str,
) -> Result<(), String> {
    let mut flat = meta.clone();
    flatten(&mut flat, language)?;
    let composition = flat
        .resources
        .iter()
        .flatten()
        .find(|r| r.id == resource_id)
        .and_then(|r| r.composition.as_ref())
        .ok_or_else(|| format!("resource {resource_id} is not a composition"))?;
    let patch = serde_json::json!({ "composition": composition });
    set_translation(
        meta,
        TranslationTarget::Resource(resource_id),
        language,
        &patch,
        by,
    )
}

/// Writes `composition` as what `language` owns on `resource_id`: what an edit
/// inside that language's own content changes.
pub fn set_own_composition(
    meta: &mut ProjectMetadata,
    resource_id: &str,
    language: &str,
    composition: &crate::Composition,
) -> Result<(), String> {
    let resource = meta
        .resources
        .iter_mut()
        .flatten()
        .find(|r| r.id == resource_id)
        .ok_or_else(|| format!("no resource with id {resource_id}"))?;
    let Some(entry) = resource
        .extra
        .get_mut(LOCALIZED_KEY)
        .and_then(|all| all.get_mut(language))
        .filter(|entry| entry.get("composition").is_some())
    else {
        return Err(format!(
            "resource {resource_id} has no composition of its own in \"{language}\""
        ));
    };
    entry["composition"] = serde_json::to_value(composition).map_err(|e| e.to_string())?;
    Ok(())
}

/// The targets that name a file or a composition of their own, so a question
/// about the source's media says nothing about them.
pub fn languages_with_media(meta: &ProjectMetadata) -> Vec<String> {
    let Some(list) = languages(meta) else {
        return Vec::new();
    };
    list.targets
        .into_iter()
        .filter(|language| {
            meta.resources.iter().flatten().any(|resource| {
                entries(resource)
                    .and_then(|all| all.get(language))
                    .is_some_and(|e| e.get("filename").is_some() || e.get("composition").is_some())
            })
        })
        .collect()
}

/// A sound's playing length, the way the timeline reads it.
fn played_length(resource: &ProjectResource) -> f64 {
    crate::video_trim_ranges(resource)
        .iter()
        .map(|range| range.duration())
        .sum()
}

/// Each language keeps its own length: a layer that played the whole take,
/// clip or composition plays the whole of this language's, the rule a trim
/// follows.
fn follow_takes(layers: &mut [crate::ProjectLayer], lengths: &[(String, f64, f64)]) {
    let mut moved = false;
    for layer in layers.iter_mut() {
        let id = layer.resource_id.as_deref();
        if let Some((_, old, new)) = lengths
            .iter()
            .find(|(known, _, _)| Some(known.as_str()) == id)
        {
            layer.follow_length(*old, *new);
            moved = true;
        }
        if let Some(members) = layer.members.as_mut() {
            follow_takes(members, lengths);
        }
    }
    if moved {
        crate::extend_background_over_content(layers);
    }
}

/// The size a caption resource says, for telling what a language changed.
fn caption_size(resource: &ProjectResource, default: f64) -> f64 {
    resource
        .caption_style
        .as_ref()
        .and_then(|style| style.font_size)
        .unwrap_or(default)
}

/// A keyframe's size outranks the style's, and the app stamps one on any
/// caption dragged on the canvas. So a language's size is a ratio over its
/// layers' keyframes: a title animating 50 to 60 at a German 40 where the
/// source says 50 animates 40 to 48.
fn scale_keyframe_sizes(
    layers: &mut [crate::ProjectLayer],
    ratios: &std::collections::BTreeMap<String, f64>,
) {
    for layer in layers {
        if let Some(ratio) = layer.resource_id.as_ref().and_then(|id| ratios.get(id)) {
            for keyframe in &mut layer.keyframes {
                keyframe.font_size = keyframe.font_size.map(|size| size * ratio);
                keyframe.zoom = keyframe.zoom.map(|size| size * ratio);
            }
        }
        if let Some(members) = layer.members.as_mut() {
            scale_keyframe_sizes(members, ratios);
        }
    }
}

/// `meta` as an ordinary project in `language`: each resource's entry for
/// it applied, and every `localized` gone. The source language only loses
/// the blocks. A language not on the project's list is refused, as is any
/// entry that cannot apply: a render never shows half a translation.
pub fn flatten(meta: &mut ProjectMetadata, language: &str) -> Result<(), String> {
    flatten_noting(meta, language).map(|_| ())
}

/// [`flatten`], answering what validate lists: each reference to a word that
/// landed by its place alone, and what else the language's words leave
/// approximate.
pub fn flatten_noting(meta: &mut ProjectMetadata, language: &str) -> Result<Vec<String>, String> {
    let list = languages(meta);
    let known = list.as_ref().map(Languages::all).unwrap_or_default();
    if !known.contains(&language) {
        return Err(if known.is_empty() {
            format!("\"{language}\": this project lists no languages")
        } else {
            format!(
                "\"{language}\" is not one of this project's languages ({})",
                known.join(", ")
            )
        });
    }
    let is_source = list.is_some_and(|l| l.source == language);
    let default_size = meta.composition_settings.subtitle_font_size;
    let mut ratios = std::collections::BTreeMap::new();
    let mut lengths = Vec::new();
    let mut maps = std::collections::BTreeMap::new();
    let mut notes = Vec::new();
    // Written in this language already: nothing below is translated into it.
    let mut own = std::collections::BTreeSet::new();
    for resource in meta.resources.iter_mut().flatten() {
        let entry = entries(resource)
            .filter(|_| !is_source)
            .and_then(|all| all.get(language))
            .cloned();
        match entry {
            Some(entry) => {
                let id = resource.id.clone();
                if entry.get("composition").is_some() {
                    own.insert(id.clone());
                }
                let size = caption_size(resource, default_size);
                let length = played_length(resource);
                let script = crate::speech::take_script(resource);
                let filename = resource.filename.clone();
                *resource = patched(resource, &entry)
                    .map_err(|why| format!("resource \"{id}\", language \"{language}\": {why}"))?;
                let own_file = resource.filename != filename;
                if own_file && entry_script(&entry).is_some() {
                    if let Some(speech) = resource.extra.get_mut("speech") {
                        speech["language"] = Value::from(language);
                    }
                } else if own_file && script.is_some() {
                    notes.push(format!(
                        "narration \"{}\" has its own file and no words here, so anchors and \
                         captions count the source's",
                        resource.display_name
                    ));
                }
                if let (Some(source), Some(here)) = (&script, crate::speech::take_script(resource))
                {
                    if *source != here {
                        if let Some(map) = crate::language_words::TakeMap::new(source, &here) {
                            maps.insert(id.clone(), map);
                        }
                    }
                }
                let now_length = played_length(resource);
                if matches!(
                    resource.kind,
                    crate::ProjectResourceKind::Audio
                        | crate::ProjectResourceKind::Video
                        | crate::ProjectResourceKind::Composition
                ) && !resource.is_looped()
                    && length > 0.0
                    && now_length > 0.0
                    && (now_length - length).abs() > 0.000_1
                {
                    lengths.push((id.clone(), length, now_length));
                }
                let now = caption_size(resource, default_size);
                if resource.kind == crate::ProjectResourceKind::Caption && now != size && size > 0.0
                {
                    ratios.insert(id, now / size);
                }
            }
            None => {
                resource.extra.remove(LOCALIZED_KEY);
            }
        }
    }
    if !lengths.is_empty() {
        follow_takes(meta.layers.as_deref_mut().unwrap_or_default(), &lengths);
        for resource in meta.resources.iter_mut().flatten() {
            if let Some(composition) = resource
                .composition
                .as_mut()
                .filter(|_| !own.contains(&resource.id))
            {
                follow_takes(&mut composition.layers, &lengths);
            }
        }
    }
    if !ratios.is_empty() {
        scale_keyframe_sizes(meta.layers.as_deref_mut().unwrap_or_default(), &ratios);
        for resource in meta.resources.iter_mut().flatten() {
            if let Some(composition) = resource
                .composition
                .as_mut()
                .filter(|_| !own.contains(&resource.id))
            {
                scale_keyframe_sizes(&mut composition.layers, &ratios);
            }
        }
    }
    notes.extend(crate::language_words::follow(meta, &maps, &own));
    // A composition a language made its own can reach back into itself where
    // the source's cannot, and recursion only stops on a finite nesting.
    if !own.is_empty() {
        if let Some(problem) = crate::nesting::problems(meta)
            .into_iter()
            .find(|p| p.contains("contains itself") || p.contains("nests deeper than"))
        {
            return Err(format!("language \"{language}\": {problem}"));
        }
    }
    Ok(notes)
}

/// What is wrong with the file's languages: `(breaks, warnings)`. A break
/// is what [`flatten`] refuses to render.
pub fn problems(meta: &ProjectMetadata) -> (Vec<String>, Vec<String>) {
    let mut breaks = Vec::new();
    let mut warnings = Vec::new();
    let list = languages(meta);
    if meta.extra.contains_key(LANGUAGES_KEY) && list.is_none() {
        breaks.push("languages: expected {\"source\": \"en\", \"targets\": [\"de\"]}".to_string());
    }
    let known = list.as_ref().map(Languages::all).unwrap_or_default();
    for resource in meta.resources.iter().flatten() {
        let Some(value) = resource.extra.get(LOCALIZED_KEY) else {
            continue;
        };
        let Some(all) = value.as_object() else {
            breaks.push(format!(
                "resource \"{}\": localized: expected an object of languages",
                resource.id
            ));
            continue;
        };
        for (language, entry) in all {
            if let Err(why) = patched(resource, entry) {
                breaks.push(format!(
                    "resource \"{}\", language \"{language}\": {why}",
                    resource.id
                ));
            }
            if !known.contains(&language.as_str()) {
                warnings.push(format!(
                    "resource \"{}\" has content for \"{language}\", which is not on the \
                     project's list of languages: it never shows",
                    resource.id
                ));
            }
        }
    }
    // What only the whole language can say: its own compositions together.
    for language in known.iter().skip(1) {
        let owns = meta.resources.iter().flatten().any(|r| {
            entries(r).is_some_and(|all| {
                all.get(*language)
                    .is_some_and(|entry| entry.get("composition").is_some())
            })
        });
        if owns && breaks.is_empty() {
            if let Err(why) = flatten(&mut meta.clone(), language) {
                breaks.push(why);
            }
        }
    }
    if list.is_some() {
        let captions = meta
            .resources
            .iter()
            .flatten()
            .filter(|r| r.caption_text.is_some());
        let on_a_resource: std::collections::BTreeSet<&str> =
            captions.map(|r| r.id.as_str()).collect();
        for layer in crate::nesting::all_layers(meta) {
            let on_resource = layer
                .resource_id
                .as_deref()
                .is_some_and(|id| on_a_resource.contains(id));
            // A generated caption's words come from its narration's script.
            if layer.caption_text.is_some() && !on_resource && layer.generated_by.is_none() {
                warnings.push(format!(
                    "caption layer \"{}\" keeps its text on the layer, so it shows the source in \
                     every language: move the text to a caption resource",
                    layer.id
                ));
            }
        }
    }
    (breaks, warnings)
}

/// What a language's replaced files leave for someone to look at: a picture of
/// another shape under a layer that says fixed numbers, a clip with no length,
/// and the trims and cuts that do not carry over to another file.
pub fn replacements(meta: &ProjectMetadata, language: &str) -> Vec<String> {
    fn aspect(resource: &ProjectResource) -> Option<f64> {
        let (width, height) = match (resource.pixel_width, resource.pixel_height) {
            (Some(width), Some(height)) => (width, height),
            _ => (
                resource.video_natural_width?,
                resource.video_natural_height?,
            ),
        };
        (height > 0.0).then(|| width / height)
    }
    let mut lines = Vec::new();
    let layers = crate::nesting::all_layers(meta);
    for resource in meta.resources.iter().flatten() {
        let Some(entry) = entries(resource).and_then(|all| all.get(language)) else {
            continue;
        };
        let Ok(here) = patched(resource, entry) else {
            continue;
        };
        if here.filename == resource.filename {
            continue;
        }
        let name = &resource.display_name;
        let playing = || {
            layers
                .iter()
                .filter(|l| l.resource_id.as_deref() == Some(resource.id.as_str()))
        };
        if let (Some(source), Some(own)) = (aspect(resource), aspect(&here)) {
            let fixed = playing()
                .filter(|l| {
                    l.keyframes.iter().any(|k| {
                        k.placement.is_none()
                            && (k.zoom.is_some()
                                || k.vertical_shift.is_some()
                                || k.horizontal_shift.is_some())
                    })
                })
                .count();
            if fixed > 0 && (source / own - 1.0).abs() > 0.01 {
                lines.push(format!(
                    "\"{name}\" has another shape here ({own:.2} wide for its height, {source:.2} \
                     in the source), and {fixed} layer(s) place it by fixed numbers: a placement \
                     rule fits either"
                ));
            }
        }
        if resource.kind == crate::ProjectResourceKind::Video && here.duration.is_none() {
            lines.push(format!(
                "\"{name}\" names its own file and no duration, so nothing knows how long it \
                 plays"
            ));
        }
        let trimmed = resource.trim_start.is_some_and(|t| t > 0.0)
            || resource.trim_end.is_some_and(|t| t > 0.0)
            || resource
                .trim_keyframes
                .as_ref()
                .is_some_and(|k| !k.is_empty());
        let says_trims = ["trimStart", "trimEnd", "trimKeyframes"]
            .iter()
            .any(|key| entry.get(*key).is_some());
        if trimmed && !says_trims {
            lines.push(format!(
                "\"{name}\" is trimmed in the source, and its own file here plays whole: a trim \
                 is seconds of the file it cuts"
            ));
        }
        for layer in playing() {
            let lost = layer
                .image_cut_id
                .as_ref()
                .filter(|id| !here.image_cuts.iter().any(|cut| &cut.id == *id))
                .or_else(|| {
                    layer
                        .media_cut_id
                        .as_ref()
                        .filter(|id| !here.media_cuts.iter().any(|cut| &cut.id == *id))
                });
            if lost.is_some() {
                lines.push(format!(
                    "layer \"{}\" plays a cut of \"{name}\" that its own file here does not \
                     have, so it plays the whole file",
                    layer.name
                ));
            }
        }
    }
    lines
}

/// The strings of a resource that a translation answers for: `(key, text)`,
/// the key as [`StringRow::key`] spells it. A resource's own words, not
/// what plays them. A generated caption has none: it is made again from
/// the narration, which is what gets translated.
fn source_strings(resource: &ProjectResource) -> Vec<(String, String)> {
    if resource.generated_by.is_some() {
        return Vec::new();
    }
    let doc = serde_json::to_value(resource).unwrap_or(Value::Null);
    strings_of(&doc)
}

fn strings_of(doc: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut push = |key: String, text: Option<&str>| {
        if let Some(text) = text.filter(|t| !t.trim().is_empty()) {
            out.push((key, text.to_string()));
        }
    };
    push("captionText".into(), doc["captionText"].as_str());
    push("recipe".into(), doc["recipe"]["text"]["text"].as_str());
    push("speech".into(), doc["speech"]["text"].as_str());
    if let Some(overrides) = doc["svg"]["overrides"].as_array() {
        for entry in overrides {
            if let Some(node) = entry["node"].as_str() {
                push(format!("svg:{node}"), entry["text"].as_str());
            }
        }
    }
    out
}

/// What an entry's `sourceHash` is computed over: every string the
/// resource says, so editing any of them marks the entry stale.
pub fn source_hash(resource: &ProjectResource) -> String {
    let texts: Vec<String> = source_strings(resource)
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    crate::speech::script_hash(&texts.join("\n"))
}

/// Where a language stands on one string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StringState {
    /// Nothing written: the source shows.
    Missing,
    /// Written from a source text that has changed since. It still shows.
    Stale,
    Current,
}

/// One language's answer for one string.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Translation {
    pub language: String,
    pub state: StringState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}

/// One translatable string and every target language's answer: what an
/// agent translates from, and what the apps' table shows.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StringRow {
    /// The resource it lives on. A caption keeping its text on a layer has
    /// none until it is translated, and names its `layer`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    /// `captionText`, `recipe`, `speech`, or `svg:<node>`.
    pub key: String,
    pub source: String,
    pub translations: Vec<Translation>,
}

/// Every translatable string in the project, in file order.
pub fn strings(meta: &ProjectMetadata) -> Vec<StringRow> {
    let targets = languages(meta).map(|l| l.targets).unwrap_or_default();
    let mut rows = Vec::new();
    for resource in meta.resources.iter().flatten() {
        let hash = source_hash(resource);
        let all = entries(resource);
        for (key, source) in source_strings(resource) {
            let translations = targets
                .iter()
                .map(|language| {
                    let entry = all.and_then(|a| a.get(language));
                    let text = entry.and_then(|e| {
                        strings_of(e)
                            .into_iter()
                            .find(|(k, _)| *k == key)
                            .map(|(_, text)| text)
                    });
                    // An entry with no hash was written by hand: nothing to
                    // compare it to.
                    let stale = entry
                        .and_then(|e| e.get("sourceHash"))
                        .and_then(Value::as_str)
                        .is_some_and(|written| written != hash);
                    Translation {
                        language: language.clone(),
                        state: match (&text, stale) {
                            (None, _) => StringState::Missing,
                            (Some(_), true) => StringState::Stale,
                            (Some(_), false) => StringState::Current,
                        },
                        by: entry
                            .filter(|_| text.is_some())
                            .and_then(|e| e.get("by"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        text,
                    }
                })
                .collect();
            rows.push(StringRow {
                resource: Some(resource.id.clone()),
                layer: None,
                key,
                source,
                translations,
            });
        }
    }
    let on_a_resource: std::collections::BTreeSet<&str> = meta
        .resources
        .iter()
        .flatten()
        .filter(|r| r.caption_text.is_some())
        .map(|r| r.id.as_str())
        .collect();
    for layer in crate::nesting::all_layers(meta) {
        let on_resource = layer
            .resource_id
            .as_deref()
            .is_some_and(|id| on_a_resource.contains(id));
        let generated = layer.generated_by.is_some();
        if let Some(text) = layer
            .caption_text
            .as_ref()
            .filter(|_| !on_resource && !generated)
        {
            rows.push(StringRow {
                resource: None,
                layer: Some(layer.id.clone()),
                key: "captionText".into(),
                source: text.clone(),
                translations: targets
                    .iter()
                    .map(|language| Translation {
                        language: language.clone(),
                        state: StringState::Missing,
                        text: None,
                        by: None,
                    })
                    .collect(),
            });
        }
    }
    rows
}

/// One language's counts over [`strings`].
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LanguageSummary {
    pub language: String,
    pub strings: usize,
    /// Each missing or stale string, as `<resource or layer> <key>`.
    pub missing: Vec<String>,
    pub stale: Vec<String>,
    /// Each script it has, and where its take stands.
    pub takes: Vec<TakeRow>,
}

/// Where one language's narration stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TakeState {
    /// Words, and no voice chosen: nothing is voiced by accident.
    NoVoice,
    /// A voice and no take yet: the source's take plays.
    NotGenerated,
    /// Generated from other words or another voice.
    OutOfDate,
    /// Silent, of the estimated length.
    Placeholder,
    /// A file of its own that no provider made.
    Recorded,
    Current,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TakeRow {
    pub resource: String,
    pub name: String,
    pub state: TakeState,
    /// What it plays and what the source plays, when they are known.
    pub seconds: Option<f64>,
    #[serde(rename = "sourceSeconds")]
    pub source_seconds: Option<f64>,
    /// The language the chosen voice speaks, when that is not this one.
    #[serde(rename = "voiceSpeaks")]
    pub voice_speaks: Option<String>,
    /// How many sentences it says, and how many the source does: what
    /// anchors on a sentence count on, so a translation keeps them.
    pub sentences: usize,
    #[serde(rename = "sourceSentences")]
    pub source_sentences: usize,
}

fn base_code(code: &str) -> String {
    code.split(['-', '_'])
        .next()
        .unwrap_or(code)
        .to_ascii_lowercase()
}

/// The language a voice's own name says it speaks: Google's `de-DE-…`, a
/// local package's `kokoro-en-…`. Other providers' names say nothing, and
/// the apps' rosters know more.
fn voice_speaks(voice: &str) -> Option<String> {
    crate::estimate::voice_language(voice)
        .or_else(|| {
            let mut parts = voice.split('-');
            let _package = parts.next()?;
            let code = parts.next()?;
            voice
                .contains('/')
                .then(|| code.to_string())
                .filter(|c| c.len() == 2 && c.chars().all(|c| c.is_ascii_lowercase()))
        })
        .map(|code| base_code(&code))
}

/// Each script `language` has, and where its take stands.
pub fn takes(meta: &ProjectMetadata, language: &str) -> Vec<TakeRow> {
    let mut rows = Vec::new();
    for resource in meta.resources.iter().flatten() {
        let Some(entry) = entries(resource).and_then(|all| all.get(language)) else {
            continue;
        };
        let Some(script) = entry_script(entry) else {
            continue;
        };
        let speech = &entry["speech"];
        let voice = speech["voiceID"].as_str().filter(|v| !v.is_empty());
        let provider = speech["provider"]
            .as_str()
            .or_else(|| resource.extra.get("speech")?.get("provider")?.as_str())
            .unwrap_or("openai");
        let own_file = entry["filename"]
            .as_str()
            .is_some_and(|name| !name.is_empty() && name != resource.filename);
        let state = if speech["placeholder"] == Value::Bool(true) {
            TakeState::Placeholder
        } else {
            match (voice, speech["renderedHash"].as_str()) {
                (_, None) if own_file => TakeState::Recorded,
                (None, _) => TakeState::NoVoice,
                (Some(_), None) => TakeState::NotGenerated,
                (Some(_), Some(_)) if !own_file => TakeState::NotGenerated,
                // A local voice's receipt folds in the project's pronunciations,
                // which this has no folder to read.
                (Some(voice), Some(receipt)) => {
                    if provider == "local"
                        || receipt == crate::speech::fingerprint(provider, voice, script)
                    {
                        TakeState::Current
                    } else {
                        TakeState::OutOfDate
                    }
                }
            }
        };
        let length = patched(resource, entry)
            .ok()
            .map(|shown| played_length(&shown));
        rows.push(TakeRow {
            resource: resource.id.clone(),
            name: resource.display_name.clone(),
            state,
            seconds: length.filter(|_| own_file).filter(|s| *s > 0.0),
            source_seconds: Some(played_length(resource)).filter(|s| *s > 0.0),
            voice_speaks: voice
                .and_then(voice_speaks)
                .filter(|speaks| *speaks != base_code(language)),
            sentences: crate::speech::tokenize(script).sentences.len(),
            source_sentences: crate::speech::take_script(resource)
                .map_or(0, |source| crate::speech::tokenize(&source).sentences.len()),
        });
    }
    rows
}

/// Each target's standing, for validate's Languages section.
pub fn summaries(meta: &ProjectMetadata) -> Vec<LanguageSummary> {
    let rows = strings(meta);
    let targets = languages(meta).map(|l| l.targets).unwrap_or_default();
    targets
        .into_iter()
        .map(|language| {
            let named = |state: StringState| -> Vec<String> {
                rows.iter()
                    .filter(|row| {
                        row.translations
                            .iter()
                            .any(|t| t.language == language && t.state == state)
                    })
                    .map(|row| {
                        let at = row.resource.as_ref().or(row.layer.as_ref());
                        format!("{} {}", at.map(String::as_str).unwrap_or("?"), row.key)
                    })
                    .collect()
            };
            LanguageSummary {
                strings: rows.len(),
                missing: named(StringState::Missing),
                stale: named(StringState::Stale),
                takes: takes(meta, &language),
                language,
            }
        })
        .collect()
}

/// `--language`, `language`: one code, a comma-separated list, or `all`
/// (source first, then each target; a project with no languages has only
/// its source, which is `None`). A code off the project's list is left for
/// [`flatten`] to refuse by name, so the others still render.
pub fn choose(meta: &ProjectMetadata, choice: &str) -> Vec<Option<String>> {
    if choice.trim() == "all" {
        return match languages(meta) {
            Some(list) => list
                .all()
                .into_iter()
                .map(|code| Some(code.to_string()))
                .collect(),
            None => vec![None],
        };
    }
    let mut chosen: Vec<Option<String>> = Vec::new();
    for code in choice.split(',').map(str::trim).filter(|c| !c.is_empty()) {
        let code = Some(code.to_string());
        if !chosen.contains(&code) {
            chosen.push(code);
        }
    }
    chosen
}

/// The whole answer about a project's strings: `promo strings --json`, the
/// tools' `promo_strings`, and the app's. `choice` keeps those target
/// languages' answers on each row.
pub fn strings_report(meta: &ProjectMetadata, choice: Option<&str>) -> Value {
    let mut rows = strings(meta);
    if let Some(choice) = choice {
        let chosen = choose(meta, choice);
        for row in &mut rows {
            row.translations
                .retain(|t| chosen.contains(&Some(t.language.clone())));
        }
    }
    let list = languages(meta);
    serde_json::json!({
        "source": list.as_ref().map(|l| l.source.clone()),
        "targets": list.as_ref().map(|l| l.targets.clone()).unwrap_or_default(),
        "summaries": summaries(meta),
        "strings": rows,
    })
}

/// The folder a language's render goes in, under where the source's goes:
/// none for the source, so nothing moves for a project with no languages.
pub fn folder(meta: &ProjectMetadata, language: Option<&str>) -> Option<String> {
    let language = language?;
    let is_source = languages(meta).is_some_and(|l| l.source == language);
    (!is_source).then(|| language.to_string())
}

/// `setLanguages`: the project's list. A language leaving the list takes
/// its content off every resource: nothing stays hidden in the file.
pub fn set_languages(
    meta: &mut ProjectMetadata,
    source: &str,
    targets: &[String],
) -> Result<(), String> {
    fn a_code(code: &str) -> bool {
        !code.is_empty()
            && code.split('-').all(|part| {
                !part.is_empty()
                    && part.len() <= 8
                    && part.chars().all(|c| c.is_ascii_alphanumeric())
            })
    }
    for code in std::iter::once(source).chain(targets.iter().map(String::as_str)) {
        if !a_code(code) {
            return Err(format!(
                "\"{code}\" is not a language code, like en or pt-BR"
            ));
        }
    }
    let unique: std::collections::BTreeSet<&str> = targets.iter().map(String::as_str).collect();
    if unique.len() != targets.len() {
        return Err("a language is listed twice".to_string());
    }
    if unique.contains(source) {
        return Err(format!(
            "\"{source}\" is the source, so it cannot also be a target"
        ));
    }
    for resource in meta.resources.iter_mut().flatten() {
        let now_empty = match resource
            .extra
            .get_mut(LOCALIZED_KEY)
            .and_then(Value::as_object_mut)
        {
            Some(all) => {
                all.retain(|language, _| unique.contains(language.as_str()));
                all.is_empty()
            }
            None => false,
        };
        if now_empty {
            resource.extra.remove(LOCALIZED_KEY);
        }
    }
    meta.extra.insert(
        LANGUAGES_KEY.to_string(),
        serde_json::json!({ "source": source, "targets": targets }),
    );
    Ok(())
}

/// What `setTranslation` writes on.
#[derive(Debug, Clone, Copy)]
pub enum TranslationTarget<'a> {
    Resource(&'a str),
    /// The caption this layer shows. One keeping its text on the layer has
    /// its text and style moved to a new caption resource first.
    Layer(&'a str),
}

fn layer_mut<'a>(
    layers: &'a mut [crate::ProjectLayer],
    id: &str,
) -> Option<&'a mut crate::ProjectLayer> {
    for layer in layers.iter_mut() {
        if layer.id == id {
            return Some(layer);
        }
        if let Some(found) = layer.members.as_mut().and_then(|m| layer_mut(m, id)) {
            return Some(found);
        }
    }
    None
}

fn find_layer_mut<'a>(
    meta: &'a mut ProjectMetadata,
    id: &str,
) -> Option<&'a mut crate::ProjectLayer> {
    let on_top = meta
        .layers
        .as_mut()
        .and_then(|layers| layer_mut(layers, id))
        .is_some();
    if on_top {
        return layer_mut(meta.layers.as_mut()?, id);
    }
    for resource in meta.resources.iter_mut().flatten() {
        if let Some(composition) = resource.composition.as_mut() {
            if let Some(found) = layer_mut(&mut composition.layers, id) {
                return Some(found);
            }
        }
    }
    None
}

/// The caption resource `layer_id` shows, made from the layer's own text
/// and style when it keeps them itself: a language's entry needs a
/// resource to live on.
fn caption_resource_of(meta: &mut ProjectMetadata, layer_id: &str) -> Result<String, String> {
    let taken: std::collections::BTreeSet<String> = meta
        .resources
        .iter()
        .flatten()
        .map(|r| r.id.clone())
        .collect();
    let layer =
        find_layer_mut(meta, layer_id).ok_or_else(|| format!("no layer with id {layer_id}"))?;
    if let Some(id) = layer.resource_id.clone().filter(|id| taken.contains(id)) {
        return Ok(id);
    }
    let text = layer
        .caption_text
        .take()
        .ok_or_else(|| format!("layer {layer_id} shows no caption"))?;
    let style = layer.caption_style.take();
    let name = layer.name.clone();
    let mut id = format!("{layer_id}-caption");
    let mut n = 2;
    while taken.contains(&id) {
        id = format!("{layer_id}-caption-{n}");
        n += 1;
    }
    layer.resource_id = Some(id.clone());
    let mut resource = ProjectResource::placeholder(crate::ProjectResourceKind::Caption);
    resource.id = id.clone();
    resource.display_name = name;
    resource.caption_text = Some(text);
    resource.caption_style = style;
    meta.resources.get_or_insert_with(Vec::new).push(resource);
    Ok(id)
}

/// A resource's style wins whole over its layer's, so a language patching
/// one key of a style the resource does not hold would lose the rest. The
/// layer's style moves up first, when its layers agree on one.
fn seed_style(meta: &mut ProjectMetadata, resource_id: &str) -> Result<(), String> {
    let holds = meta
        .resources
        .iter()
        .flatten()
        .any(|r| r.id == resource_id && r.caption_style.is_some());
    if holds {
        return Ok(());
    }
    let mut styles: Vec<crate::SubtitleStyle> = Vec::new();
    for layer in crate::nesting::all_layers(meta) {
        if layer.resource_id.as_deref() == Some(resource_id) {
            if let Some(style) = &layer.caption_style {
                if !styles.contains(style) {
                    styles.push(style.clone());
                }
            }
        }
    }
    if styles.len() > 1 {
        return Err(format!(
            "the layers showing resource {resource_id} have different styles, so one language's \
             style has nothing to build on: give the resource a captionStyle first"
        ));
    }
    if let Some(style) = styles.pop() {
        if let Some(resource) = meta
            .resources
            .iter_mut()
            .flatten()
            .find(|r| r.id == resource_id)
        {
            resource.caption_style = Some(style);
        }
    }
    Ok(())
}

/// `setTranslation`: a merge patch over one language's entry on a resource
/// (`null` removes a key). A patch naming text stamps `by` and the source
/// it was written from. Automation never overwrites a person's words.
pub fn set_translation(
    meta: &mut ProjectMetadata,
    target: TranslationTarget<'_>,
    language: &str,
    patch: &Value,
    by: &str,
) -> Result<(), String> {
    if by != "person" && by != "machine" {
        return Err(format!("by: \"{by}\" is not \"person\" or \"machine\""));
    }
    let Some(list) = languages(meta) else {
        return Err("this project lists no languages: setLanguages first".to_string());
    };
    if language == list.source {
        return Err(format!(
            "\"{language}\" is the source: change the resource itself"
        ));
    }
    if !list.targets.iter().any(|t| t == language) {
        return Err(format!(
            "\"{language}\" is not one of this project's languages ({})",
            list.all().join(", ")
        ));
    }
    let Some(fields) = patch.as_object() else {
        return Err("patch must be a JSON object of the fields to change".to_string());
    };
    if let Some(key) = ENTRY_KEYS.iter().find(|key| fields.contains_key(**key)) {
        return Err(format!("\"{key}\" is written by this command"));
    }
    let resource_id = match target {
        TranslationTarget::Resource(id) => id.to_string(),
        TranslationTarget::Layer(id) => caption_resource_of(meta, id)?,
    };
    if fields.contains_key("captionStyle") {
        seed_style(meta, &resource_id)?;
    }
    let resource = meta
        .resources
        .iter_mut()
        .flatten()
        .find(|r| r.id == resource_id)
        .ok_or_else(|| format!("no resource with id {resource_id}"))?;
    let mut entry = entries(resource)
        .and_then(|all| all.get(language))
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    // A voice is not a word: a machine may choose one for a person's script.
    let names_text = ["captionText", "recipe", "svg"]
        .iter()
        .any(|key| fields.contains_key(*key))
        || fields
            .get("speech")
            .is_some_and(|s| s.get("text").is_some());
    if names_text && by == "machine" && entry["by"].as_str() == Some("person") {
        return Err(format!(
            "resource {resource_id}: a person wrote its {language} text, and a machine does not \
             overwrite that"
        ));
    }
    merge_patch(&mut entry, patch);
    prune_empty(&mut entry);
    // Words, not their neighbours: a title's bold says nothing about whether
    // its text is still right.
    if !strings_of(patch).is_empty() {
        let map = entry.as_object_mut().expect("an entry is an object");
        map.insert("sourceHash".into(), Value::from(source_hash(resource)));
        map.insert("by".into(), Value::from(by));
    }
    patched(resource, &entry).map_err(|why| format!("resource {resource_id}: {why}"))?;
    let says_nothing = entry
        .as_object()
        .is_some_and(|map| map.keys().all(|key| ENTRY_KEYS.contains(&key.as_str())));
    let all = resource
        .extra
        .entry(LOCALIZED_KEY.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let map = all
        .as_object_mut()
        .ok_or_else(|| format!("resource {resource_id}: localized is not an object"))?;
    if says_nothing {
        map.remove(language);
    } else {
        map.insert(language.to_string(), entry);
    }
    if map.is_empty() {
        resource.extra.remove(LOCALIZED_KEY);
    }
    Ok(())
}

/// What a patch that took the last key out of an object leaves behind.
fn prune_empty(value: &mut Value) {
    if let Some(map) = value.as_object_mut() {
        map.values_mut().for_each(prune_empty);
        map.retain(|_, v| !v.as_object().is_some_and(Map::is_empty));
    }
}

/// The patch that says `text` for one of [`strings`]' keys, or takes it back
/// to the source. `entry` is the language's as it stands: an element of a
/// vector is one item of a list, and a patch replaces a list whole.
fn string_patch(entry: Option<&Value>, key: &str, text: Option<&str>) -> Result<Value, String> {
    let said = text.map_or(Value::Null, Value::from);
    Ok(match key {
        "captionText" => serde_json::json!({ "captionText": said }),
        "recipe" => serde_json::json!({ "recipe": { "text": { "text": said } } }),
        // A script taken back takes its take too: the source's plays again.
        // The voice stays chosen, for whoever writes the words next.
        "speech" if text.is_none() => serde_json::json!({
            "speech": { "text": null, "renderedHash": null, "placeholder": null, "wordFixes": null },
            "filename": null, "duration": null,
            "trimStart": null, "trimEnd": null, "trimKeyframes": null }),
        "speech" => serde_json::json!({ "speech": { "text": said } }),
        other => {
            let Some(node) = other.strip_prefix("svg:") else {
                return Err(format!(
                    "key \"{key}\" is none of captionText, recipe, speech or svg:<node>"
                ));
            };
            let mut own = entry
                .and_then(|e| e["svg"]["overrides"].as_array())
                .cloned()
                .unwrap_or_default();
            match own.iter_mut().find(|o| o["node"] == node) {
                Some(found) => merge_patch(found, &serde_json::json!({ "text": said })),
                None if text.is_some() => {
                    own.push(serde_json::json!({ "node": node, "text": said }))
                }
                None => {}
            }
            own.retain(|o| o.as_object().is_some_and(|m| m.keys().any(|k| k != "node")));
            let list = if own.is_empty() {
                Value::Null
            } else {
                Value::Array(own)
            };
            serde_json::json!({ "svg": { "overrides": list } })
        }
    })
}

/// `setTranslation` by a row of [`strings`]: its `key` and the language's
/// `text`. No text, or only spaces, takes the string back to the source: a
/// language is never blank because nobody has written it.
pub fn set_string(
    meta: &mut ProjectMetadata,
    target: TranslationTarget<'_>,
    language: &str,
    key: &str,
    text: Option<&str>,
    by: &str,
) -> Result<(), String> {
    let resource_id = match target {
        TranslationTarget::Resource(id) => id.to_string(),
        TranslationTarget::Layer(id) => caption_resource_of(meta, id)?,
    };
    let entry = meta
        .resources
        .iter()
        .flatten()
        .find(|r| r.id == resource_id)
        .and_then(entries)
        .and_then(|all| all.get(language));
    let text = text.filter(|t| !t.trim().is_empty());
    let patch = string_patch(entry, key, text)?;
    set_translation(
        meta,
        TranslationTarget::Resource(&resource_id),
        language,
        &patch,
        by,
    )
}

/// Whether the file carries languages: ignorable, so stamped the lossless
/// reader's rung.
pub fn carries_languages(meta: &ProjectMetadata) -> bool {
    meta.extra.contains_key(LANGUAGES_KEY)
        || meta
            .resources
            .iter()
            .flatten()
            .any(|r| r.extra.contains_key(LOCALIZED_KEY))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn meta(resources: Value) -> ProjectMetadata {
        let doc = json!({
            "id": "P", "name": "P", "createdAt": 0, "subtitles": [], "trimStart": 0, "trimEnd": 0,
            "videoDuration": 0, "compositionSettings": {},
            "state": "recorded", "languages": {"source": "en", "targets": ["de", "ja"]},
            "resources": resources,
        });
        ProjectMetadata::from_json(&doc.to_string()).unwrap()
    }

    fn caption() -> Value {
        json!({"id": "c", "kind": "caption", "filename": "", "displayName": "c", "addedAt": 0,
               "captionText": "Now on iPad",
               "captionStyle": {"fontSize": 60, "alignment": "center"},
               "localized": {"de": {"captionText": "Jetzt auf dem iPad",
                                    "captionStyle": {"fontSize": 40},
                                    "sourceHash": "a1b2c3d4", "by": "person"}}})
    }

    fn resource(meta: &ProjectMetadata) -> &ProjectResource {
        &meta.resources.as_ref().unwrap()[0]
    }

    #[test]
    fn a_language_patches_only_what_it_names() {
        let mut m = meta(json!([caption()]));
        flatten(&mut m, "de").unwrap();
        let r = resource(&m);
        assert_eq!(r.caption_text.as_deref(), Some("Jetzt auf dem iPad"));
        let style = r.caption_style.as_ref().unwrap();
        assert_eq!(style.font_size, Some(40.0));
        let doc = serde_json::to_value(r).unwrap();
        assert_eq!(
            doc["captionStyle"]["alignment"],
            json!("center"),
            "merged, not replaced"
        );
        assert!(doc.get("localized").is_none());
        assert!(doc.get("sourceHash").is_none() && doc.get("by").is_none());
    }

    #[test]
    fn a_language_with_no_entry_and_the_source_show_the_source() {
        for language in ["ja", "en"] {
            let mut m = meta(json!([caption()]));
            flatten(&mut m, language).unwrap();
            assert_eq!(resource(&m).caption_text.as_deref(), Some("Now on iPad"));
            assert!(!resource(&m).extra.contains_key(LOCALIZED_KEY));
        }
    }

    #[test]
    fn a_language_off_the_list_is_refused() {
        let mut m = meta(json!([caption()]));
        let why = flatten(&mut m, "fr").unwrap_err();
        assert!(why.contains("en, de, ja"), "{why}");
    }

    #[test]
    fn identity_cannot_differ_and_validate_says_so() {
        for key in IDENTITY_KEYS {
            let mut bad = caption();
            bad["localized"]["de"][*key] = json!("x");
            let mut m = meta(json!([bad]));
            let (breaks, _) = problems(&m);
            assert!(breaks.iter().any(|b| b.contains(key)), "{key}: {breaks:?}");
            assert!(flatten(&mut m, "de").is_err(), "{key}");
        }
    }

    #[test]
    fn an_entry_of_the_wrong_shape_breaks_rather_than_half_applies() {
        let mut bad = caption();
        bad["localized"]["de"]["captionStyle"] = json!("big");
        let mut m = meta(json!([bad]));
        assert_eq!(problems(&m).0.len(), 1);
        assert!(flatten(&mut m, "de").is_err());
        assert_eq!(resource(&m).caption_text.as_deref(), Some("Now on iPad"));
    }

    #[test]
    fn another_file_brings_its_own_facts_not_the_sources() {
        let clip = json!({"id": "v", "kind": "video", "filename": "en.mp4", "displayName": "v",
            "addedAt": 0, "duration": 9.0, "trimStart": 1.0, "trimEnd": 2.0,
            "videoNaturalWidth": 1920.0, "videoNaturalHeight": 1080.0,
            "localized": {"de": {"filename": "de.mp4", "duration": 7.0,
                                 "videoNaturalWidth": 1280.0},
                          "ja": {"volume": 0.5}}});
        let mut de = meta(json!([clip]));
        flatten(&mut de, "de").unwrap();
        let r = resource(&de);
        assert_eq!(r.filename, "de.mp4");
        assert_eq!(r.duration, Some(7.0));
        assert_eq!(r.video_natural_width, Some(1280.0));
        assert_eq!(
            (r.trim_start, r.trim_end),
            (None, None),
            "seconds of the other file"
        );
        assert_eq!(r.video_natural_height, None);

        let mut ja = meta(json!([clip]));
        flatten(&mut ja, "ja").unwrap();
        let r = resource(&ja);
        assert_eq!(r.filename, "en.mp4");
        assert_eq!(r.trim_start, Some(1.0), "the same file keeps its trims");
    }

    #[test]
    fn content_off_the_list_and_text_on_a_layer_are_named() {
        let mut orphan = caption();
        orphan["localized"]["fr"] = json!({"captionText": "Maintenant"});
        let mut doc = serde_json::to_value(meta(json!([orphan]))).unwrap();
        doc["layers"] = json!([{"id": "inline", "name": "i", "sortIndex": 0, "kind": "caption",
            "isEnabled": true, "startTime": 0, "duration": 2, "captionText": "Hi", "keyframes": []},
            {"id": "placed", "name": "p", "sortIndex": 1, "kind": "caption", "isEnabled": true,
             "startTime": 0, "duration": 2, "resourceID": "c", "keyframes": []}]);
        let m = ProjectMetadata::from_json(&doc.to_string()).unwrap();
        let (breaks, warnings) = problems(&m);
        assert!(breaks.is_empty(), "{breaks:?}");
        assert!(
            warnings.iter().any(|w| w.contains("\"fr\"")),
            "{warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("\"inline\"")),
            "{warnings:?}"
        );
        assert!(
            !warnings.iter().any(|w| w.contains("\"placed\"")),
            "{warnings:?}"
        );
    }

    #[test]
    fn strings_say_who_is_missing_and_who_is_stale() {
        let mut m = meta(json!([caption()]));
        let row = |m: &ProjectMetadata| strings(m).remove(0);
        // Hand-written, with a hash that matches nothing.
        let first = row(&m);
        assert_eq!(first.resource.as_deref(), Some("c"));
        assert_eq!(first.source, "Now on iPad");
        assert_eq!(first.translations[0].state, StringState::Stale);
        assert_eq!(first.translations[1].state, StringState::Missing);

        set_translation(
            &mut m,
            TranslationTarget::Resource("c"),
            "de",
            &json!({"captionText": "Jetzt"}),
            "machine",
        )
        .unwrap_err(); // a person's words stand
        set_translation(
            &mut m,
            TranslationTarget::Resource("c"),
            "de",
            &json!({"captionText": "Jetzt"}),
            "person",
        )
        .unwrap();
        assert_eq!(row(&m).translations[0].state, StringState::Current);
        assert_eq!(row(&m).translations[0].by.as_deref(), Some("person"));

        // The source moves on: stale, and it still renders the old words.
        m.resources.as_mut().unwrap()[0].caption_text = Some("Now on Mac".into());
        assert_eq!(row(&m).translations[0].state, StringState::Stale);
        let summary = summaries(&m);
        assert_eq!(summary[0].stale, vec!["c captionText".to_string()]);
        assert_eq!(summary[1].missing, vec!["c captionText".to_string()]);
    }

    #[test]
    fn a_language_size_scales_the_sizes_its_keyframes_say() {
        let mut m = with_layers(
            json!([caption()]),
            json!([{"id": "l", "name": "l", "kind": "caption", "isEnabled": true, "resourceID": "c", "startTime": 0,
                    "sortIndex": 0, "keyframes": [{"id": "k0", "time": 0, "zoom": 60},
                                                  {"id": "k1", "time": 1, "fontSize": 90},
                                                  {"id": "k2", "time": 2, "verticalShift": 10}]}]),
        );
        flatten(&mut m, "de").unwrap();
        let keys = &m.layers.as_ref().unwrap()[0].keyframes;
        assert_eq!(keys[0].zoom, Some(40.0));
        assert_eq!(keys[1].font_size, Some(60.0));
        assert_eq!(keys[2].vertical_shift, Some(10.0), "position is shared");
    }

    #[test]
    fn the_source_leaves_keyframe_sizes_alone() {
        let layers = json!([{"id": "l", "name": "l", "kind": "caption", "isEnabled": true, "resourceID": "c",
                             "startTime": 0, "sortIndex": 0,
                             "keyframes": [{"id": "k0", "time": 0, "zoom": 60}]}]);
        let mut m = with_layers(json!([caption()]), layers);
        flatten(&mut m, "en").unwrap();
        assert_eq!(m.layers.as_ref().unwrap()[0].keyframes[0].zoom, Some(60.0));
    }

    #[test]
    fn a_string_is_written_and_taken_back_by_its_key() {
        let mut m = meta(
            json!([{"id": "c", "kind": "caption", "filename": "", "displayName": "c",
                                 "addedAt": 0, "captionText": "Now on iPad"}]),
        );
        set_string(
            &mut m,
            TranslationTarget::Resource("c"),
            "de",
            "captionText",
            Some("Jetzt"),
            "person",
        )
        .unwrap();
        let row = &strings(&m)[0].translations[0];
        assert_eq!(
            (row.text.as_deref(), row.state),
            (Some("Jetzt"), StringState::Current)
        );
        // Spaces are nothing written: the source shows, and nothing is left.
        set_string(
            &mut m,
            TranslationTarget::Resource("c"),
            "de",
            "captionText",
            Some("  "),
            "person",
        )
        .unwrap();
        assert_eq!(strings(&m)[0].translations[0].state, StringState::Missing);
        assert!(!resource(&m).extra.contains_key(LOCALIZED_KEY));
        let unknown = set_string(
            &mut m,
            TranslationTarget::Resource("c"),
            "de",
            "nope",
            Some("x"),
            "person",
        );
        assert!(unknown.unwrap_err().contains("nope"));
    }

    #[test]
    fn a_title_takes_its_words_and_keeps_its_recipe() {
        let mut m = meta(
            json!([{"id": "t", "kind": "model", "filename": "", "displayName": "t",
                                 "addedAt": 0,
                                 "recipe": {"text": {"text": "Hello", "bold": true}}}]),
        );
        set_string(
            &mut m,
            TranslationTarget::Resource("t"),
            "de",
            "recipe",
            Some("Hallo"),
            "person",
        )
        .unwrap();
        let mut flat = m.clone();
        flatten(&mut flat, "de").unwrap();
        let doc = serde_json::to_value(resource(&flat)).unwrap();
        assert_eq!(doc["recipe"]["text"]["text"], json!("Hallo"));
        assert_eq!(doc["recipe"]["text"]["bold"], json!(true));
        set_string(
            &mut m,
            TranslationTarget::Resource("t"),
            "de",
            "recipe",
            None,
            "person",
        )
        .unwrap();
        assert!(
            !resource(&m).extra.contains_key(LOCALIZED_KEY),
            "no empty husk left"
        );
    }

    #[test]
    fn a_titles_bold_does_not_make_old_words_look_fresh() {
        let mut m = meta(
            json!([{"id": "t", "kind": "model", "filename": "", "displayName": "t",
                                 "addedAt": 0, "recipe": {"text": {"text": "Hello"}},
                                 "localized": {"de": {"recipe": {"text": {"text": "Hallo"}},
                                                      "sourceHash": "old", "by": "machine"}}}]),
        );
        set_translation(
            &mut m,
            TranslationTarget::Resource("t"),
            "de",
            &json!({"recipe": {"text": {"bold": true}}}),
            "person",
        )
        .unwrap();
        let row = &strings(&m)[0].translations[0];
        assert_eq!(row.state, StringState::Stale);
        assert_eq!(row.by.as_deref(), Some("machine"));
    }

    #[test]
    fn a_vector_answers_element_by_element() {
        let mut m = meta(json!([{"id": "v", "kind": "image", "filename": "a.svg",
                                 "displayName": "v", "addedAt": 0,
                                 "svg": {"overrides": [
                                     {"node": "one", "text": "First", "fill": "#ff0000"},
                                     {"node": "two", "text": "Second"}]}}]));
        set_string(
            &mut m,
            TranslationTarget::Resource("v"),
            "de",
            "svg:one",
            Some("Erste"),
            "person",
        )
        .unwrap();
        let rows = strings(&m);
        assert_eq!(rows[0].translations[0].state, StringState::Current);
        assert_eq!(
            rows[1].translations[0].state,
            StringState::Missing,
            "only what it wrote"
        );
        let mut flat = m.clone();
        flatten(&mut flat, "de").unwrap();
        let doc = serde_json::to_value(resource(&flat)).unwrap();
        let list = doc["svg"]["overrides"].as_array().unwrap();
        assert_eq!(list[0]["text"], json!("Erste"));
        assert_eq!(
            list[0]["fill"],
            json!("#ff0000"),
            "the source's other fields stay"
        );
        assert_eq!(
            list[1]["text"],
            json!("Second"),
            "an element it did not name stays"
        );
    }

    #[test]
    fn a_generated_caption_has_no_string_of_its_own() {
        let m = meta(
            json!([{"id": "g", "kind": "caption", "filename": "", "displayName": "g",
                             "addedAt": 0, "captionText": "Hello", "generatedBy": "n"}]),
        );
        assert!(strings(&m).is_empty());
    }

    #[test]
    fn a_size_alone_leaves_the_text_stale_rather_than_fresh() {
        let mut m = meta(json!([caption()]));
        set_translation(
            &mut m,
            TranslationTarget::Resource("c"),
            "de",
            &json!({"captionStyle": {"fontSize": 30}}),
            "machine",
        )
        .unwrap();
        assert_eq!(strings(&m)[0].translations[0].state, StringState::Stale);
        assert_eq!(strings(&m)[0].translations[0].by.as_deref(), Some("person"));
    }

    fn with_layers(resources: Value, layers: Value) -> ProjectMetadata {
        let mut doc = serde_json::to_value(meta(resources)).unwrap();
        doc["layers"] = layers;
        ProjectMetadata::from_json(&doc.to_string()).unwrap()
    }

    #[test]
    fn a_caption_on_a_layer_moves_to_a_resource_when_translated() {
        let mut m = with_layers(
            json!([]),
            json!([{"id": "l", "name": "Headline", "sortIndex": 0, "kind": "caption",
                    "isEnabled": true, "startTime": 0, "duration": 2, "captionText": "Hi",
                    "captionStyle": {"fontSize": 50, "alignment": "leading"}, "keyframes": []}]),
        );
        assert_eq!(strings(&m)[0].layer.as_deref(), Some("l"));
        set_translation(
            &mut m,
            TranslationTarget::Layer("l"),
            "de",
            &json!({"captionText": "Hallo", "captionStyle": {"fontSize": 40}}),
            "machine",
        )
        .unwrap();
        let layer = &m.layers.as_ref().unwrap()[0];
        assert_eq!(layer.resource_id.as_deref(), Some("l-caption"));
        assert_eq!(layer.caption_text, None);
        assert!(problems(&m).1.is_empty(), "{:?}", problems(&m).1);
        assert_eq!(strings(&m)[0].resource.as_deref(), Some("l-caption"));

        let mut german = m.clone();
        flatten(&mut german, "de").unwrap();
        let doc = serde_json::to_value(resource(&german)).unwrap();
        assert_eq!(doc["captionText"], json!("Hallo"));
        assert_eq!(doc["captionStyle"]["fontSize"], json!(40.0));
        assert_eq!(
            doc["captionStyle"]["alignment"],
            json!("leading"),
            "the layer's style came along"
        );
        let source = serde_json::to_value(resource(&m)).unwrap();
        assert_eq!(source["captionText"], json!("Hi"));
        assert_eq!(source["captionStyle"]["fontSize"], json!(50.0));
    }

    #[test]
    fn a_layers_style_seeds_a_resource_that_has_none() {
        let mut bare = caption();
        bare.as_object_mut().unwrap().remove("captionStyle");
        bare.as_object_mut().unwrap().remove("localized");
        let layer = |id: &str, size: f64| {
            json!({"id": id, "name": id, "sortIndex": 0, "kind": "caption", "isEnabled": true,
                   "startTime": 0, "duration": 2, "resourceID": "c",
                   "captionStyle": {"fontSize": size, "alignment": "leading"}, "keyframes": []})
        };
        let patch = json!({"captionStyle": {"fontSize": 20}});
        let mut one = with_layers(json!([bare]), json!([layer("a", 50.0)]));
        set_translation(
            &mut one,
            TranslationTarget::Resource("c"),
            "de",
            &patch,
            "person",
        )
        .unwrap();
        flatten(&mut one, "de").unwrap();
        let doc = serde_json::to_value(resource(&one)).unwrap();
        assert_eq!(doc["captionStyle"]["alignment"], json!("leading"));

        let mut two = with_layers(json!([bare]), json!([layer("a", 50.0), layer("b", 60.0)]));
        let why = set_translation(
            &mut two,
            TranslationTarget::Resource("c"),
            "de",
            &patch,
            "person",
        )
        .unwrap_err();
        assert!(why.contains("different styles"), "{why}");
    }

    #[test]
    fn set_translation_refuses_what_is_not_a_translation() {
        let mut m = meta(json!([caption()]));
        let mut try_it = |language: &str, patch: Value| {
            set_translation(
                &mut m,
                TranslationTarget::Resource("c"),
                language,
                &patch,
                "person",
            )
            .unwrap_err()
        };
        assert!(try_it("en", json!({"captionText": "x"})).contains("is the source"));
        assert!(try_it("fr", json!({"captionText": "x"})).contains("en, de, ja"));
        assert!(try_it("de", json!({"id": "x"})).contains("cannot differ"));
        assert!(try_it("de", json!({"sourceHash": "x"})).contains("written by this command"));
        assert!(try_it("de", json!("x")).contains("JSON object"));
    }

    #[test]
    fn taking_every_key_back_removes_the_entry() {
        let mut m = meta(json!([caption()]));
        set_translation(
            &mut m,
            TranslationTarget::Resource("c"),
            "de",
            &json!({"captionText": null, "captionStyle": null}),
            "person",
        )
        .unwrap();
        assert!(!resource(&m).extra.contains_key(LOCALIZED_KEY));
    }

    #[test]
    fn a_language_leaving_the_list_takes_its_content_along() {
        let mut m = meta(json!([caption()]));
        set_languages(&mut m, "en", &["ja".to_string()]).unwrap();
        assert!(!resource(&m).extra.contains_key(LOCALIZED_KEY));
        assert_eq!(languages(&m).unwrap().targets, vec!["ja".to_string()]);
        assert!(set_languages(&mut m, "en", &["en".to_string()]).is_err());
        assert!(set_languages(&mut m, "en", &["de".to_string(), "de".to_string()]).is_err());
        assert!(set_languages(&mut m, "not a code", &[]).is_err());
        set_languages(&mut m, "en", &["pt-BR".to_string()]).unwrap();
    }

    #[test]
    fn choosing_languages() {
        let m = meta(json!([caption()]));
        let some = |s: &str| Some(s.to_string());
        assert_eq!(choose(&m, "all"), vec![some("en"), some("de"), some("ja")]);
        assert_eq!(choose(&m, "de, ja,de"), vec![some("de"), some("ja")]);
        assert_eq!(folder(&m, Some("en")), None);
        assert_eq!(folder(&m, Some("de")), some("de"));
        assert_eq!(folder(&m, None), None);
        let mut plain = serde_json::to_value(&m).unwrap();
        plain.as_object_mut().unwrap().remove("languages");
        let plain = ProjectMetadata::from_json(&plain.to_string()).unwrap();
        assert_eq!(choose(&plain, "all"), vec![None]);
    }

    #[test]
    fn a_project_with_languages_is_stamped_the_lossless_reader() {
        let m = meta(json!([caption()]));
        assert!(carries_languages(&m));
        assert_eq!(
            m.minimum_reader_version().max(crate::LOSSLESS_READER),
            crate::LOSSLESS_READER
        );
        assert_eq!(m.minimum_reader_version(), crate::LOSSLESS_READER);
    }

    fn narrated() -> ProjectMetadata {
        ProjectMetadata::from_json(
            &serde_json::json!({
                "id": "p", "name": "p", "createdAt": 0, "state": "recorded",
                "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
                "compositionSettings": { "canvasWidth": 320, "canvasHeight": 180,
                                         "backgroundColorHex": "000000" },
                "languages": { "source": "en", "targets": ["de"] },
                "resources": [
                    { "id": "r1", "kind": "audio", "filename": "r1.mp3", "displayName": "Intro",
                      "addedAt": 0, "imageCuts": [], "disabledAudioTrackIndices": [],
                      "duration": 8.0, "trimEnd": 6.0,
                      "speech": { "text": "Hello there.", "provider": "openai", "voiceID": "alloy",
                                  "renderedHash": "english", "placeholder": true,
                                  "wordFixes": [{ "word": 1, "start": 0.1 }],
                                  "sourceResourceID": "page" },
                      "localized": { "de": {
                          "filename": "r1.de.mp3", "duration": 9.4,
                          "speech": { "text": "Hallo zusammen.", "provider": "elevenlabs",
                                      "voiceID": "anna", "renderedHash": "german" },
                          "sourceHash": "x", "by": "machine" } } } ],
                "layers": [
                    { "id": "bg", "kind": "background", "isEnabled": true, "name": "bg",
                      "startTime": 0, "duration": 6.0, "sortIndex": 0, "keyframes": [] },
                    { "id": "whole", "kind": "audio", "isEnabled": true, "name": "whole",
                      "resourceID": "r1", "startTime": 0, "duration": 6.0, "sortIndex": 1,
                      "keyframes": [] },
                    { "id": "short", "kind": "audio", "isEnabled": true, "name": "short",
                      "resourceID": "r1", "startTime": 0, "duration": 2.0, "sortIndex": 2,
                      "keyframes": [] } ]
            })
            .to_string(),
        )
        .unwrap()
    }

    /// A language's take is a new recording: the source's trims and receipt
    /// say nothing about it, a layer that played the whole take plays the
    /// whole of this one, the background covers it, and one shortened by hand
    /// keeps its length.
    #[test]
    fn a_layer_follows_a_languages_take() {
        let mut meta = narrated();
        flatten(&mut meta, "de").unwrap();
        let take = &meta.resources.as_ref().unwrap()[0];
        assert_eq!(take.filename, "r1.de.mp3");
        assert_eq!(take.trim_end, None);
        let speech = &take.extra["speech"];
        assert_eq!(speech["text"], "Hallo zusammen.");
        assert_eq!(speech["renderedHash"], "german");
        assert_eq!(speech["language"], "de");
        assert!(speech.get("placeholder").is_none(), "the source's");
        assert!(speech.get("wordFixes").is_none(), "the source's words");
        assert_eq!(speech["sourceResourceID"], "page", "not about the bytes");
        let layer = |id: &str| {
            meta.layers
                .as_ref()
                .unwrap()
                .iter()
                .find(|l| l.id == id)
                .unwrap()
                .duration
        };
        assert_eq!(layer("whole"), Some(9.4));
        assert_eq!(layer("short"), Some(2.0));
        assert_eq!(layer("bg"), Some(9.4));

        // The source is untouched by any of it.
        let mut source = narrated();
        flatten(&mut source, "en").unwrap();
        let whole = source
            .layers
            .as_ref()
            .unwrap()
            .iter()
            .find(|l| l.id == "whole");
        assert_eq!(whole.unwrap().duration, Some(6.0));
        assert_eq!(source.resources.as_ref().unwrap()[0].filename, "r1.mp3");
    }

    /// Words with no take yet: the source's take plays, and its receipt is
    /// not taken for this script's. A voice with no words changes nothing.
    #[test]
    fn a_script_without_a_take_plays_the_source() {
        let mut meta = narrated();
        let entry = |value: serde_json::Value| serde_json::json!({ "de": value });
        meta.resources.as_mut().unwrap()[0].extra.insert(
            LOCALIZED_KEY.into(),
            entry(serde_json::json!({
                "speech": { "text": "Hallo zusammen." } })),
        );
        let mut flat = meta.clone();
        flatten(&mut flat, "de").unwrap();
        let take = &flat.resources.as_ref().unwrap()[0];
        assert_eq!(take.filename, "r1.mp3");
        assert_eq!(take.trim_end, Some(6.0));
        // What plays is English, so English is what anchors and captions read.
        assert_eq!(
            take.extra["speech"]["text"],
            meta.resources.as_ref().unwrap()[0].extra["speech"]["text"]
        );
        assert_eq!(take.extra["speech"]["voiceID"], "alloy");
        assert_eq!(take.extra["speech"]["renderedHash"], "english");

        meta.resources.as_mut().unwrap()[0].extra.insert(
            LOCALIZED_KEY.into(),
            entry(serde_json::json!({
                "speech": { "voiceID": "anna" } })),
        );
        let mut flat = meta.clone();
        flatten(&mut flat, "de").unwrap();
        let speech = &flat.resources.as_ref().unwrap()[0].extra["speech"];
        assert_eq!(speech["voiceID"], "alloy");
        assert_eq!(speech["renderedHash"], "english");
    }

    /// Taking a script back takes its take with it, and leaves the voice.
    #[test]
    fn clearing_a_script_brings_the_source_take_back() {
        let mut meta = narrated();
        set_string(
            &mut meta,
            TranslationTarget::Resource("r1"),
            "de",
            "speech",
            Some("  "),
            "person",
        )
        .unwrap();
        let entry = &entries(&meta.resources.as_ref().unwrap()[0]).unwrap()["de"];
        assert!(entry.get("filename").is_none() && entry.get("duration").is_none());
        assert!(entry["speech"].get("text").is_none());
        assert!(entry["speech"].get("renderedHash").is_none());
        assert_eq!(entry["speech"]["voiceID"], "anna");
        let mut flat = meta.clone();
        flatten(&mut flat, "de").unwrap();
        let take = &flat.resources.as_ref().unwrap()[0];
        assert_eq!(take.filename, "r1.mp3");
        assert_eq!(take.extra["speech"]["text"], "Hello there.");
    }

    /// A machine may choose a voice for a person's script, not change its words.
    #[test]
    fn a_machine_may_choose_a_voice_for_a_persons_words() {
        let mut meta = narrated();
        let r1 = TranslationTarget::Resource("r1");
        set_string(&mut meta, r1, "de", "speech", Some("Hallo."), "person").unwrap();
        let r1 = TranslationTarget::Resource("r1");
        let voice = serde_json::json!({ "speech": { "voiceID": "karl" } });
        set_translation(&mut meta, r1, "de", &voice, "machine").unwrap();
        let r1 = TranslationTarget::Resource("r1");
        let words = serde_json::json!({ "speech": { "text": "Servus." } });
        assert!(set_translation(&mut meta, r1, "de", &words, "machine").is_err());
    }

    /// Where each take stands, for validate and the apps.
    #[test]
    fn takes_say_where_they_stand() {
        let mut meta = narrated();
        let state = |meta: &ProjectMetadata| takes(meta, "de")[0].state;
        let write = |meta: &mut ProjectMetadata, entry: serde_json::Value| {
            meta.resources.as_mut().unwrap()[0]
                .extra
                .insert(LOCALIZED_KEY.into(), serde_json::json!({ "de": entry }));
        };
        assert_eq!(
            state(&meta),
            TakeState::OutOfDate,
            "\"german\" is no receipt"
        );
        assert_eq!(takes(&meta, "de")[0].seconds, Some(9.4));
        assert_eq!(takes(&meta, "de")[0].source_seconds, Some(6.0));
        let text = "Hallo zusammen.";
        write(
            &mut meta,
            serde_json::json!({ "filename": "r1.de.mp3", "duration": 9.4, "speech": {
            "text": text, "provider": "elevenlabs", "voiceID": "anna",
            "renderedHash": crate::speech::fingerprint("elevenlabs", "anna", text) } }),
        );
        assert_eq!(state(&meta), TakeState::Current);
        write(&mut meta, serde_json::json!({ "speech": { "text": text } }));
        assert_eq!(state(&meta), TakeState::NoVoice);
        write(
            &mut meta,
            serde_json::json!({ "speech": { "text": text, "voiceID": "anna" } }),
        );
        assert_eq!(state(&meta), TakeState::NotGenerated);
        write(
            &mut meta,
            serde_json::json!({ "filename": "mine.m4a", "duration": 3, "speech": {
            "text": text } }),
        );
        assert_eq!(state(&meta), TakeState::Recorded);
        write(
            &mut meta,
            serde_json::json!({ "filename": "r1.de.mp3", "duration": 3, "speech": {
            "text": text, "voiceID": "anna", "placeholder": true } }),
        );
        assert_eq!(state(&meta), TakeState::Placeholder);
        write(
            &mut meta,
            serde_json::json!({ "speech": { "text": text, "provider": "local",
            "voiceID": "kokoro-en-q8/af_heart" } }),
        );
        assert_eq!(takes(&meta, "de")[0].voice_speaks.as_deref(), Some("en"));
        write(
            &mut meta,
            serde_json::json!({ "speech": { "text": text, "provider": "google",
            "voiceID": "de-DE-Neural2-F" } }),
        );
        assert_eq!(takes(&meta, "de")[0].voice_speaks, None);
        write(
            &mut meta,
            serde_json::json!({ "speech": { "text": text, "voiceID": "alloy" } }),
        );
        assert_eq!(takes(&meta, "de")[0].voice_speaks, None);
    }

    /// A language's file is the resource's: the folder listing makes no new
    /// resource of it.
    #[test]
    fn a_languages_file_is_not_a_new_resource() {
        let meta = narrated();
        let resolved = crate::inventory::effective_resources(
            &meta,
            &[
                "r1.mp3".to_string(),
                "r1.de.mp3".to_string(),
                "new.mp3".to_string(),
            ],
        );
        let names: Vec<&str> = resolved
            .iter()
            .map(|r| r.resource.filename.as_str())
            .collect();
        assert_eq!(names, ["r1.mp3", "new.mp3"]);
    }

    fn nested(resources: Value, layers: Value) -> ProjectMetadata {
        let mut doc = serde_json::to_value(meta(resources)).unwrap();
        doc["layers"] = layers;
        ProjectMetadata::from_json(&doc.to_string()).unwrap()
    }

    /// Nothing is translated into a composition a language owns: it is written
    /// in that language, so its sizes are not scaled again and its words are
    /// not looked up in the source's script. A copy of the source's is, once,
    /// at the moment it is made.
    #[test]
    fn nothing_is_translated_into_a_composition_a_language_owns() {
        let keyframes = json!([{"id": "k", "time": 0, "transitionDuration": 0, "fontSize": 60}]);
        let inner = json!({"id": "in", "name": "in", "sortIndex": 0, "kind": "caption",
                           "isEnabled": true, "startTime": 0, "duration": 2, "resourceID": "c",
                           "keyframes": keyframes});
        let reel = json!({"id": "reel", "kind": "composition", "filename": "",
                          "displayName": "Reel", "addedAt": 0, "duration": 2,
                          "composition": {"canvasWidth": 100, "canvasHeight": 100,
                                          "layers": [inner.clone()]}});
        let size = |m: &ProjectMetadata| {
            m.resources.as_ref().unwrap()[1]
                .composition
                .as_ref()
                .unwrap()
                .layers[0]
                .keyframes[0]
                .font_size
        };
        let mut shared = nested(json!([caption(), reel.clone()]), json!([]));
        flatten(&mut shared, "de").unwrap();
        assert_eq!(size(&shared), Some(40.0), "the source's layers are scaled");

        let mut forked = nested(json!([caption(), reel.clone()]), json!([]));
        fork_composition(&mut forked, "reel", "de", "person").unwrap();
        let owned = own_composition(&forked.resources.as_ref().unwrap()[1], "de").unwrap();
        assert_eq!(owned.layers[0].keyframes[0].font_size, Some(40.0));
        flatten(&mut forked, "de").unwrap();
        assert_eq!(size(&forked), Some(40.0), "scaled once, when it was copied");

        // The other languages still share the source's.
        let mut japanese = nested(json!([caption(), reel]), json!([]));
        fork_composition(&mut japanese, "reel", "de", "person").unwrap();
        flatten(&mut japanese, "ja").unwrap();
        assert_eq!(size(&japanese), Some(60.0));
    }

    #[test]
    fn a_replaced_file_leaves_the_library_and_the_source_files_facts() {
        let image = json!({"id": "i", "kind": "image", "filename": "shared.png",
                           "displayName": "i", "addedAt": 0, "libraryID": "lib",
                           "pixelWidth": 10, "pixelHeight": 10, "imageCuts":
                               [{"id": "cut", "filename": "shared-cut.png", "rect": [[0, 0], [1, 1]], "createdAt": 0}],
                           "localized": {
                               "de": {"filename": "mine.de.png", "pixelWidth": 20},
                               "ja": {"captionStyle": {"fontSize": 1}}}});
        let mut m = meta(json!([image.clone()]));
        flatten(&mut m, "de").unwrap();
        let r = resource(&m);
        assert_eq!(r.filename, "mine.de.png");
        assert!(r.extra.get("libraryID").is_none());
        assert_eq!((r.pixel_width, r.pixel_height), (Some(20.0), None));
        assert!(r.image_cuts.is_empty());

        let mut m = meta(json!([image]));
        flatten(&mut m, "ja").unwrap();
        assert!(
            resource(&m).extra.contains_key("libraryID"),
            "its file is the library's"
        );
    }

    #[test]
    fn a_language_pointing_at_a_resource_is_found() {
        let reel = json!({"id": "reel", "kind": "composition", "filename": "",
                          "displayName": "Reel", "addedAt": 0, "duration": 2,
                          "composition": {"canvasWidth": 100, "canvasHeight": 100, "layers": []},
                          "localized": {"de": {"composition": {"layers": [
                              {"id": "n", "name": "n", "sortIndex": 0, "kind": "image",
                               "isEnabled": true, "startTime": 0, "duration": 1,
                               "resourceID": "only-german", "keyframes": []}]}}}});
        let m = meta(json!([reel]));
        let r = resource(&m);
        assert_eq!(
            language_pointing_at(r, "only-german").as_deref(),
            Some("de")
        );
        assert_eq!(language_pointing_at(r, "nothing"), None);
    }
}
