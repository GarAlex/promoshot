//! `promo video` as a job that survives being killed.
//!
//! A long render on a CPU box is the job most likely to be interrupted —
//! a redeploy, an OOM kill, a closed SSH session — and it used to start
//! again from frame one. Now the picture is encoded in SEGMENTS (ten
//! seconds of video each, by default) into a work folder beside the output,
//! `<out>.promo-parts/`. Each finished segment is renamed into place only
//! once its encoder has closed it, so a segment that exists is whole. A
//! re-run of the same command skips the segments already there and renders
//! the rest; then the segments are joined WITHOUT re-encoding, the
//! soundtrack is mixed once for the whole range and muxed in at the join
//! (so the sound has no segment edges at all), and the work folder goes.
//!
//! The folder is keyed by a digest of everything that decides the pixels:
//! the project's `metadata.json`, every file in the project (by size and
//! modification time), the render settings, the segment length, this
//! build's version and the GPU adapter. An edited project, other settings
//! or another renderer never reuse a stale segment: the folder is cleared
//! and the render starts over, saying why.

use crate::project::Project;
use crate::render::{
    build_soundtrack_between, ExportOutcome, ExportSettings, ProxyPolicy, Renderer,
};
use promo_media::Registry;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Seconds of video per segment unless told otherwise: about 40 seconds of
/// work lost at worst on a one-core lavapipe box (the 703-frame reel took
/// 179 s there), and few enough files to join.
pub const DEFAULT_SEGMENT_SECONDS: f64 = 10.0;

const JOB_FILE: &str = "job.json";

/// What the resumable render tells its caller as it goes.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Before the first frame: the plan, and what an earlier run left.
    Plan {
        frames: usize,
        segments: usize,
        /// Segments found finished from an earlier run.
        done_segments: usize,
        /// Their frames.
        done_frames: usize,
        /// Why an earlier run's work folder was cleared, when it was.
        restarted: Option<String>,
        work: PathBuf,
    },
    /// One frame rendered: `done` counts the skipped segments' frames too.
    Frame { done: usize, total: usize },
    /// A segment finished and was kept.
    Segment { index: usize, segments: usize },
    /// Every segment is there; joining them with the soundtrack.
    Joining { segments: usize },
}

/// Where the work folder for `out` lives: beside it, named after it.
pub fn work_dir(out: &Path) -> PathBuf {
    let name = out
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "render".into());
    out.with_file_name(format!("{name}.promo-parts"))
}

/// The segments a render of `frames` frames is cut into: `[from, to)`
/// frame ranges of `segment_frames` each, the last one shorter.
pub fn plan_segments(frames: usize, segment_frames: usize) -> Vec<(usize, usize)> {
    let step = segment_frames.max(1);
    (0..frames.max(1))
        .step_by(step)
        .map(|from| (from, (from + step).min(frames.max(1))))
        .collect()
}

fn segment_name(from: usize, to: usize, ext: &str) -> String {
    format!("seg-{from:07}-{to:07}.{ext}")
}

/// Hex SHA-256 of what decides this render's pixels; see the module doc.
pub fn job_digest(
    project: &Project,
    out: &Path,
    settings: &ExportSettings,
    segment_frames: usize,
    proxy: ProxyPolicy,
    adapter: &str,
) -> Result<String, String> {
    let mut hash = Sha256::new();
    let mut line = |text: String| {
        hash.update(text.as_bytes());
        hash.update(b"\n");
    };
    line("promo video resume v1".into());
    line(format!("version {}", env!("CARGO_PKG_VERSION")));
    line(format!("adapter {adapter}"));
    line(format!(
        "settings {}x{} {:?} alpha={} start={:016x} end={:016x} fps={:016x} segment={segment_frames} proxy={proxy:?}",
        settings.width,
        settings.height,
        settings.codec,
        settings.alpha,
        settings.start.to_bits(),
        settings.end.to_bits(),
        settings.fps.to_bits(),
    ));
    match &settings.overlay {
        Some((bgra, w, h)) => {
            let mut overlay = Sha256::new();
            overlay.update(bgra);
            line(format!("overlay {w}x{h} {:x}", overlay.finalize()));
        }
        None => line("overlay none".into()),
    }
    // The document itself by content; everything else by size and time.
    let mut files = Vec::new();
    walk(&project.dir, &project.dir, out, &mut files)
        .map_err(|e| format!("{}: {e}", project.dir.display()))?;
    files.sort();
    for (relative, size, modified) in files {
        if relative == "metadata.json" {
            let bytes = std::fs::read(project.dir.join(&relative))
                .map_err(|e| format!("{relative}: {e}"))?;
            let mut doc = Sha256::new();
            doc.update(&bytes);
            line(format!("doc {:x}", doc.finalize()));
        } else {
            line(format!("file {relative} {size} {modified}"));
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Every file under the project, but never this render's own output or
/// any render's work folder: an `--out` inside the project folder must not
/// change the digest by being written.
fn walk(
    root: &Path,
    dir: &Path,
    out: &Path,
    into: &mut Vec<(String, u64, u128)>,
) -> std::io::Result<()> {
    let out = std::path::absolute(out).unwrap_or_else(|_| out.to_path_buf());
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".promo-parts") || std::path::absolute(&path).is_ok_and(|p| p == out) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            walk(root, &path, &out, into)?;
        } else {
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            into.push((relative, meta.len(), modified));
        }
    }
    Ok(())
}

