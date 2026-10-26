//! Where the music model lives and how it gets there: a models folder
//! like the voices folder, one package (`ace-step-base`) of exactly the
//! files lego reads, pinned to Hugging Face commits with their sizes and
//! SHA-256, and an install with progress, cancel and resume. A package
//! appears only once every file is complete and verified.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use sha2::{Digest, Sha256};

use crate::pipeline::DIT_DIR;

/// One file of a package: its path under the package folder (the Hugging
/// Face layout; [`url`] says where it comes from) and what it must be.
#[derive(Debug, Clone, Copy)]
pub struct File {
    pub path: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct Package {
    pub id: &'static str,
    pub title: &'static str,
    pub license: &'static str,
    pub source: &'static str,
    pub files: &'static [File],
}

impl Package {
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.bytes).sum()
    }
}

/// ACE-Step/Ace-Step1.5 (the VAE and the text encoder) and
/// ACE-Step/acestep-v15-base (the DiT), at the commits the port was
/// verified against (2026-10-26).
const MAIN: &str =
    "https://huggingface.co/ACE-Step/Ace-Step1.5/resolve/19671f406d603126926c1b7e2adc169acbcade22";
const BASE: &str = "https://huggingface.co/ACE-Step/acestep-v15-base/resolve/e432212fec32b8965a14ffa57ae653438d6abd14";

pub const ACE_STEP_BASE: &str = "ace-step-base";

static FILES: [File; 5] = [
    File {
        path: "acestep-v15-base/model.safetensors",
        bytes: 4_787_825_604,
        sha256: "4177f600501a6d4bd81cadaa0abac557ffd15c54e5c8cb52053cdb24a0844d6b",
    },
    File {
        path: "acestep-v15-base/silence_latent.pt",
        bytes: 3_841_215,
        sha256: "a778e9dd942f5e8b2c09c55370782d318834432b03dabbcdf70e6ed49ad6358b",
    },
    File {
        path: "vae/diffusion_pytorch_model.safetensors",
        bytes: 337_431_388,
        sha256: "da17edb604c40deaf09e9b24974e590d1ca83a374070e5d0884cfa4bed9a99b0",
    },
    File {
        path: "Qwen3-Embedding-0.6B/model.safetensors",
        bytes: 1_191_586_416,
        sha256: "0437e45c94563b09e13cb7a64478fc406947a93cb34a7e05870fc8dcd48e23fd",
    },
    File {
        path: "Qwen3-Embedding-0.6B/tokenizer.json",
        bytes: 11_423_705,
        sha256: "def76fb086971c7867b829c23a26261e38d9d74e02139253b38aeb9df8b4b50a",
    },
];

static CATALOG: [Package; 1] = [Package {
    id: ACE_STEP_BASE,
    title: "ACE-Step 1.5 base (lego)",
    license: "MIT (ACE-Step 1.5); Apache-2.0 (Qwen3-Embedding-0.6B)",
    source: "https://huggingface.co/ACE-Step",
    files: &FILES,
}];

pub fn catalog() -> &'static [Package] {
    &CATALOG
}

pub fn package(id: &str) -> Result<&'static Package, String> {
    CATALOG.iter().find(|p| p.id == id).ok_or_else(|| {
        format!(
            "unknown music model {id:?} (known: {})",
            CATALOG.iter().map(|p| p.id).collect::<Vec<_>>().join(", ")
        )
    })
}

/// A file's download URL.
pub fn url(file: &File) -> String {
    let (base, rel) = if file.path.starts_with(DIT_DIR) {
        (
            BASE,
            file.path
                .trim_start_matches(DIT_DIR)
                .trim_start_matches('/'),
        )
    } else {
        (MAIN, file.path)
    };
    format!("{base}/{rel}?download=true")
}

fn host_dir() -> &'static Mutex<Option<PathBuf>> {
    static DIR: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
    DIR.get_or_init(|| Mutex::new(None))
}

/// Names the folder the apps keep the model in (their container); None
/// returns to the default.
pub fn set_models_dir(dir: Option<PathBuf>) {
    *host_dir().lock().unwrap_or_else(|e| e.into_inner()) = dir;
}

