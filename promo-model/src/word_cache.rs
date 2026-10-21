//! The word-timing cache (docs/SPEECH-SYNC.md §1.3): `Resources/.words/
//! <audio32>-<script8>.json` — the first 32 hex digits of the SHA-256 of
//! the take's bytes and 8 of its script's. Keyed by content, a record
//! follows its take through renames, library moves and duplicated
//! projects; a re-made take is new bytes, so a new key. Authoritative:
//! provider timing cannot be recomputed, so it is stored the moment a take
//! is bought, before the document. The only code that knows the naming.

use std::path::{Path, PathBuf};

use crate::speech::{script_hash, TakeTiming};
use crate::{ProjectMetadata, ProjectResourceKind};

/// The folder of a project's records.
pub fn cache_dir(project: &Path) -> PathBuf {
    cache_dir_in(&project.join("Resources"))
}

/// The folder of the records beside a resources folder — what a host that
/// knows only where it writes takes (the apps' synthesizer) names.
pub fn cache_dir_in(resources: &Path) -> PathBuf {
    resources.join(".words")
}

/// Where the record of audio `audio32` saying `script8` lives.
pub fn cache_path(project: &Path, audio32: &str, script8: &str) -> PathBuf {
    cache_path_in(&project.join("Resources"), audio32, script8)
}

/// [`cache_path`] beside a resources folder.
pub fn cache_path_in(resources: &Path, audio32: &str, script8: &str) -> PathBuf {
    cache_dir_in(resources).join(format!("{audio32}-{script8}.json"))
}

/// A record found for a take.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub timing: TakeTiming,
    /// The record is for this audio under ANOTHER script: the text was
    /// edited and no new take was bought yet. It still says what the take
    /// says; validate warns.
    pub stale: bool,
    pub path: PathBuf,
}

fn read(path: &Path) -> Option<TakeTiming> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Every record of audio `audio32`, with its path.
pub fn records_for(project: &Path, audio32: &str) -> Vec<(PathBuf, TakeTiming)> {
    records_for_in(&project.join("Resources"), audio32)
}

/// [`records_for`] beside a resources folder.
pub fn records_for_in(resources: &Path, audio32: &str) -> Vec<(PathBuf, TakeTiming)> {
    let prefix = format!("{audio32}-");
    let Ok(entries) = std::fs::read_dir(cache_dir_in(resources)) else {
        return Vec::new();
    };
    let mut out: Vec<(PathBuf, TakeTiming)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".json"))
        })
        .filter_map(|p| Some((p.clone(), read(&p)?)))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// The record of the take in `audio` (a file) saying `script`:
/// 1. the exact `<audio32>-<script8>`;
/// 2. else a record of that audio under another script — the take is
///    stale; of several, the newest by `timingVersion`, then by file
///    modification time;
/// 3. else none: the take needs timing.
pub fn lookup(project: &Path, audio: &Path, script: &str) -> Option<Found> {
    lookup_in(&project.join("Resources"), audio, script)
}

/// [`lookup`] beside a resources folder.
pub fn lookup_in(resources: &Path, audio: &Path, script: &str) -> Option<Found> {
    let source = crate::media_cache::source_of(audio).ok()?;
    let exact = cache_path_in(resources, &source.sha256, &script_hash(script));
    if let Some(timing) = read(&exact) {
        return Some(Found {
            timing,
            stale: false,
            path: exact,
        });
    }
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    records_for_in(resources, &source.sha256)
        .into_iter()
        .max_by(|a, b| {
            a.1.timing_version
                .cmp(&b.1.timing_version)
                .then_with(|| mtime(&a.0).cmp(&mtime(&b.0)))
        })
        .map(|(path, timing)| Found {
            timing,
            stale: true,
            path,
        })
}

/// Writes the record of the take in `audio`: its `audio` source and
/// `scriptHash` filled in from the file and the record's own script.
/// Answers what was written.
pub fn store(project: &Path, audio: &Path, timing: TakeTiming) -> Result<TakeTiming, String> {
    store_in(&project.join("Resources"), audio, timing)
}

