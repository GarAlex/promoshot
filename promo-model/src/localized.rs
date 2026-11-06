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
    }
    let mut patch = patch.clone();
    for key in ENTRY_KEYS {
        patch.remove(*key);
    }
    merge_patch(&mut doc, &Value::Object(patch));
    serde_json::from_value(doc).map_err(|e| e.to_string())
}

/// `meta` as an ordinary project in `language`: each resource's entry for
/// it applied, and every `localized` gone. The source language only loses
/// the blocks. A language not on the project's list is refused, as is any
/// entry that cannot apply: a render never shows half a translation.
pub fn flatten(meta: &mut ProjectMetadata, language: &str) -> Result<(), String> {
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
    for resource in meta.resources.iter_mut().flatten() {
        let entry = entries(resource)
            .filter(|_| !is_source)
            .and_then(|all| all.get(language))
            .cloned();
        match entry {
            Some(entry) => {
                let id = resource.id.clone();
                *resource = patched(resource, &entry)
                    .map_err(|why| format!("resource \"{id}\", language \"{language}\": {why}"))?;
            }
            None => {
                resource.extra.remove(LOCALIZED_KEY);
            }
        }
    }
    Ok(())
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
            if layer.caption_text.is_some() && !on_resource {
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

/// The strings of a resource that a translation answers for: `(key, text)`,
/// the key as [`StringRow::key`] spells it. A resource's own words, not
/// what plays them.
fn source_strings(resource: &ProjectResource) -> Vec<(String, String)> {
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
        if let Some(text) = layer.caption_text.as_ref().filter(|_| !on_resource) {
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LanguageSummary {
    pub language: String,
    pub strings: usize,
    /// Each missing or stale string, as `<resource or layer> <key>`.
    pub missing: Vec<String>,
    pub stale: Vec<String>,
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
    let names_text = ["captionText", "recipe", "speech", "svg"]
        .iter()
        .any(|key| fields.contains_key(*key));
    if names_text && by == "machine" && entry["by"].as_str() == Some("person") {
        return Err(format!(
            "resource {resource_id}: a person wrote its {language} text, and a machine does not \
             overwrite that"
        ));
    }
    merge_patch(&mut entry, patch);
    if names_text {
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
}