/// Where music models are installed: the host's folder when it named one,
/// else `PROMOSHOT_MUSIC_MODELS`, else the per-user data folder —
/// `~/Library/Application Support/PromoShot/Music` on macOS,
/// `$XDG_DATA_HOME/promoshot/music` (or `~/.local/share/promoshot/music`)
/// on Linux, `%APPDATA%\PromoShot\Music` on Windows.
pub fn models_dir() -> PathBuf {
    if let Some(dir) = host_dir().lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return dir;
    }
    if let Some(dir) = std::env::var_os("PROMOSHOT_MUSIC_MODELS").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = || {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
    };
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/PromoShot/Music")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(home)
            .join("PromoShot")
            .join("Music")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/share"))
            .join("promoshot")
            .join("music")
    }
}

/// A package's folder (it may not exist).
pub fn package_dir(dir: &Path, id: &str) -> PathBuf {
    dir.join(id)
}

/// Whether every file of `id` is present at its pinned size. (The hashes
/// are checked at install; a launch only stats.)
pub fn installed(dir: &Path, id: &str) -> bool {
    package(id).is_ok_and(|p| installed_package(dir, p))
}

fn installed_package(dir: &Path, p: &Package) -> bool {
    let root = package_dir(dir, p.id);
    p.files
        .iter()
        .all(|f| std::fs::metadata(root.join(f.path)).is_ok_and(|m| m.len() == f.bytes))
}

/// Removes an installed package (and any partial download). Ok(true) when
/// something was removed.
pub fn remove(dir: &Path, id: &str) -> Result<bool, String> {
    if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') || id.is_empty() {
        return Err(format!("bad package id {id:?}"));
    }
    let mut removed = false;
    for d in [package_dir(dir, id), partial_dir(dir, id)] {
        if d.exists() {
            std::fs::remove_dir_all(&d).map_err(|e| format!("remove {}: {e}", d.display()))?;
            removed = true;
        }
    }
    Ok(removed)
}

fn partial_dir(dir: &Path, id: &str) -> PathBuf {
    dir.join(".partial").join(id)
}

/// Install progress: bytes verified so far of the package's total.
#[derive(Debug, Clone, Copy, Default)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
}

pub const CANCELLED: &str = "install cancelled";

/// Downloads `id` into `dir`: each file into `.partial/<id>/` (resuming a
/// file already partly there), its SHA-256 checked against the pin, and the
/// whole folder moved to `<id>/` once all match. `progress` is called as
/// bytes arrive; `cancel` is polled between reads.
#[cfg(feature = "download")]
pub fn install(
    dir: &Path,
    id: &str,
    progress: &mut dyn FnMut(Progress),
    cancel: &dyn Fn() -> bool,
) -> Result<PathBuf, String> {
    install_from(dir, package(id)?, &url, progress, cancel)
}

