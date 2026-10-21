//! What a cached analysis is OF: a file's name, the first 32 hex digits of
//! the SHA-256 of its bytes, and its size. Every content-keyed cache in a
//! project — the beat analyses (`Resources/.beats/`, docs/BEAT-SYNC.md
//! §3.5) and the word timings (`Resources/.words/`, docs/SPEECH-SYNC.md
//! §1.3) — names its files by this hash, so a moved or renamed file finds
//! its answers and a re-made one does not.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The file an analysis is of: its name, the first 32 hex digits of the
/// SHA-256 of its bytes (the cache key), its size.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MediaSource {
    pub filename: String,
    pub sha256: String,
    pub bytes: u64,
}

/// The file's name, the first 32 hex digits of the SHA-256 of its bytes,
/// and its size.
///
/// Hashing a song or a voice-over is the slow part of finding its cache,
/// so the process remembers each answer by (path, size, modification
/// time): a file that changed in either is hashed again.
pub fn source_of(path: &Path) -> Result<MediaSource, String> {
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
            return Ok(MediaSource {
                filename,
                sha256,
                bytes,
            });
        }
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    HASHED.fetch_add(1, Ordering::Relaxed);
    let source = MediaSource {
        filename,
        sha256: content_key(&bytes),
        bytes: bytes.len() as u64,
    };
    if let (Some(key), Ok(mut memo)) = (memo_key, memo().lock()) {
        memo.insert(key, (source.sha256.clone(), source.bytes));
    }
    Ok(source)
}

/// The first 32 hex digits of the SHA-256 of `bytes` — the key a file's
/// cached answers are filed under.
pub fn content_key(bytes: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(bytes);
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
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
