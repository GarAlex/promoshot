//! Senses for the agent: what a source file IS, before anything composes it.
//!
//! `promo_media_probe` answers the facts (streams, sizes, fps, duration),
//! `promo_media_filmstrip` gives eyes (a tiled contact sheet of evenly
//! spaced frames, with the times each cell samples),
//! `promo_media_silences` gives ears (where the sound isn't, which is where
//! cuts want to land), and `promo_media_scenes` finds the cuts the picture
//! itself makes. All of them ride the same ffmpeg/ffprobe the render
//! pipeline already requires — no new dependency, and nothing here touches
//! a project: these inspect INPUT media so authoring decisions can be made
//! about it. The distillation parsers are pure functions, tested against
//! canned tool output.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

fn required_file(args: &Value) -> Result<PathBuf, String> {
    let path = args
        .get("file")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("`file` is required")?;
    let path = PathBuf::from(path);
    if !path.exists() {
        return Err(format!("file {} does not exist", path.display()));
    }
    Ok(path)
}

/// The facts, distilled: ffprobe's firehose reduced to what an authoring
/// decision reads — container duration, each stream's kind, codec, size,
/// fps, channels. Returned as JSON, machine-first.
/// One file, or several.
///
/// Several because that is what the corpus asked for: fifteen denied
/// `sips -g pixelWidth -g pixelHeight resources/*.png` calls across the
/// demo runs, every one of them naming MORE THAN ONE file. An agent with
/// six sprite sheets to measure was choosing one shell line over six tool
/// calls, and getting refused by the harness. The answer is keyed by the
/// path asked for, so a caller can match them up.
pub fn probe_many(args: &Value) -> Result<String, String> {
    let Some(list) = args.get("files").and_then(Value::as_array) else {
        return probe(args);
    };
    let paths: Vec<&str> = list.iter().filter_map(Value::as_str).collect();
    if paths.is_empty() {
        return Err("`files` is empty — pass one or more paths, or use `file`".into());
    }
    let mut out = serde_json::Map::new();
    for path in paths {
        let one = json!({ "file": path });
        let answer = match probe(&one) {
            Ok(text) => serde_json::from_str::<Value>(&text).unwrap_or(Value::Null),
            Err(why) => json!({ "error": why }),
        };
        out.insert(path.to_string(), answer);
    }
    serde_json::to_string_pretty(&Value::Object(out)).map_err(|e| e.to_string())
}