/// [`install`] with the URLs from `url_of` (the tests serve a package locally).
#[cfg(feature = "download")]
pub fn install_from(
    dir: &Path,
    p: &Package,
    url_of: &dyn Fn(&File) -> String,
    progress: &mut dyn FnMut(Progress),
    cancel: &dyn Fn() -> bool,
) -> Result<PathBuf, String> {
    let id = p.id;
    let target = package_dir(dir, id);
    if installed_package(dir, p) {
        progress(Progress {
            done: p.bytes(),
            total: p.bytes(),
        });
        return Ok(target);
    }
    let staging = partial_dir(dir, id);
    std::fs::create_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    let total = p.bytes();
    let mut base = 0u64;
    for f in p.files {
        let dest = staging.join(f.path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        // A file an earlier install already finished (and moved) is reused.
        let done_file = target.join(f.path);
        if std::fs::metadata(&done_file).is_ok_and(|m| m.len() == f.bytes) && !dest.exists() {
            std::fs::copy(&done_file, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        }
        fetch(
            f,
            &url_of(f),
            &dest,
            &mut |n| {
                progress(Progress {
                    done: base + n,
                    total,
                })
            },
            cancel,
        )?;
        base += f.bytes;
    }
    // Every file verified: move the package in.
    if target.exists() {
        std::fs::remove_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    std::fs::rename(&staging, &target)
        .map_err(|e| format!("{} → {}: {e}", staging.display(), target.display()))?;
    progress(Progress { done: total, total });
    Ok(target)
}

/// Hashes what is already at `dest` (a previous attempt), fetches the rest
/// with a Range request, and checks the whole file.
#[cfg(feature = "download")]
fn fetch(
    f: &File,
    url: &str,
    dest: &Path,
    progress: &mut dyn FnMut(u64),
    cancel: &dyn Fn() -> bool,
) -> Result<(), String> {
    let mut hasher = Sha256::new();
    let mut have = 0u64;
    if let Ok(mut existing) = std::fs::File::open(dest) {
        let len = existing.metadata().map(|m| m.len()).unwrap_or(0);
        if len > f.bytes {
            drop(existing);
            let _ = std::fs::remove_file(dest);
        } else {
            let mut buf = vec![0u8; 1 << 20];
            loop {
                if cancel() {
                    return Err(CANCELLED.into());
                }
                let n = existing
                    .read(&mut buf)
                    .map_err(|e| format!("{}: {e}", dest.display()))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                have += n as u64;
                progress(have);
            }
        }
    }
    if have < f.bytes {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(30))
            .build();
        let mut req = agent.get(url).set("User-Agent", "PromoShot");
        if have > 0 {
            req = req.set("Range", &format!("bytes={have}-"));
        }
        let resp = req
            .call()
            .map_err(|e| format!("download {}: {e}", f.path))?;
        if have > 0 && resp.status() != 206 {
            // No range support: start over.
            return Err(format!(
                "download {}: the server did not resume (HTTP {}); retry the install",
                f.path,
                resp.status()
            ));
        }
        let mut out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dest)
            .map_err(|e| format!("{}: {e}", dest.display()))?;
        let mut reader = resp.into_reader();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            if cancel() {
                return Err(CANCELLED.into());
            }
            let n = reader
                .read(&mut buf)
                .map_err(|e| format!("download {}: {e}", f.path))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])
                .map_err(|e| format!("{}: {e}", dest.display()))?;
            hasher.update(&buf[..n]);
            have += n as u64;
            if have > f.bytes {
                break;
            }
            progress(have);
        }
        out.flush()
            .map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    let digest = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if have != f.bytes || digest != f.sha256 {
        let _ = std::fs::remove_file(dest);
        return Err(format!(
            "{}: got {have} bytes, sha256 {digest}; expected {} bytes, {} — the download was removed, retry the install",
            f.path, f.bytes, f.sha256
        ));
    }
    Ok(())
}

/// Hashes an installed file (for `promo music verify` and the tests).
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// The `music` door of `promo` and `promoshot-mcp`: `list` (default),
/// `install <id>` (progress on stderr), `remove <id>`.
#[cfg(feature = "download")]
pub fn command(program: &str, args: &[String]) -> Result<String, String> {
    let dir = models_dir();
    let usage =
        format!("usage: {program} music [list | install {ACE_STEP_BASE} | remove {ACE_STEP_BASE}]");
    match (args.first().map(String::as_str), args.get(1)) {
        (None | Some("list"), None) => {
            let mut lines = vec![format!("music models in {}:", dir.display())];
            for p in catalog() {
                lines.push(format!(
                    "  {:<14} {:<2} {:>5.2} GB  {}  {}",
                    p.id,
                    if installed(&dir, p.id) { "✓" } else { "-" },
                    p.bytes() as f64 / 1e9,
                    p.title,
                    p.license
                ));
            }
            Ok(lines.join("\n"))
        }
        (Some("install"), Some(id)) => {
            let mut shown = 0u64;
            let path = install(
                &dir,
                id,
                &mut |p| {
                    let percent = (p.done * 100).checked_div(p.total).unwrap_or(0).min(100);
                    if percent >= shown + 5 {
                        shown = percent;
                        eprintln!("{id}: {percent}% of {:.2} GB", p.total as f64 / 1e9);
                    }
                },
                &|| false,
            )?;
            Ok(format!(
                "{id}: installed and verified at {}",
                path.display()
            ))
        }
        (Some("remove"), Some(id)) => Ok(if remove(&dir, id)? {
            format!("{id}: removed")
        } else {
            format!("{id}: not installed")
        }),
        _ => Err(usage),
    }
}
