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
