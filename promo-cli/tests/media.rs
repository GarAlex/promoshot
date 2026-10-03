//! `promo media activity` through the built binary, on a real encode: the
//! synthetic scenes in promo-media's activity tests never meet a codec,
//! and this is where H.264, ffmpeg's resampling and the pipe all do.

use std::process::Command;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// A gray screen with a bar of "text"; a button goes dark from 1.5 s to
/// 1.9 s, and at 3 s the whole screen changes. One event at the button,
/// then; one cut at 3.
#[test]
fn a_real_encode_measures_its_button_and_its_cut() {
    if !ffmpeg_available() {
        eprintln!("skipped: ffmpeg is not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("promo-media-activity-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let video = dir.join("recording.mp4");
    let filters = "drawbox=x=40:y=40:w=560:h=40:color=0x505050:t=fill,\
                   drawbox=x=200:y=160:w=64:h=40:color=black:t=fill:enable='between(t,1.5,1.9)',\
                   drawbox=x=0:y=0:w=640:h=400:color=0x202020:t=fill:enable='gte(t,3)'";
    let status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=0xE0E0E0:s=640x400:r=30:d=4",
            "-vf",
            filters,
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&video)
        .status()
        .expect("ffmpeg runs");
    assert!(status.success(), "ffmpeg could not encode the fixture");

    let out = Command::new(env!("CARGO_BIN_EXE_promo"))
        .args(["media", "activity", video.to_str().unwrap(), "--rate", "10"])
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let track: promo_model::ActivityTrack = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(track.meter, promo_media::activity::METER_VERSION);
    assert_eq!(track.rate, 10.0);
    assert_eq!(track.events.len(), 1, "{track:?}");
    let e = track.events[0];
    assert!(
        (e[0] - 1.5).abs() <= 0.1 + 1e-9,
        "starts at the press: {e:?}"
    );
    assert!(
        (e[1] - 1.9).abs() <= 0.15 + 1e-9,
        "ends at the release: {e:?}"
    );
    let (bx, by, bw, bh) = (200.0 / 640.0, 160.0 / 400.0, 64.0 / 640.0, 40.0 / 400.0);
    assert!(
        e[2] <= bx && e[3] <= by && e[2] + e[4] >= bx + bw && e[3] + e[5] >= by + bh,
        "covers the button: {e:?}"
    );
    assert!(e[4] < 0.2 && e[5] < 0.25, "and not much more: {e:?}");
    assert_eq!(track.cuts.len(), 1, "{track:?}");
    assert!((track.cuts[0] - 3.0).abs() <= 0.1 + 1e-9, "{track:?}");
    assert!(
        track.scrolls.is_empty() && track.ambient.is_empty(),
        "{track:?}"
    );

    let bad = Command::new(env!("CARGO_BIN_EXE_promo"))
        .args(["media", "activity", video.to_str().unwrap(), "--rate", "0"])
        .output()
        .expect("run");
    assert!(!bad.status.success());
    let _ = std::fs::remove_dir_all(&dir);
}