/// [`store`] beside a resources folder.
pub fn store_in(
    resources: &Path,
    audio: &Path,
    mut timing: TakeTiming,
) -> Result<TakeTiming, String> {
    let source = crate::media_cache::source_of(audio)?;
    timing.script_hash = script_hash(&timing.script);
    let path = cache_path_in(resources, &source.sha256, &timing.script_hash);
    timing.audio = source;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(&timing).map_err(|e| e.to_string())?;
    std::fs::write(&path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(timing)
}

/// Removes the records of audio `audio32` — bytes a re-made take has just
/// replaced — except those under a script in `keep` (another file with the
/// same bytes still says it: two silent placeholders of one length are one
/// audio). Answers how many went.
pub fn retire_in(resources: &Path, audio32: &str, keep: &[String]) -> usize {
    records_for_in(resources, audio32)
        .into_iter()
        .filter(|(_, timing)| !keep.contains(&script_hash(&timing.script)))
        .filter(|(path, _)| std::fs::remove_file(path).is_ok())
        .count()
}

/// Whether a found record is the answer, by the cache's rules (§1.3): a
/// provider's, a local voice's or a transcription's timing is
/// authoritative and stands — and `refresh` cannot replace it (an error
/// says why); an aligned or estimated record stands when this aligner (or
/// estimate) or a later one made it; a stale record says what the take
/// says, so it stands. False: align the take (again).
pub fn usable(found: &Found, refresh: bool, take: &str) -> Result<bool, String> {
    use crate::speech::{TimingSource, ALIGNER_VERSION, ESTIMATE_VERSION};
    if found.timing.source.is_authoritative() {
        if refresh {
            return Err(format!(
                "\"{take}\" is timed by {} — provider timing is authoritative; re-voice the take \
                 to replace it",
                found.timing.source.as_text()
            ));
        }
        return Ok(true);
    }
    if refresh {
        return Ok(false);
    }
    let made_by = found.timing.aligner_version.unwrap_or(0);
    let current = match found.timing.source {
        TimingSource::Aligned => made_by >= ALIGNER_VERSION,
        TimingSource::Estimate => made_by >= ESTIMATE_VERSION,
        _ => true,
    };
    Ok(current || found.stale)
}

/// A take in a project: a resource whose file says a script.
#[derive(Debug, Clone, PartialEq)]
pub struct Take {
    pub id: String,
    /// Its bytes (`beat_cache::song_file`: `Resources/`, or the shared
    /// library's folder for a library reference).
    pub file: PathBuf,
    /// `speech.text`, else `transcript`.
    pub script: String,
}

/// The project's takes: every audio or video resource with a plain
/// filename whose script says something ([`crate::speech::take_script`]).
pub fn takes(dir: &Path, meta: &ProjectMetadata) -> Vec<Take> {
    meta.resources
        .iter()
        .flatten()
        .filter(|r| {
            matches!(
                r.kind,
                ProjectResourceKind::Audio | ProjectResourceKind::Video
            )
        })
        .filter(|r| {
            !r.filename.is_empty()
                && !r.filename.contains('/')
                && !r.filename.contains('\\')
                && !r.filename.starts_with('.')
        })
        .filter_map(|r| {
            Some(Take {
                id: r.id.clone(),
                file: crate::beat_cache::song_file(dir, r),
                script: crate::speech::take_script(r)?,
            })
        })
        .collect()
}

/// The takes whose file is there and has no record at all — what an open
/// aligns (S5) and the apps' aligner works through.
pub fn takes_needing_timing(dir: &Path, meta: &ProjectMetadata) -> Vec<Take> {
    takes(dir, meta)
        .into_iter()
        .filter(|t| t.file.is_file() && lookup(dir, &t.file, &t.script).is_none())
        .collect()
}

/// The take a `promo words` of a project times: the one named, else the
/// project's only take — both hosts' rule, in the CLI's words.
pub fn choose_take<'a>(
    meta: &'a ProjectMetadata,
    resource: Option<&str>,
) -> Result<&'a crate::ProjectResource, String> {
    let all: Vec<&crate::ProjectResource> = meta.resources.iter().flatten().collect();
    if let Some(id) = resource {
        let found = all
            .iter()
            .find(|r| r.id == id)
            .ok_or_else(|| format!("no resource `{id}` in the project"))?;
        if crate::speech::take_script(found).is_none() {
            return Err(format!(
                "`{id}` says nothing: give it `speech.text`, or a `transcript` for a recording"
            ));
        }
        return Ok(found);
    }
    let takes: Vec<&crate::ProjectResource> = all
        .into_iter()
        .filter(|r| {
            matches!(
                r.kind,
                ProjectResourceKind::Audio | ProjectResourceKind::Video
            ) && crate::speech::take_script(r).is_some()
        })
        .collect();
    match takes.as_slice() {
        [one] => Ok(one),
        [] => Err(
            "the project has no take: no audio or video resource with `speech.text` or \
                   a `transcript`"
                .into(),
        ),
        many => Err(format!(
            "name the take with --resource — the project's takes are {}",
            many.iter()
                .map(|r| format!("`{}`", r.id))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// `promo words`' JSON on every host: the record, plus — for a project's
/// take — `fromCache` (false: nothing is stored yet and these are an
/// estimate) and `stale` when the record follows an older script.
pub fn words_answer(
    timing: &TakeTiming,
    from_cache: Option<bool>,
    stale: bool,
) -> Result<serde_json::Value, String> {
    let mut value = serde_json::to_value(timing).map_err(|e| e.to_string())?;
    if let Some(cached) = from_cache {
        value["fromCache"] = serde_json::json!(cached);
    }
    if stale {
        value["stale"] = serde_json::json!(true);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speech::{TimingSource, TIMING_VERSION};

    fn project(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("promo-words-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        dir
    }

    fn timing(script: &str) -> TakeTiming {
        TakeTiming {
            timing_version: TIMING_VERSION,
            audio: Default::default(),
            script: script.into(),
            script_hash: String::new(),
            duration: 1.0,
            source: TimingSource::Estimate,
            estimated: true,
            confidence: 0.3,
            words: Vec::new(),
            sentences: Vec::new(),
            aligner_version: Some(1),
            extra: Default::default(),
        }
    }

    /// Two byte-identical files saying different scripts are two records;
    /// one file's record is found under a new name.
    #[test]
    fn cache_key_is_audio_and_script() {
        let dir = project("key");
        let a = dir.join("Resources/a.mp3");
        let b = dir.join("Resources/b.mp3");
        std::fs::write(&a, b"same silent bytes").unwrap();
        std::fs::write(&b, b"same silent bytes").unwrap();
        let stored = store(&dir, &a, timing("Hello there.")).unwrap();
        store(&dir, &b, timing("Goodbye.")).unwrap();
        assert_eq!(stored.audio.sha256.len(), 32);
        assert_eq!(stored.audio.filename, "a.mp3");
        assert_eq!(stored.script_hash, script_hash("Hello there."));
        let name = format!("{}-{}.json", stored.audio.sha256, stored.script_hash);
        assert!(cache_dir(&dir).join(&name).is_file(), "{name}");
        assert_eq!(records_for(&dir, &stored.audio.sha256).len(), 2);
        let found = lookup(&dir, &b, "Hello there.").unwrap();
        assert!(!found.stale);
        assert_eq!(found.timing.script, "Hello there.");
        // Renamed: the content still finds it.
        let moved = dir.join("Resources/renamed.mp3");
        std::fs::rename(&a, &moved).unwrap();
        assert!(!lookup(&dir, &moved, "Goodbye.").unwrap().stale);
        // Other bytes: nothing.
        std::fs::write(&moved, b"a new take").unwrap();
        assert!(lookup(&dir, &moved, "Goodbye.").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An edited script with no new take finds the take's old record,
    /// marked stale; of two, the later one.
    #[test]
    fn stale_take_finds_its_old_record() {
        let dir = project("stale");
        let take = dir.join("Resources/vo.mp3");
        std::fs::write(&take, b"voice bytes").unwrap();
        store(&dir, &take, timing("The first script.")).unwrap();
        let found = lookup(&dir, &take, "An edited script.").unwrap();
        assert!(found.stale);
        assert_eq!(found.timing.script, "The first script.");
        std::thread::sleep(std::time::Duration::from_millis(20));
        store(&dir, &take, timing("The second script.")).unwrap();
        let found = lookup(&dir, &take, "An edited script.").unwrap();
        assert_eq!(found.timing.script, "The second script.");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn takes_needing_timing_are_the_ones_without_a_record() {
        let dir = project("needing");
        std::fs::write(dir.join("Resources/vo.mp3"), b"vo").unwrap();
        std::fs::write(dir.join("Resources/rec.m4a"), b"recording").unwrap();
        std::fs::write(dir.join("Resources/song.m4a"), b"music").unwrap();
        let meta: ProjectMetadata = serde_json::from_value(serde_json::json!({
            "id": "P", "name": "W", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 1, "videoDuration": 1, "subtitles": [],
            "compositionSettings": {},
            "resources": [
                {"id": "vo", "kind": "audio", "filename": "vo.mp3", "displayName": "VO",
                 "addedAt": 0, "speech": {"text": "Spoken words.", "provider": "openai"},
                 "transcript": "ignored: speech wins"},
                {"id": "rec", "kind": "audio", "filename": "rec.m4a", "displayName": "Rec",
                 "addedAt": 0, "transcript": "What the recording says."},
                {"id": "song", "kind": "audio", "filename": "song.m4a", "displayName": "Song",
                 "addedAt": 0},
                {"id": "gone", "kind": "audio", "filename": "missing.mp3", "displayName": "G",
                 "addedAt": 0, "transcript": "Not there."}
            ]
        }))
        .unwrap();
        let all = takes(&dir, &meta);
        assert_eq!(
            all.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            ["vo", "rec", "gone"]
        );
        assert_eq!(all[0].script, "Spoken words.");
        let needing = takes_needing_timing(&dir, &meta);
        assert_eq!(
            needing.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            ["vo", "rec"]
        );
        store(&dir, &all[1].file, timing("What the recording says.")).unwrap();
        let needing = takes_needing_timing(&dir, &meta);
        assert_eq!(
            needing.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            ["vo"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
