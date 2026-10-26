//! The model store: the pins against the reference checkpoints (when
//! `PROMOSHOT_ACE_CKPT` names them), and the install over a local server —
//! verify, resume, a bad hash, cancel.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use promo_music::store::{self, File, Package};

#[test]
fn pins_match_the_reference_checkpoints() {
    let Some(ckpt) = std::env::var_os("PROMOSHOT_ACE_CKPT").map(PathBuf::from) else {
        return;
    };
    let p = store::package(store::ACE_STEP_BASE).unwrap();
    for f in p.files {
        let path = ckpt.join(f.path);
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            f.bytes,
            "{}",
            f.path
        );
        assert_eq!(store::sha256_file(&path).unwrap(), f.sha256, "{}", f.path);
    }
    eprintln!("{} files, {} bytes", p.files.len(), p.bytes());
}

#[test]
fn urls_are_pinned_commits() {
    let p = store::package(store::ACE_STEP_BASE).unwrap();
    for f in p.files {
        let u = store::url(f);
        assert!(u.starts_with("https://huggingface.co/ACE-Step/"), "{u}");
        assert!(u.contains("/resolve/") && !u.contains("/main/"), "{u}");
    }
    assert!(store::url(&p.files[0]).contains("acestep-v15-base/resolve/e432212f"));
    assert!(store::url(&p.files[2]).ends_with("Ace-Step1.5/resolve/19671f406d603126926c1b7e2adc169acbcade22/vae/diffusion_pytorch_model.safetensors?download=true"));
}

/// Serves `body` for any path, honouring `Range: bytes=N-`; counts requests.
fn serve(body: Vec<u8>) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            h.fetch_add(1, Ordering::SeqCst);
            let mut reader = BufReader::new(s.try_clone().unwrap());
            let mut from = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(r) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                    from = r.trim().trim_end_matches('-').parse().unwrap_or(0);
                }
            }
            let part = &body[from.min(body.len())..];
            let status = if from > 0 {
                "206 Partial Content"
            } else {
                "200 OK"
            };
            let _ = write!(
                s,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                part.len()
            );
            let _ = s.write_all(part);
        }
    });
    (format!("http://{addr}"), hits)
}

fn sha(b: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

#[test]
fn install_verifies_resumes_and_refuses_a_bad_hash() {
    let body: Vec<u8> = (0..3_000_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let (base, _hits) = serve(body.clone());
    let tmp = std::env::temp_dir().join(format!("promo-music-store-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let files: &'static [File] = Box::leak(Box::new([File {
        path: "sub/weights.bin",
        bytes: body.len() as u64,
        sha256: leak(sha(&body)),
    }]));
    let p = Package {
        id: "test-pkg",
        title: "t",
        license: "t",
        source: "t",
        files,
    };
    let url = |_: &File| format!("{base}/w");

    // A partial file from an earlier attempt is resumed, not refetched.
    let partial = tmp.join(".partial/test-pkg/sub/weights.bin");
    std::fs::create_dir_all(partial.parent().unwrap()).unwrap();
    std::fs::File::create(&partial)
        .unwrap()
        .write_all(&body[..1_000_000])
        .unwrap();
    let mut last = store::Progress::default();
    let out = store::install_from(&tmp, &p, &url, &mut |pr| last = pr, &|| false).unwrap();
    let mut got = Vec::new();
    std::fs::File::open(out.join("sub/weights.bin"))
        .unwrap()
        .read_to_end(&mut got)
        .unwrap();
    assert_eq!(got, body);
    assert_eq!(
        (last.done, last.total),
        (body.len() as u64, body.len() as u64)
    );
    assert!(!tmp.join(".partial/test-pkg").exists(), "staging moved in");

    // A wrong pin: refused, nothing installed, the bad download removed.
    let bad: &'static [File] = Box::leak(Box::new([File {
        path: "w.bin",
        bytes: body.len() as u64,
        sha256: "00",
    }]));
    let q = Package {
        id: "bad-pkg",
        files: bad,
        ..p
    };
    let e = store::install_from(&tmp, &q, &url, &mut |_| {}, &|| false).unwrap_err();
    assert!(e.contains("sha256"), "{e}");
    assert!(!tmp.join("bad-pkg").exists() && !tmp.join(".partial/bad-pkg/w.bin").exists());

    // Cancel.
    let r = Package {
        id: "cancel-pkg",
        ..p
    };
    let e = store::install_from(&tmp, &r, &url, &mut |_| {}, &|| true).unwrap_err();
    assert_eq!(e, store::CANCELLED);
    assert!(!tmp.join("cancel-pkg").exists());
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn not_installed_without_the_files() {
    let tmp = std::env::temp_dir().join(format!("promo-music-empty-{}", std::process::id()));
    assert!(!store::installed(&tmp, store::ACE_STEP_BASE));
    assert!(store::remove(&tmp, "../x").is_err());
    assert!(!store::remove(&tmp, store::ACE_STEP_BASE).unwrap());
}
