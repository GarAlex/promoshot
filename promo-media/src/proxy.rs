//! Proxies for long sources — the headless twin of the Mac's `ProxyCache`.
//!
//! A proxy is a small, all-intra H.264 copy of a video source: every frame
//! a keyframe, so a far seek is a short read instead of a decode from the
//! last keyframe of an hour-long 4K file. Proxies live OUTSIDE `.promo`
//! packages (a package stays portable) in a cache directory, keyed by the
//! source's content identity — path, size and mtime — and tier, the same
//! rule the app uses, so a source has ONE identity everywhere.

use crate::MediaError;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Tier 1's long edge, in pixels — the app's `ProxyCache.tier1LongEdge`.
pub const TIER1_LONG_EDGE: u32 = 960;

/// Where proxies live: `$PROMO_PROXY_DIR`, else the platform's cache
/// directory under `promoshot/proxies`.
pub fn cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("PROMO_PROXY_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = home {
            return home.join("Library/Caches/promoshot/proxies");
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
            return PathBuf::from(xdg).join("promoshot/proxies");
        }
        if let Some(home) = home {
            return home.join(".cache/promoshot/proxies");
        }
    }
    std::env::temp_dir().join("promoshot/proxies")
}

/// The source's identity: a digest of its absolute path, byte size and
/// modification time. A re-encoded or replaced file is a new identity; a
/// package moved elsewhere is too (the path is part of it — proxies are
/// cheap to rebuild and never wrong).
pub fn key(source: &Path) -> Option<String> {
    let meta = std::fs::metadata(source).ok()?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let absolute = std::fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
    let text = format!(
        "{}\u{1f}{}\u{1f}{}",
        absolute.display(),
        meta.len(),
        modified
    );
    Some(format!("{:016x}", fnv1a(text.as_bytes())))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The proxy's path for `source` at `tier`, whether or not it exists.
pub fn proxy_path(cache_dir: &Path, source: &Path, tier: u32) -> Option<PathBuf> {
    Some(cache_dir.join(format!("{}-p{tier}.mp4", key(source)?)))
}

/// The proxy that is ready to use: present, non-empty, and finished (a
/// proxy is written to a temp name and renamed, so a file at this path is
/// whole).
pub fn available(cache_dir: &Path, source: &Path, tier: u32) -> Option<PathBuf> {
    let path = proxy_path(cache_dir, source, tier)?;
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() && meta.len() > 0 => Some(path),
        _ => None,
    }
}