/// Removes this job's own files from the work folder (never anything
/// else), and the folder when that empties it.
fn clear(work: &Path) {
    let Ok(entries) = std::fs::read_dir(work) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == JOB_FILE || name.starts_with("seg-") || name.starts_with("joined.") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let _ = std::fs::remove_dir(work);
}

/// [`crate::render::export_video`], resumable: see the module doc.
/// `events` returning false stops the render; the finished segments stay
/// for the next run to resume from (Cancelled).
pub fn export_video_resumable(
    project: &Project,
    out: &Path,
    settings: &ExportSettings,
    segment_seconds: f64,
    proxy: ProxyPolicy,
    events: &mut dyn FnMut(Event) -> bool,
) -> Result<ExportOutcome, String> {
    let fps = settings.fps;
    let frames = promo_timeline::frame_count(settings.start, settings.end, fps);
    let segment_frames = ((segment_seconds.max(0.1) * fps).round() as usize).max(1);
    let segments = plan_segments(frames, segment_frames);
    let codec = if settings.alpha {
        promo_media::VideoCodec::ProRes4444
    } else {
        settings.codec
    };
    let ext = match codec {
        promo_media::VideoCodec::H264 => "mp4",
        _ => "mov",
    };

    let adapter = promo_gpu::GpuContext::shared_or_error()?.adapter_summary();
    let digest = job_digest(project, out, settings, segment_frames, proxy, &adapter)?;
    let work = work_dir(out);

    // What an earlier run left: kept when it was this job, cleared when not.
    let mut restarted = None;
    if work.is_dir() {
        let earlier = std::fs::read_to_string(work.join(JOB_FILE))
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|job| job["digest"].as_str().map(str::to_string));
        if earlier.as_deref() != Some(digest.as_str()) {
            restarted = Some(match earlier {
                Some(_) => "the project or the render settings changed since the interrupted \
                            render, so its segments were discarded"
                    .to_string(),
                None => "the work folder had no job record, so it was cleared".to_string(),
            });
            clear(&work);
        }
    }
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    // A segment half-written when the last run died is not one: only
    // renamed segments count, and stray partials go.
    if let Ok(entries) = std::fs::read_dir(&work) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.contains(".partial.") || name.starts_with("joined.") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    let job = serde_json::json!({
        "digest": digest,
        "out": out.display().to_string(),
        "frames": frames,
        "segmentFrames": segment_frames,
        "version": env!("CARGO_PKG_VERSION"),
    });
    std::fs::write(work.join(JOB_FILE), job.to_string())
        .map_err(|e| format!("{}: {e}", work.join(JOB_FILE).display()))?;

    let paths: Vec<PathBuf> = segments
        .iter()
        .map(|(from, to)| work.join(segment_name(*from, *to, ext)))
        .collect();
    let finished: Vec<bool> = paths.iter().map(|p| p.is_file()).collect();
    let done_segments = finished.iter().filter(|f| **f).count();
    let done_frames: usize = segments
        .iter()
        .zip(&finished)
        .filter(|(_, f)| **f)
        .map(|((from, to), _)| to - from)
        .sum();
    if !events(Event::Plan {
        frames,
        segments: segments.len(),
        done_segments,
        done_frames,
        restarted,
        work: work.clone(),
    }) {
        return Ok(ExportOutcome::Cancelled);
    }

    let mut renderer: Option<Renderer> = None;
    let registry = Registry::with_defaults();
    let mut done = done_frames;
    for (index, ((from, to), path)) in segments.iter().zip(&paths).enumerate() {
        if finished[index] {
            continue;
        }
        let renderer = match &mut renderer {
            Some(r) => r,
            None => {
                let mut r = Renderer::with_proxy(project, settings.width, settings.height, proxy)?;
                r.set_transparent_plate(settings.alpha);
                if let Some((bgra, w, h)) = &settings.overlay {
                    r.set_overlay(Some((bgra, *w, *h)))?;
                }
                renderer.insert(r)
            }
        };
        let partial = work.join(format!("seg-{from:07}-{to:07}.partial.{ext}"));
        let spec = promo_media::EncodeSpec {
            chapters: Vec::new(),
            chapters_end: 0.0,
            codec,
            alpha: settings.alpha,
            width: settings.width,
            height: settings.height,
            fps,
            quality: 18,
            audio: None,
        };
        let mut encoder = registry
            .open_encoder(&partial, &spec)
            .map_err(|e| e.to_string())?;
        for i in *from..*to {
            let time = settings.start + i as f64 / fps;
            let mut bgra = renderer.frame_bgra(time)?;
            if settings.alpha {
                crate::render::unpremultiply(&mut bgra);
            }
            encoder.write_frame(&bgra).map_err(|e| e.to_string())?;
            done += 1;
            if !events(Event::Frame {
                done,
                total: frames,
            }) {
                drop(encoder);
                let _ = std::fs::remove_file(&partial);
                return Ok(ExportOutcome::Cancelled);
            }
        }
        encoder.finish().map_err(|e| e.to_string())?;
        std::fs::rename(&partial, path).map_err(|e| format!("{}: {e}", path.display()))?;
        events(Event::Segment {
            index,
            segments: segments.len(),
        });
    }

    events(Event::Joining {
        segments: segments.len(),
    });
    // The soundtrack, once for the whole range: never cut at a segment.
    let audio = build_soundtrack_between(project, settings.start, settings.end)?;
    let chapters: Vec<(f64, String)> = project
        .meta
        .markers
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter(|m| m.kind == promo_model::MarkerKind::Chapter)
        .map(|m| (m.time, m.name.clone()))
        .collect();
    // Joined inside the work folder, then moved over the output: a join
    // killed halfway never leaves a file that looks exported.
    let joined = work.join(format!("joined.{ext}"));
    promo_media::ffmpeg::join_segments(
        &paths,
        &joined,
        audio.as_ref(),
        &chapters,
        (settings.end - settings.start).max(0.0),
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(&joined, out).map_err(|e| format!("{}: {e}", out.display()))?;
    clear(&work);
    Ok(ExportOutcome::Finished)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_cover_every_frame_once() {
        assert_eq!(
            plan_segments(703, 300),
            vec![(0, 300), (300, 600), (600, 703)]
        );
        assert_eq!(plan_segments(300, 300), vec![(0, 300)]);
        assert_eq!(plan_segments(1, 300), vec![(0, 1)]);
        assert_eq!(
            plan_segments(0, 300),
            vec![(0, 1)],
            "a still project is one frame"
        );
    }

    fn ffmpeg_ok() -> bool {
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// A stream's decoded content as ffmpeg's md5, frame by frame for the
    /// picture and as one PCM digest for the sound.
    fn decoded_md5(path: &Path, stream: &str) -> String {
        let format = if stream == "0:v" { "framemd5" } else { "md5" };
        let out = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(path)
            .args(["-map", stream, "-f", format, "-"])
            .output()
            .expect("ffmpeg");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The whole contract on a small moving project with a tone under it:
    /// a render stopped in its second segment keeps the first and no half
    /// segment; the re-run says it resumes, renders only the rest, and
    /// writes a file whose decoded picture and sound equal an unbroken
    /// render's; the work folder is gone after. An edited project, after
    /// another stop, starts over instead of reusing what was left.
    #[test]
    fn a_stopped_render_resumes_to_the_same_file() {
        if !ffmpeg_ok() {
            eprintln!("ffmpeg unavailable; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-resume-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        let tone = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("sine=frequency=330:sample_rate=48000:duration=2")
            .args(["-c:a", "pcm_s16le"])
            .arg(dir.join("Resources/tone.wav"))
            .status()
            .is_ok_and(|s| s.success());
        assert!(tone, "ffmpeg makes the tone");
        let json = r#"{"id":"AAAAAAAA-0000-0000-0000-0000000000RS","name":"resume",
            "createdAt":0,"state":"recorded","trimStart":0,"trimEnd":2,
            "videoDuration":2,"subtitles":[],
            "compositionSettings":{"canvasWidth":96,"canvasHeight":64,
              "backgroundColorHex":"102030","fps":30},
            "resources":[{"id":"D","kind":"drawing","filename":"",
              "displayName":"Ink","addedAt":0,
              "drawing":{"shapes":[{"id":"S","kind":"oval",
                "points":[[0,0],[400,300]],
                "strokeColorHex":"FF0000","strokeWidth":8,
                "fillColorHex":"FF0000",
                "arrowStart":false,"arrowEnd":false}]}},
              {"id":"T","kind":"audio","filename":"tone.wav","displayName":"tone",
               "addedAt":0,"duration":2,"imageCuts":[],"disabledAudioTrackIndices":[]}],
            "layers":[{"id":"L","name":"Ink","sortIndex":0,"kind":"drawing",
              "isEnabled":true,"startTime":0.0,"duration":2.0,"resourceID":"D",
              "keyframes":[{"id":"K","time":0.0,"transitionDuration":0.0,"zoom":0.4,
                "placement":{"mode":"fit","anchor":"center"}},
                {"id":"K2","time":1.9,"transitionDuration":1.9,"zoom":1.0,
                "placement":{"mode":"fit","anchor":"center"}}]},
              {"id":"A","name":"tone","sortIndex":1,"kind":"audio","isEnabled":true,
               "startTime":0,"duration":2,"resourceID":"T","keyframes":[]}]}"#;
        std::fs::write(dir.join("metadata.json"), json).unwrap();
        if promo_gpu::gpu_for_test().is_none() {
            return;
        }
        let project = Project::open(&dir).expect("project");
        let settings = ExportSettings {
            width: 96,
            height: 64,
            codec: promo_media::VideoCodec::H264,
            alpha: false,
            start: 0.0,
            end: 2.0,
            fps: 30.0,
            overlay: None,
        };
        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();
        let whole = out_dir.join("whole.mp4");
        let resumed = out_dir.join("resumed.mp4");
        let run = |out: &Path, stop_after: Option<usize>, project: &Project| {
            let mut plan = None;
            let outcome = export_video_resumable(
                project,
                out,
                &settings,
                0.5,
                ProxyPolicy::Off,
                &mut |event| {
                    if let Event::Plan { .. } = &event {
                        plan = Some(event.clone());
                    }
                    match (&event, stop_after) {
                        (Event::Frame { done, .. }, Some(n)) => *done < n,
                        _ => true,
                    }
                },
            )
            .expect("render");
            (outcome, plan.expect("a plan event"))
        };

        let (outcome, _) = run(&whole, None, &project);
        assert_eq!(outcome, ExportOutcome::Finished);
        assert!(
            !work_dir(&whole).exists(),
            "the work folder goes on success"
        );

        // Stopped at frame 20: segment 0–15 is whole, 15–30 was not.
        let (outcome, _) = run(&resumed, Some(20), &project);
        assert_eq!(outcome, ExportOutcome::Cancelled);
        assert!(!resumed.exists(), "a stopped render writes no output");
        let work = work_dir(&resumed);
        let mut left: Vec<String> = std::fs::read_dir(&work)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, vec!["job.json", "seg-0000000-0000015.mp4"]);

        let (outcome, plan) = run(&resumed, None, &project);
        assert_eq!(outcome, ExportOutcome::Finished);
        match plan {
            Event::Plan {
                frames,
                segments,
                done_segments,
                done_frames,
                restarted,
                ..
            } => {
                assert_eq!((frames, segments), (60, 4));
                assert_eq!((done_segments, done_frames), (1, 15), "it resumes");
                assert_eq!(restarted, None);
            }
            other => panic!("not a plan: {other:?}"),
        }
        assert!(!work.exists(), "the work folder goes on success");
        assert_eq!(
            decoded_md5(&whole, "0:v"),
            decoded_md5(&resumed, "0:v"),
            "the resumed picture is the unbroken render's, frame for frame"
        );
        let sound = decoded_md5(&whole, "0:a");
        assert!(!sound.is_empty(), "the render has its sound");
        assert_eq!(sound, decoded_md5(&resumed, "0:a"), "and the same sound");

        // An edit between the stop and the re-run starts over.
        let edited = out_dir.join("edited.mp4");
        run(&edited, Some(20), &project);
        let mut doc = std::fs::read_to_string(dir.join("metadata.json")).unwrap();
        doc.push(' ');
        std::fs::write(dir.join("metadata.json"), doc).unwrap();
        let project = Project::open(&dir).expect("project");
        let (_, plan) = run(&edited, None, &project);
        match plan {
            Event::Plan {
                done_segments,
                restarted,
                ..
            } => {
                assert_eq!(done_segments, 0, "nothing stale is reused");
                assert!(restarted.is_some_and(|why| why.contains("changed")));
            }
            other => panic!("not a plan: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_work_folder_sits_beside_the_output() {
        assert_eq!(
            work_dir(Path::new("/r/out/reel.mp4")),
            PathBuf::from("/r/out/reel.mp4.promo-parts")
        );
    }
}
