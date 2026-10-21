//! The beat analysis cache (docs/BEAT-SYNC.md §3.5): `Resources/.beats/
//! <first 32 hex of the SHA-256 of the song's bytes>.json`, keyed by
//! content so a moved or renamed song finds its analysis, authoritative
//! once written. Here, beside the wire types, so every reader — the CLI
//! that analyses, explain and the apps that only read — names the files
//! the same way.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::{ProjectMetadata, ProjectResource};

/// The apps' shared resource library's folder of files, when the host has
/// one: a resource that is a library reference (`libraryID`) keeps its file
/// there, not in the project's `Resources/`. Process-wide — one library a
/// device — and unset for the CLI, which reads only the project.
static LIBRARY_FILES: std::sync::RwLock<Option<PathBuf>> = std::sync::RwLock::new(None);

/// Tells the cache where the shared library keeps its files (None: no
/// library). The apps call it once their library is known.
pub fn set_library_files(dir: Option<PathBuf>) {
    if let Ok(mut slot) = LIBRARY_FILES.write() {
        *slot = dir;
    }
}

/// Where a song's bytes are: the shared library's folder for a library
/// reference (when the host named one), the project's `Resources/` else.
pub fn song_file(dir: &Path, resource: &ProjectResource) -> PathBuf {
    if resource.extra.contains_key("libraryID") {
        if let Some(library) = LIBRARY_FILES.read().ok().and_then(|slot| slot.clone()) {
            return library.join(&resource.filename);
        }
    }
    dir.join("Resources").join(&resource.filename)
}

/// The file's name, the first 32 hex digits of the SHA-256 of its bytes,
/// and its size.
///
/// Hashing a song is the slow part of finding its cache, so the process
/// remembers each answer by (path, size, modification time): a file that
/// changed in either is hashed again.
pub fn source_of(path: &Path) -> Result<crate::musical::BeatSource, String> {
    use sha2::Digest;
    use std::sync::atomic::Ordering;
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stamp = std::fs::metadata(path)
        .ok()
        .and_then(|m| Some((m.len(), m.modified().ok()?)));
    let memo_key = stamp.map(|(size, modified)| (path.to_path_buf(), size, modified));
    if let Some(key) = &memo_key {
        if let Some((sha256, bytes)) = memo().lock().ok().and_then(|m| m.get(key).cloned()) {
            return Ok(crate::musical::BeatSource {
                filename,
                sha256,
                bytes,
            });
        }
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    HASHED.fetch_add(1, Ordering::Relaxed);
    let digest = sha2::Sha256::digest(&bytes);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let source = crate::musical::BeatSource {
        filename,
        sha256: hex[..32].to_string(),
        bytes: bytes.len() as u64,
    };
    if let (Some(key), Ok(mut memo)) = (memo_key, memo().lock()) {
        memo.insert(key, (source.sha256.clone(), source.bytes));
    }
    Ok(source)
}

type MemoKey = (PathBuf, u64, std::time::SystemTime);

fn memo() -> &'static std::sync::Mutex<std::collections::HashMap<MemoKey, (String, u64)>> {
    static MEMO: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<MemoKey, (String, u64)>>,
    > = std::sync::OnceLock::new();
    MEMO.get_or_init(Default::default)
}