/// The tier-1 proxy for `source`, built if it is not there yet. Uses the
/// ffmpeg CLI: scaled to `long_edge` on the longer side (even dimensions),
/// every frame a keyframe, no audio (the mix reads the source).
pub fn ensure(cache_dir: &Path, source: &Path, long_edge: u32) -> Result<PathBuf, MediaError> {
    if let Some(ready) = available(cache_dir, source, 1) {
        return Ok(ready);
    }
    let path = proxy_path(cache_dir, source, 1).ok_or_else(|| {
        MediaError::Backend(format!("{}: cannot read the source", source.display()))
    })?;
    std::fs::create_dir_all(cache_dir).map_err(|e| MediaError::Backend(e.to_string()))?;
    let temp = path.with_extension(format!("{}.tmp.mp4", std::process::id()));
    // Fit the longer side to `long_edge`, never upscale, keep dimensions even.
    let filter = format!(
        "scale='if(gt(iw,ih),min({le},iw),-2)':'if(gt(iw,ih),-2,min({le},ih))':flags=bicubic,\
         scale=trunc(iw/2)*2:trunc(ih/2)*2",
        le = long_edge
    );
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-y", "-i"])
        .arg(source)
        .args([
            "-an",
            "-vf",
            &filter,
            "-c:v",
            "libx264",
            "-g",
            "1",
            "-preset",
            "veryfast",
            "-crf",
            "20",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
        ])
        .arg(&temp)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => MediaError::ToolMissing(
                "ffmpeg",
                "not found on PATH — install it to build proxies".into(),
            ),
            _ => MediaError::Backend(e.to_string()),
        })?;
    if !status.status.success() {
        let _ = std::fs::remove_file(&temp);
        return Err(MediaError::Backend(format!(
            "ffmpeg could not build a proxy for {}: {}",
            source.display(),
            String::from_utf8_lossy(&status.stderr).trim()
        )));
    }
    std::fs::rename(&temp, &path).map_err(|e| MediaError::Backend(e.to_string()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VideoDecoder;

    fn ffmpeg_available() -> bool {
        Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    fn synth(dir: &Path, name: &str, size: &str) -> PathBuf {
        let path = dir.join(name);
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-y", "-f", "lavfi", "-i"])
            .arg(format!("testsrc=size={size}:rate=30:duration=1"))
            .args(["-pix_fmt", "yuv420p", "-c:v", "libx264", "-crf", "18"])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "synth clip");
        path
    }

    /// PSNR of two BGRA frames of one size, over RGB.
    fn psnr(a: &[u8], b: &[u8]) -> f64 {
        let mut sum = 0.0f64;
        let mut n = 0usize;
        for (x, y) in a.chunks(4).zip(b.chunks(4)) {
            for c in 0..3 {
                let d = x[c] as f64 - y[c] as f64;
                sum += d * d;
                n += 1;
            }
        }
        let mse = sum / n.max(1) as f64;
        if mse == 0.0 {
            99.0
        } else {
            10.0 * (255.0f64 * 255.0 / mse).log10()
        }
    }

    #[test]
    fn a_proxy_is_keyed_by_content_scaled_to_the_long_edge_and_close_to_the_source() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg unavailable; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-proxy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("cache");
        let source = synth(&dir, "src.mp4", "1280x720");
        assert!(available(&cache, &source, 1).is_none());

        let proxy = ensure(&cache, &source, 640).expect("proxy");
        assert_eq!(
            available(&cache, &source, 1).as_deref(),
            Some(proxy.as_path())
        );
        assert!(proxy
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with("-p1.mp4"));
        let info = crate::ffmpeg::FfmpegDecoder::open(&proxy)
            .expect("opens")
            .info()
            .clone();
        assert_eq!((info.width, info.height), (640, 360), "{info:?}");

        // The proxy's picture is the source's, scaled: compare at the
        // proxy's size through the same decoder.
        let mut full = crate::ffmpeg::FfmpegDecoder::open(&source).expect("source");
        let mut small = crate::ffmpeg::FfmpegDecoder::open(&proxy).expect("proxy");
        let frame_bgra = |surface: crate::GpuSurface| -> (Vec<u8>, u32, u32) {
            match surface {
                crate::GpuSurface::CpuPixels {
                    data,
                    width,
                    height,
                    bytes_per_row,
                } => {
                    let mut out = Vec::with_capacity((width * height * 4) as usize);
                    for row in 0..height as usize {
                        let start = row * bytes_per_row as usize;
                        out.extend_from_slice(&data[start..start + width as usize * 4]);
                    }
                    (out, width, height)
                }
                _ => panic!("cpu pixels expected"),
            }
        };
        let (a, aw, ah) = frame_bgra(full.frame_at(0.5).unwrap().unwrap());
        let (b, bw, bh) = frame_bgra(small.frame_at(0.5).unwrap().unwrap());
        assert_eq!((bw, bh), (640, 360));
        // Downsample the source by 2 (box) to the proxy's size.
        let mut down = vec![0u8; (bw * bh * 4) as usize];
        for y in 0..bh as usize {
            for x in 0..bw as usize {
                for c in 0..4 {
                    let mut acc = 0u32;
                    for dy in 0..2 {
                        for dx in 0..2 {
                            acc += a[((2 * y + dy) * aw as usize + 2 * x + dx) * 4 + c] as u32;
                        }
                    }
                    down[(y * bw as usize + x) * 4 + c] = (acc / 4) as u8;
                }
            }
        }
        let _ = ah;
        let quality = psnr(&down, &b);
        assert!(quality >= 30.0, "proxy vs source PSNR {quality:.1} dB");

        // Same content, same key; a touched file is a new identity.
        let k1 = key(&source).unwrap();
        assert_eq!(key(&source).unwrap(), k1);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let bytes = std::fs::read(&source).unwrap();
        std::fs::write(&source, &bytes).unwrap();
        assert_ne!(key(&source).unwrap(), k1, "mtime moved, so did the key");

        // A half-written proxy is not a proxy.
        let half = proxy_path(&cache, &source, 1).unwrap();
        std::fs::write(&half, b"").unwrap();
        assert!(available(&cache, &source, 1).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_cache_dir_honours_the_override() {
        std::env::set_var("PROMO_PROXY_DIR", "/tmp/promo-proxies-test");
        assert_eq!(cache_dir(), PathBuf::from("/tmp/promo-proxies-test"));
        std::env::remove_var("PROMO_PROXY_DIR");
        assert!(cache_dir().ends_with("promoshot/proxies"));
    }
}