pub fn probe(args: &Value) -> Result<String, String> {
    let path = required_file(args)?;
    let out = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_format",
            "-show_streams",
            "-of",
            "json",
        ])
        .arg(&path)
        .output()
        .map_err(|e| format!("ffprobe: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ffprobe could not read {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let raw: Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let mut distilled = distill_probe(&raw);
    if let Some(resource) = resource_entry(&path, &distilled) {
        distilled["resource"] = resource;
    }
    serde_json::to_string_pretty(&distilled).map_err(|e| e.to_string())
}

/// The resource entry this file becomes, ready to paste into `resources`.
///
/// The facts alone left the agent to assemble it, and the one it forgets
/// is the one that matters: without `pixelWidth`/`pixelHeight` a
/// `placement` rule resolves against a SQUARE and the layer lands wrong —
/// the skill's own "measure what you place". Built here and then PARSED
/// as a `ProjectResource` before it is handed over, so what comes back
/// cannot be a shape the format would reject.
fn resource_entry(path: &Path, facts: &Value) -> Option<Value> {
    let filename = path.file_name()?.to_str()?.to_string();
    let stem = path.file_stem()?.to_str()?.to_string();
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let video = facts["streams"]
        .as_array()
        .is_some_and(|s| s.iter().any(|x| x["type"] == "video"));
    let audio = facts["streams"]
        .as_array()
        .is_some_and(|s| s.iter().any(|x| x["type"] == "audio"));
    let duration = facts["duration"].as_f64();
    let (width, height) = facts["streams"]
        .as_array()
        .and_then(|s| s.iter().find(|x| x["type"] == "video"))
        .map(|s| (s["width"].as_f64(), s["height"].as_f64()))
        .unwrap_or((None, None));
    // A moving picture has a duration; a still has pixels and no clock.
    let kind = match (video, audio, duration) {
        (true, _, Some(d)) if d > 0.0 => "video",
        (true, _, _) => "image",
        (false, true, _) => "audio",
        _ => return None,
    };
    let mut entry = json!({
        "id": uuid::Uuid::new_v4().to_string().to_uppercase(),
        "kind": kind,
        "filename": filename,
        "displayName": pretty_name(&stem),
        "addedAt": 0,
        "imageCuts": [],
        "disabledAudioTrackIndices": [],
    });
    match kind {
        "image" => {
            entry["pixelWidth"] = json!(width?);
            entry["pixelHeight"] = json!(height?);
        }
        "video" => {
            entry["duration"] = json!(duration?);
            entry["trimStart"] = json!(0.0);
            entry["trimEnd"] = json!(duration?);
            if let (Some(w), Some(h)) = (width, height) {
                entry["videoNaturalWidth"] = json!(w);
                entry["videoNaturalHeight"] = json!(h);
            }
        }
        _ => {
            if let Some(d) = duration {
                entry["duration"] = json!(d);
            }
        }
    }
    let _ = extension;
    // Through the format's own parser before it is offered: a block that
    // would not decode is worse than no block.
    let parsed: promo_model::ProjectResource = serde_json::from_value(entry).ok()?;
    serde_json::to_value(parsed).ok()
}

/// "rec_lumen_2560" -> "Rec Lumen 2560": a name a person reads in the
/// layer list, from the one the file happens to carry.
fn pretty_name(stem: &str) -> String {
    let words: Vec<String> = stem
        .split(['_', '-', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect();
    if words.is_empty() {
        stem.to_string()
    } else {
        words.join(" ")
    }
}

/// Pure: ffprobe's JSON in, the summary out.
pub fn distill_probe(raw: &Value) -> Value {
    let duration = raw
        .pointer("/format/duration")
        .and_then(Value::as_str)
        .and_then(|d| d.parse::<f64>().ok());
    let container = raw
        .pointer("/format/format_name")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut streams = Vec::new();
    for stream in raw
        .pointer("/streams")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let kind = stream
            .get("codec_type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let codec = stream.get("codec_name").and_then(Value::as_str);
        let mut entry = json!({ "type": kind, "codec": codec });
        match kind {
            "video" => {
                entry["width"] = stream.get("width").cloned().unwrap_or(Value::Null);
                entry["height"] = stream.get("height").cloned().unwrap_or(Value::Null);
                // A STILL has no frame rate. ffprobe answers 25/1 for a PNG
                // or JPEG pipe — its own default for a single-image demuxer,
                // not a fact about the file — and an agent that reads it
                // sets a project's fps from a screenshot. The app's probe
                // reports none for a still; this one now agrees.
                let still = matches!(
                    stream.get("codec_name").and_then(Value::as_str),
                    Some("png" | "mjpeg" | "jpeg" | "bmp" | "gif" | "webp" | "tiff")
                ) || container.ends_with("_pipe");
                if !still {
                    if let Some(fps) = stream
                        .get("r_frame_rate")
                        .and_then(Value::as_str)
                        .and_then(parse_fraction)
                    {
                        entry["fps"] = json!((fps * 100.0).round() / 100.0);
                    }
                }
                // A quarter-turned capture STORES landscape and displays
                // portrait; the display matrix is where that truth lives,
                // and the conformance suite exists because ignoring it
                // scrambled exactly such a clip.
                if let Some(rotation) = stream
                    .pointer("/side_data_list")
                    .and_then(Value::as_array)
                    .and_then(|list| {
                        list.iter()
                            .find_map(|side| side.get("rotation").and_then(Value::as_f64))
                    })
                {
                    entry["rotation"] = json!(rotation);
                }
            }
            "audio" => {
                entry["channels"] = stream.get("channels").cloned().unwrap_or(Value::Null);
                entry["sampleRate"] = stream
                    .get("sample_rate")
                    .and_then(Value::as_str)
                    .and_then(|r| r.parse::<i64>().ok())
                    .map(|r| json!(r))
                    .unwrap_or(Value::Null);
            }
            _ => {}
        }
        streams.push(entry);
    }
    json!({ "container": container, "duration": duration, "streams": streams })
}

fn parse_fraction(text: &str) -> Option<f64> {
    match text.split_once('/') {
        Some((numerator, denominator)) => {
            let n: f64 = numerator.parse().ok()?;
            let d: f64 = denominator.parse().ok()?;
            (d != 0.0).then_some(n / d)
        }
        None => text.parse().ok(),
    }
}

/// Eyes on the footage: `count` evenly spaced frames tiled into one PNG,
/// written into the workspace (or `out`), the sampled times returned so a
/// cell maps back to a moment. The way an agent answers "what is IN this
/// clip" without decoding video itself.
pub fn filmstrip(args: &Value, workspace: &Path) -> Result<String, String> {
    let path = required_file(args)?;
    let count = args
        .get("count")
        .and_then(Value::as_u64)
        .unwrap_or(12)
        .clamp(2, 48) as usize;
    let duration = probe_duration(&path)?;
    let (columns, rows) = grid_for(count);
    let out_path = match args.get("out").and_then(Value::as_str) {
        Some(out) => PathBuf::from(out),
        None => {
            std::fs::create_dir_all(workspace).map_err(|e| e.to_string())?;
            workspace.join(format!(
                "{}-filmstrip.png",
                path.file_stem().unwrap_or_default().to_string_lossy()
            ))
        }
    };
    let status = std::process::Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-i"])
        .arg(&path)
        .args([
            "-vf",
            &format!(
                "fps={count}/{duration},scale=320:-2,tile={columns}x{rows}",
                duration = duration.max(0.01)
            ),
            "-frames:v",
            "1",
        ])
        .arg(&out_path)
        .status()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    if !status.success() {
        return Err(format!("ffmpeg could not filmstrip {}", path.display()));
    }
    let times = sample_times(duration, count);
    Ok(format!(
        "wrote {} — {count} frames, {columns} per row, sampled near [{}] seconds",
        out_path.display(),
        times
            .iter()
            .map(|t| format!("{t:.1}"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// Pure: the grid a count tiles into — as square as it gets, row-major.
pub fn grid_for(count: usize) -> (usize, usize) {
    let columns = match count {
        0..=3 => count.max(1),
        4..=8 => 4,
        _ => 6,
    };
    (columns, count.div_ceil(columns))
}

/// Pure: where each cell samples, matching ffmpeg's `fps=count/duration`
/// cadence (one frame per step, from the start).
pub fn sample_times(duration: f64, count: usize) -> Vec<f64> {
    let step = duration / count as f64;
    (0..count).map(|i| i as f64 * step).collect()
}

/// Ears on the footage: where the sound ISN'T. ffmpeg's silencedetect,
/// distilled to silence spans plus their inverse — the speech spans an
/// edit actually wants — with the thresholds echoed so the answer says
/// what question it answered.
pub fn silences(args: &Value) -> Result<String, String> {
    let path = required_file(args)?;
    let threshold_db = args
        .get("thresholdDb")
        .and_then(Value::as_f64)
        .unwrap_or(-35.0);
    let min_seconds = args
        .get("minSeconds")
        .and_then(Value::as_f64)
        .unwrap_or(0.35);
    let duration = probe_duration(&path)?;
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "info", "-i"])
        .arg(&path)
        .args([
            "-af",
            &format!("silencedetect=noise={threshold_db}dB:d={min_seconds}"),
            "-f",
            "null",
            "-",
        ])
        .output()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    let log = String::from_utf8_lossy(&out.stderr);
    let value = distill_silences(&log, duration, threshold_db, min_seconds);
    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
}

/// Pure: silencedetect's log lines in, spans and their inverse out.
pub fn distill_silences(log: &str, duration: f64, threshold_db: f64, min_seconds: f64) -> Value {
    let mut silences: Vec<(f64, f64)> = Vec::new();
    let mut open: Option<f64> = None;
    for line in log.lines() {
        if let Some(rest) = line.split("silence_start: ").nth(1) {
            open = rest.trim().parse().ok();
        } else if let Some(rest) = line.split("silence_end: ").nth(1) {
            let end: Option<f64> = rest.split('|').next().and_then(|t| t.trim().parse().ok());
            if let (Some(start), Some(end)) = (open.take(), end) {
                silences.push((start, end));
            }
        }
    }
    // A silence still open at the log's end runs to the end of the file.
    if let Some(start) = open {
        silences.push((start, duration));
    }
    let mut speech: Vec<(f64, f64)> = Vec::new();
    let mut cursor = 0.0;
    for &(start, end) in &silences {
        if start > cursor + 0.01 {
            speech.push((cursor, start));
        }
        cursor = cursor.max(end);
    }
    if duration > cursor + 0.01 {
        speech.push((cursor, duration));
    }
    let spans = |list: &[(f64, f64)]| {
        list.iter()
            .map(|(s, e)| json!({ "start": s, "end": e }))
            .collect::<Vec<_>>()
    };
    json!({
        "duration": duration,
        "thresholdDb": threshold_db,
        "minSeconds": min_seconds,
        "silences": spans(&silences),
        "sound": spans(&speech),
    })
}

/// Eyes for CUTS (issue #3): ffmpeg's per-frame scene-change score,
/// distilled to cut times and the SHOTS between them — the footage-first
/// answer when a clip has no silence gaps to cut on. The score is
/// ffmpeg's scene score — min(mafd, |mafd - previous mafd|)/100 over the
/// luma plane, clamped 0..1 — so sustained motion is suppressed and 0.4
/// catches hard cuts.
pub fn scenes(args: &Value) -> Result<String, String> {
    let path = required_file(args)?;
    let threshold = args.get("threshold").and_then(Value::as_f64).unwrap_or(0.4);
    let duration = probe_duration(&path)?;
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&path)
        .args([
            "-vf",
            &format!("select='gt(scene,{threshold})',metadata=print:file=-"),
            "-an",
            "-f",
            "null",
            "-",
        ])
        .output()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    let log = String::from_utf8_lossy(&out.stdout);
    let value = distill_scenes(&log, duration, threshold);
    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
}

/// Pure: the metadata-print log in, cut times and shot spans out — the
/// same span shape `silences` answers in, so an edit can treat either as
/// its cut list.
pub fn distill_scenes(log: &str, duration: f64, threshold: f64) -> Value {
    let mut cuts: Vec<f64> = Vec::new();
    let mut pending: Option<f64> = None;
    for line in log.lines() {
        if let Some(rest) = line.split("pts_time:").nth(1) {
            pending = rest.split_whitespace().next().and_then(|t| t.parse().ok());
        } else if line.contains("lavfi.scene_score=") {
            if let Some(t) = pending.take() {
                cuts.push(t);
            }
        }
    }
    let mut shots: Vec<(f64, f64)> = Vec::new();
    let mut cursor = 0.0;
    for &cut in &cuts {
        if cut > cursor + 0.01 {
            shots.push((cursor, cut));
        }
        cursor = cursor.max(cut);
    }
    if duration > cursor + 0.01 {
        shots.push((cursor, duration));
    }
    json!({
        "duration": duration,
        "threshold": threshold,
        "cuts": cuts,
        "shots": shots
            .iter()
            .map(|(s, e)| json!({ "start": s, "end": e }))
            .collect::<Vec<_>>(),
    })
}

/// Ears for WORDS (REVIEW A3): a transcript with timings, so captions can
/// be drafted from what was said. Headless this needs whisper.cpp's
/// `whisper-cli` on PATH and `WHISPER_MODEL` pointing at a ggml model —
/// and without them an agent CANNOT transcribe; the refusal says so
/// rather than pretending (the Mac app transcribes with Apple's speech
/// recognizer instead). The audio is extracted with the ffmpeg the
/// pipeline already requires: 16 kHz mono wav, which is what whisper wants.
pub fn transcribe(args: &Value) -> Result<String, String> {
    let path = required_file(args)?;
    let model = std::env::var("WHISPER_MODEL").map_err(|_| {
        "no transcriber configured: install whisper.cpp's `whisper-cli` on PATH and \
         set WHISPER_MODEL to a ggml model file. Without one an agent cannot \
         transcribe headless — the PromoShot app transcribes with Apple's speech \
         recognizer, or drop caption text in by hand."
            .to_string()
    })?;
    let scratch = std::env::temp_dir().join(format!("promo-transcribe-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
    let wav = scratch.join("audio.wav");
    let status = std::process::Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-i"])
        .arg(&path)
        .args(["-ar", "16000", "-ac", "1", "-f", "wav"])
        .arg(&wav)
        .status()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    if !status.success() {
        return Err(format!(
            "ffmpeg could not extract audio from {}",
            path.display()
        ));
    }
    let base = scratch.join("transcript");
    let run = std::process::Command::new("whisper-cli")
        .args(["-m", &model, "-f"])
        .arg(&wav)
        .args(["-oj", "-of"])
        .arg(&base)
        .output()
        .map_err(|e| {
            format!("whisper-cli: {e} — install whisper.cpp and put whisper-cli on PATH")
        })?;
    if !run.status.success() {
        return Err(format!(
            "whisper-cli failed: {}",
            String::from_utf8_lossy(&run.stderr).trim()
        ));
    }
    let raw: Value = serde_json::from_slice(
        &std::fs::read(base.with_extension("json")).map_err(|e| format!("transcript: {e}"))?,
    )
    .map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&scratch);
    serde_json::to_string_pretty(&distill_transcript(&raw)).map_err(|e| e.to_string())
}

/// Pure: whisper.cpp's JSON (`transcription[]` with millisecond offsets)
/// in, cues out — start, end, text — the shape a caption draft wants.
pub fn distill_transcript(raw: &Value) -> Value {
    let cues: Vec<Value> = raw
        .get("transcription")
        .and_then(Value::as_array)
        .map(|segments| {
            segments
                .iter()
                .filter_map(|seg| {
                    let text = seg.get("text").and_then(Value::as_str)?.trim();
                    if text.is_empty() {
                        return None;
                    }
                    let ms = |key: &str| {
                        seg.pointer(&format!("/offsets/{key}"))
                            .and_then(Value::as_f64)
                            .map(|v| v / 1000.0)
                    };
                    Some(json!({ "start": ms("from"), "end": ms("to"), "text": text }))
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "language": raw.pointer("/result/language").cloned().unwrap_or(Value::Null),
        "cues": cues,
    })
}

/// The server's answer to promo-author's probe seam: ffprobe, best
/// effort — absent facts stay absent and the document logic degrades the
/// way the format does.
pub fn host_probe(path: &Path, _video: bool) -> promo_author::MediaInfo {
    promo_author::MediaInfo {
        duration: probe_duration(path).ok(),
        pixels: probe_pixels(path).ok(),
    }
}

fn probe_pixels(path: &Path) -> Result<(f64, f64), String> {
    let out = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text.trim().split(',');
    let width: f64 = parts
        .next()
        .and_then(|w| w.parse().ok())
        .ok_or("no width")?;
    let height: f64 = parts
        .next()
        .and_then(|h| h.parse().ok())
        .ok_or("no height")?;
    Ok((width, height))
}

fn probe_duration(path: &Path) -> Result<f64, String> {
    let out = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .map_err(|e| format!("ffprobe: {e}"))?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .map_err(|_| {
            format!(
                "ffprobe read no duration from {} — a filmstrip wants a video",
                path.display()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe distiller, on canned ffprobe JSON — a rotated portrait
    /// capture with sound, reduced to the facts an authoring decision reads.
    #[test]
    fn the_probe_distills_to_authoring_facts() {
        let raw = serde_json::json!({
            "format": { "format_name": "mov,mp4,m4a", "duration": "12.480000" },
            "streams": [
                { "codec_type": "video", "codec_name": "h264",
                  "width": 1920, "height": 1080, "r_frame_rate": "30000/1001",
                  "side_data_list": [ { "rotation": -90 } ] },
                { "codec_type": "audio", "codec_name": "aac",
                  "channels": 2, "sample_rate": "48000" }
            ]
        });
        let facts = distill_probe(&raw);
        assert_eq!(facts["duration"], 12.48);
        assert_eq!(facts["streams"][0]["fps"], 29.97);
        assert_eq!(facts["streams"][0]["rotation"], -90.0);
        assert_eq!(facts["streams"][1]["channels"], 2);
        assert_eq!(facts["streams"][1]["sampleRate"], 48000);
    }

    /// The entry a file becomes, ready to paste — and PARSED as a
    /// `ProjectResource` on the way out, so it cannot be a shape the
    /// format rejects. `pixelWidth`/`pixelHeight` are the point: without
    /// them a `placement` rule resolves against a square and the layer
    /// lands wrong, and assembling the block by hand is where they go
    /// missing.
    #[test]
    fn a_probe_answers_with_the_resource_the_file_becomes() {
        let still = serde_json::json!({
            "format": { "format_name": "png_pipe", "duration": null },
            "streams": [{ "codec_type": "video", "codec_name": "png",
                          "width": 2064, "height": 2752, "r_frame_rate": "25/1" }]
        });
        let facts = distill_probe(&still);
        let r = resource_entry(Path::new("/tmp/hero_shot.png"), &facts).expect("an entry");
        assert_eq!(r["kind"], "image");
        assert_eq!(r["filename"], "hero_shot.png");
        assert_eq!(r["displayName"], "Hero Shot", "a name a person reads");
        assert_eq!(r["pixelWidth"], 2064.0);
        assert_eq!(r["pixelHeight"], 2752.0);
        assert!(
            r["id"].as_str().is_some_and(|id| id.len() == 36),
            "a UUID: {r}"
        );

        let clip = serde_json::json!({
            "format": { "format_name": "mov,mp4", "duration": "6.5" },
            "streams": [
                { "codec_type": "video", "codec_name": "h264", "width": 1920,
                  "height": 1080, "r_frame_rate": "30/1" },
                { "codec_type": "audio", "codec_name": "aac", "channels": 2 }
            ]
        });
        let v = resource_entry(Path::new("/tmp/clip.mp4"), &distill_probe(&clip)).expect("video");
        assert_eq!(v["kind"], "video");
        assert_eq!(v["duration"], 6.5);
        assert_eq!(v["trimEnd"], 6.5, "trimmed to its whole length to start");
        assert_eq!(v["videoNaturalWidth"], 1920.0);

        // Sound alone is a resource too, and carries no pixels.
        let song = serde_json::json!({
            "format": { "format_name": "mp3", "duration": "12.0" },
            "streams": [{ "codec_type": "audio", "codec_name": "mp3", "channels": 2 }]
        });
        let a = resource_entry(Path::new("/tmp/bed.mp3"), &distill_probe(&song)).expect("audio");
        assert_eq!(a["kind"], "audio");
        assert_eq!(a["duration"], 12.0);
        assert!(a.get("pixelWidth").is_none());
    }

    /// Several files in one call. Fifteen denied `sips -g pixelWidth`
    /// calls across the demo runs named MORE THAN ONE file every time: an
    /// agent with six sprite sheets was choosing one shell line over six
    /// tool calls. One bad path does not sink the rest of the batch.
    #[test]
    fn a_probe_takes_several_files_and_keys_the_answer_by_path() {
        let answer = probe_many(&serde_json::json!({
            "files": ["nowhere/a.png", "nowhere/b.png"]
        }))
        .expect("a batch answers even when every file is missing");
        let out: Value = serde_json::from_str(&answer).unwrap();
        assert_eq!(out.as_object().unwrap().len(), 2);
        for path in ["nowhere/a.png", "nowhere/b.png"] {
            assert!(
                out[path]["error"]
                    .as_str()
                    .is_some_and(|e| e.contains("does not exist")),
                "{out}"
            );
        }
        assert!(
            probe_many(&serde_json::json!({ "files": [] })).is_err(),
            "an empty list says so"
        );
    }

    /// A still has no frame rate. ffprobe answers 25/1 for a PNG pipe — its
    /// own default for a single-image demuxer — and an agent reading it set
    /// a project's fps from a screenshot. The app's probe reports none.
    #[test]
    fn a_still_reports_no_frame_rate() {
        let raw = serde_json::json!({
            "format": { "format_name": "png_pipe", "duration": null },
            "streams": [{
                "codec_type": "video", "codec_name": "png",
                "width": 2064, "height": 2752, "r_frame_rate": "25/1"
            }]
        });
        let facts = distill_probe(&raw);
        assert_eq!(facts["streams"][0]["width"], 2064);
        assert_eq!(facts["streams"][0]["fps"], Value::Null, "{facts}");

        // A real clip keeps its rate.
        let clip = serde_json::json!({
            "format": { "format_name": "mov,mp4,m4a", "duration": "6.0" },
            "streams": [{
                "codec_type": "video", "codec_name": "h264",
                "width": 1920, "height": 1080, "r_frame_rate": "30000/1001"
            }]
        });
        assert_eq!(distill_probe(&clip)["streams"][0]["fps"], 29.97);
    }

    /// The silence distiller: spans parsed, an unterminated silence runs to
    /// the end, and the INVERSE — the sound — is what the edit reads.
    /// The scenes distiller on canned metadata-print output: two hard cuts
    /// become three shots, spans in the same shape silences answers in.
    #[test]
    fn scene_cuts_and_their_shots_come_out_of_the_log() {
        let log = "frame:0    pts:2562  pts_time:5.004\n\
                   lavfi.scene_score=0.523\n\
                   frame:1    pts:5124  pts_time:10.008\n\
                   lavfi.scene_score=0.671\n";
        let value = distill_scenes(log, 15.0, 0.4);
        assert_eq!(value["cuts"], serde_json::json!([5.004, 10.008]));
        let shots = value["shots"].as_array().unwrap();
        assert_eq!(shots.len(), 3, "{value}");
        assert_eq!(shots[0], serde_json::json!({"start": 0.0, "end": 5.004}));
        assert_eq!(shots[2], serde_json::json!({"start": 10.008, "end": 15.0}));
        let quiet = distill_scenes("", 8.0, 0.4);
        assert_eq!(quiet["cuts"], serde_json::json!([]));
        assert_eq!(
            quiet["shots"],
            serde_json::json!([{"start": 0.0, "end": 8.0}]),
            "no cuts means one shot, and an edit can still read the span"
        );
    }

    /// The transcript distiller on whisper.cpp's own JSON shape, and the
    /// honest refusal when nothing can transcribe.
    #[test]
    fn a_transcript_distills_to_cues_and_the_refusal_names_the_fix() {
        let raw = serde_json::json!({
            "result": { "language": "en" },
            "transcription": [
                { "offsets": { "from": 0, "to": 3210 }, "text": " PromoShot builds promo videos." },
                { "offsets": { "from": 5050, "to": 9030 }, "text": "  " },
                { "offsets": { "from": 11020, "to": 14960 }, "text": " This clip exists." }
            ]
        });
        let cues = distill_transcript(&raw);
        let list = cues["cues"].as_array().unwrap();
        assert_eq!(list.len(), 2, "blank segments drop: {cues}");
        assert_eq!(list[0]["start"], 0.0);
        assert_eq!(list[0]["end"], 3.21);
        assert_eq!(list[1]["text"], "This clip exists.");
        assert_eq!(cues["language"], "en");

        if std::env::var("WHISPER_MODEL").is_err() {
            let scratch =
                std::env::temp_dir().join(format!("promo-noaudio-{}", std::process::id()));
            std::fs::write(&scratch, b"not audio").unwrap();
            let err =
                transcribe(&serde_json::json!({ "file": scratch.to_string_lossy() })).unwrap_err();
            assert!(
                err.contains("WHISPER_MODEL") && err.contains("whisper-cli"),
                "{err}"
            );
            let _ = std::fs::remove_file(&scratch);
        }
    }

    #[test]
    fn silences_and_their_inverse_come_out_of_the_log() {
        let log = "\
[silencedetect @ 0x1] silence_start: 1.5\n\
[silencedetect @ 0x1] silence_end: 2.75 | silence_duration: 1.25\n\
[silencedetect @ 0x1] silence_start: 9.0\n";
        let value = distill_silences(log, 10.0, -35.0, 0.35);
        assert_eq!(value["silences"][0]["start"], 1.5);
        assert_eq!(value["silences"][0]["end"], 2.75);
        assert_eq!(value["silences"][1]["end"], 10.0, "open silence runs out");
        assert_eq!(value["sound"][0]["start"], 0.0);
        assert_eq!(value["sound"][0]["end"], 1.5);
        assert_eq!(value["sound"][1]["start"], 2.75);
        assert_eq!(value["sound"][1]["end"], 9.0);
    }

    /// Grid and cadence: as square as it gets, times matching ffmpeg's
    /// fps=count/duration sampling.
    #[test]
    fn the_filmstrip_grid_and_times_agree_with_the_filter() {
        assert_eq!(grid_for(12), (6, 2));
        assert_eq!(grid_for(6), (4, 2));
        assert_eq!(grid_for(3), (3, 1));
        let times = sample_times(6.0, 12);
        assert_eq!(times.len(), 12);
        assert!((times[1] - 0.5).abs() < 1e-9);
        assert!((times[11] - 5.5).abs() < 1e-9);
    }
}
