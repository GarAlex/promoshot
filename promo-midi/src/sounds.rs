//! Where SoundFont banks live, and installing the one PromoShot renders
//! with (MIDI.md §0): GeneralUser GS v2.0.3, pinned by size and SHA-256,
//! its licence fetched beside it. Mirrors the local voices' folder
//! (`promo_speech::local::voices_dir`): `PROMOSHOT_SOUNDS` when set, else
//! the per-user data folder; a host that keeps its own folder names it
//! with [`set_sounds_dir`]. The apps pass their bundled copy's path to the
//! render instead and never install.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// A bank this build knows by its pinned bytes.
#[derive(Debug, Clone, Copy)]
pub struct KnownBank {
    /// What `promoshot-mcp sounds install <id>` takes.
    pub id: &'static str,
    /// The name a take's `bank.name` records.
    pub title: &'static str,
    /// The folder it installs into, inside the sounds folder.
    pub folder: &'static str,
    pub file: &'static str,
    pub url: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
    pub license_url: &'static str,
    pub license_sha256: &'static str,
    pub license: &'static str,
}

pub const GENERALUSER: KnownBank = KnownBank {
    id: "generaluser",
    title: "GeneralUser GS 2.0.3",
    folder: "GeneralUser-GS-2.0.3",
    file: "GeneralUser-GS.sf2",
    url: "https://raw.githubusercontent.com/mrbumpy409/GeneralUser-GS/main/GeneralUser-GS.sf2",
    bytes: 32_319_396,
    sha256: "9575028c7a1f589f5770fccc8cff2734566af40cd26ed836944e9a5152688cfe",
    license_url:
        "https://raw.githubusercontent.com/mrbumpy409/GeneralUser-GS/main/documentation/LICENSE.txt",
    license_sha256: "7b32efefdf95ce38a043799f0659853ddc00fbaa14d8c50f0aca16b9b8b405be",
    license: "GeneralUser GS License v2.0 (use and bundling in commercial software allowed)",
};

pub const BANKS: [KnownBank; 1] = [GENERALUSER];

/// The known bank whose `.sf2` has this SHA-256.
pub fn known(sha256: &str) -> Option<&'static KnownBank> {
    BANKS.iter().find(|b| b.sha256.eq_ignore_ascii_case(sha256))
}

fn host_dir() -> &'static Mutex<Option<PathBuf>> {
    static DIR: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
    DIR.get_or_init(|| Mutex::new(None))
}

/// The host's sounds folder, explicitly; `None` returns to
/// [`sounds_dir`]'s own rule.
pub fn set_sounds_dir(dir: Option<PathBuf>) {
    *host_dir().lock().unwrap_or_else(|e| e.into_inner()) = dir;
}

/// Where banks are installed: the host's folder, else `PROMOSHOT_SOUNDS`,
/// else `~/Library/Application Support/PromoShot/Sounds` on macOS,
/// `$XDG_DATA_HOME/promoshot/sounds` (or `~/.local/share/promoshot/sounds`)
/// on Linux, `%APPDATA%\PromoShot\Sounds` on Windows.
pub fn sounds_dir() -> PathBuf {
    if let Some(dir) = host_dir().lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return dir;
    }
    if let Some(dir) = std::env::var_os("PROMOSHOT_SOUNDS").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = || {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
    };
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/PromoShot/Sounds")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(home)
            .join("PromoShot")
            .join("Sounds")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/share"))
            .join("promoshot")
            .join("sounds")
    }
}

/// Where a known bank's `.sf2` sits once installed in `dir`.
pub fn bank_path(bank: &KnownBank, dir: &Path) -> PathBuf {
    dir.join(bank.folder).join(bank.file)
}

/// The installed default bank, if it is there (checked by size; the
/// render checks the hash).
pub fn installed_default() -> Option<PathBuf> {
    let path = bank_path(&GENERALUSER, &sounds_dir());
    std::fs::metadata(&path)
        .ok()
        .filter(|m| m.len() == GENERALUSER.bytes)
        .map(|_| path)
}

/// What to tell a person with no bank installed.
pub fn missing_bank_message() -> String {
    format!(
        "no sound bank installed in {} — install GeneralUser GS (32 MB) with \
         `promoshot-mcp sounds install generaluser` (or `promo sounds install generaluser`)",
        sounds_dir().display()
    )
}

/// Downloads and installs `id` into `dir`: the `.sf2` and its licence,
/// each checked against its pinned SHA-256 (and the bank against its
/// size), written beside their final names and moved in only once both
/// match. An installed bank whose bytes match is left alone. `progress`
/// hears bytes done of total.
#[cfg(feature = "download")]
pub fn install(
    id: &str,
    dir: &Path,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<PathBuf, String> {
    let bank = BANKS.iter().find(|b| b.id == id).ok_or_else(|| {
        let ids: Vec<&str> = BANKS.iter().map(|b| b.id).collect();
        format!("unknown sound bank `{id}` — one of {}", ids.join(", "))
    })?;
    let folder = dir.join(bank.folder);
    let target = folder.join(bank.file);
    let license = folder.join("LICENSE.txt");
    if let Ok(bytes) = std::fs::read(&target) {
        if crate::render::sha_hex(&bytes) == bank.sha256 && license.exists() {
            return Ok(target);
        }
    }
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let sf2 = fetch(bank.url, Some(bank.bytes), progress)?;
    if sf2.len() as u64 != bank.bytes || crate::render::sha_hex(&sf2) != bank.sha256 {
        return Err(format!(
            "{}: the download does not match the pinned SHA-256 ({} bytes) — nothing installed",
            bank.url,
            sf2.len()
        ));
    }
    let text = fetch(bank.license_url, None, &mut |_, _| {})?;
    if crate::render::sha_hex(&text) != bank.license_sha256 {
        return Err(format!(
            "{}: the licence does not match its pinned SHA-256 — nothing installed",
            bank.license_url
        ));
    }
    write_atomic(&license, &text)?;
    write_atomic(&target, &sf2)?;
    Ok(target)
}

#[cfg(feature = "download")]
fn fetch(
    url: &str,
    expected: Option<u64>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let response = ureq::get(url).call().map_err(|e| format!("{url}: {e}"))?;
    let total = response
        .header("Content-Length")
        .and_then(|l| l.parse::<u64>().ok())
        .or(expected)
        .unwrap_or(0);
    let mut reader = response.into_reader().take(256 * 1024 * 1024);
    let mut out = Vec::with_capacity(total as usize);
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let n = reader
            .read(&mut buffer)
            .map_err(|e| format!("{url}: {e}"))?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buffer[..n]);
        progress(out.len() as u64, total);
    }
    Ok(out)
}

/// Writes `bytes` beside `path` and renames it into place.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temporary = path.with_file_name(format!(".{name}.partial-{}", std::process::id()));
    std::fs::write(&temporary, bytes).map_err(|e| format!("{}: {e}", temporary.display()))?;
    std::fs::rename(&temporary, path).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        format!("{}: {e}", path.display())
    })
}