static HASHED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How many times this process has hashed a file's bytes — what the memo
/// saves, for the tests that hold it to that.
pub fn files_hashed() -> usize {
    HASHED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Where an analysis of content `key` lives in a project:
/// `Resources/.beats/<key>.json`.
pub fn cache_path(project: &Path, key: &str) -> PathBuf {
    project
        .join("Resources")
        .join(".beats")
        .join(format!("{key}.json"))
}

/// The cache key of a song's analysis: its content's, and the tempo hint
/// it was measured with — a hinted and an unhinted analysis of one file
/// are two answers, and neither overwrites the other.
pub fn cache_key(sha256: &str, hint: Option<f64>) -> String {
    match hint.filter(|h| h.is_finite() && (20.0..=400.0).contains(h)) {
        None => sha256.to_string(),
        Some(h) => {
            let text = format!("{:.3}", h);
            let text = text.trim_end_matches('0').trim_end_matches('.');
            format!("{sha256}-hint{text}")
        }
    }
}

/// The project's cached analysis of `audio` measured with `hint` (none:
/// unhinted), by content, when there is one made by this tracker or a
/// later one. An older tracker's answer is not the answer any more — it is
/// measured again (and replaced) like a song never analysed; anything else
/// in the cache is authoritative.
pub fn load_cached(
    project: &Path,
    audio: &Path,
    hint: Option<f64>,
) -> Option<crate::musical::BeatAnalysis> {
    let key = cache_key(&source_of(audio).ok()?.sha256, hint);
    let text = std::fs::read_to_string(cache_path(project, &key)).ok()?;
    let analysis: crate::musical::BeatAnalysis = serde_json::from_str(&text).ok()?;
    (analysis.tracker_version >= crate::musical::TRACKER_VERSION).then_some(analysis)
}

/// Writes `analysis` (of `audio`) into the project's cache, `source` filled
/// in, under the key of the hint it was measured with, and returns what
/// was written.
pub fn store(
    project: &Path,
    audio: &Path,
    mut analysis: crate::musical::BeatAnalysis,
) -> Result<crate::musical::BeatAnalysis, String> {
    let source = source_of(audio)?;
    let path = cache_path(project, &cache_key(&source.sha256, analysis.bpm_hint));
    analysis.source = Some(source);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(&analysis).map_err(|e| e.to_string())?;
    std::fs::write(&path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(analysis)
}

/// A song a beat clock counts.
#[derive(Debug, Clone, PartialEq)]
pub struct ClockSong {
    pub id: String,
    /// Its bytes (`song_file`).
    pub file: PathBuf,
    /// The tempo hint the resource keeps (`bpmHint`).
    pub hint: Option<f64>,
    /// It states its own `beatGrid` — the analysis is for its sections.
    pub stated: bool,
}

/// The project's songs a beat clock counts — the settings', a
/// composition's, a layer's clock — with their files (`song_file`:
/// `Resources/`, or the shared library's folder for a library reference),
/// their hints, and whether they state their grid. The cache itself stays
/// the project's.
pub fn clock_songs(dir: &Path, meta: &ProjectMetadata) -> Vec<ClockSong> {
    let mut ids: Vec<String> = Vec::new();
    let mut note = |clock: &Option<crate::musical::BeatClock>| {
        if let Some(id) = clock.as_ref().and_then(|c| c.resource_id.clone()) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    };
    note(&meta.composition_settings.beat_clock);
    for resource in meta.resources.iter().flatten() {
        if let Some(composition) = &resource.composition {
            note(&composition.beat_clock);
        }
    }
    for layer in crate::nesting::all_layers(meta) {
        note(&layer.beat_clock);
    }
    ids.into_iter()
        .filter_map(|id| {
            let resource = meta.resources.iter().flatten().find(|r| r.id == id)?;
            let plain = !resource.filename.is_empty()
                && !resource.filename.contains('/')
                && !resource.filename.contains('\\')
                && resource.filename != ".."
                && resource.filename != ".";
            (resource.kind == crate::ProjectResourceKind::Audio && plain).then(|| ClockSong {
                file: song_file(dir, resource),
                hint: resource.bpm_hint,
                stated: resource.beat_grid.is_some(),
                id,
            })
        })
        .collect()
}

/// The cached analysis of every clock song that has one (with the song's
/// hint) — what a reader that never analyses counts beats on, and where a
/// stated grid's song keeps its sections.
pub fn cached_analyses(
    dir: &Path,
    meta: &ProjectMetadata,
) -> BTreeMap<String, crate::musical::BeatAnalysis> {
    clock_songs(dir, meta)
        .into_iter()
        .filter_map(|song| Some((song.id, load_cached(dir, &song.file, song.hint)?)))
        .collect()
}
