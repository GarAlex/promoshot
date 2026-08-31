//! Senses for the agent: what a source file IS, before anything composes it.
//!
//! `promo_media_probe` answers the facts (streams, sizes, fps, duration),
//! `promo_media_filmstrip` gives eyes (a tiled contact sheet of evenly
//! spaced frames, with the times each cell samples), and
//! `promo_media_silences` gives ears (where the sound isn't, which is where
//! cuts want to land). All three ride the same ffmpeg/ffprobe the render
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
    let distilled = distill_probe(&raw);
    serde_json::to_string_pretty(&distilled).map_err(|e| e.to_string())
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
                if let Some(fps) = stream
                    .get("r_frame_rate")
                    .and_then(Value::as_str)
                    .and_then(parse_fraction)
                {
                    entry["fps"] = json!((fps * 100.0).round() / 100.0);
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

    /// The silence distiller: spans parsed, an unterminated silence runs to
    /// the end, and the INVERSE — the sound — is what the edit reads.
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
