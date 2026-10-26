//! Settling a MIDI resource (MIDI.md §2), the `promo-speech` settle's
//! shape: the receipt decides; a take whose receipt matches and whose
//! file is there is reused; anything else renders, the WAV lands in
//! `Resources/` atomically, and the resource gets its filename, duration,
//! authored `beatGrid` and receipt.

use std::path::{Path, PathBuf};

use promo_model::midi::{self, MidiBlock};
use serde_json::{json, Value};

use crate::render::{self, Bank};
use crate::score::{self, Score};

/// One track as a result lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackInfo {
    pub name: String,
    pub program: u8,
    /// The GM program's name, or the kit's for drums.
    pub program_name: String,
    pub family: String,
    pub drums: bool,
    /// A file track's channel.
    pub channel: Option<u8>,
    pub notes: usize,
    pub muted: bool,
}

/// What a settle did.
#[derive(Debug, Clone)]
pub struct Settled {
    pub resource: String,
    pub filename: String,
    pub duration: f64,
    pub bpm: f64,
    pub time_signature: [u32; 2],
    pub rendered_hash: String,
    /// The receipt held and the file was there: nothing rendered.
    pub reused: bool,
    pub looped: bool,
    pub tracks: Vec<TrackInfo>,
    pub notes: Vec<String>,
    pub peak_dbfs: Option<f64>,
    pub bank: String,
}

impl Settled {
    /// `{resource, filename, duration, beatGrid {bpm, offset, timeSignature},
    /// renderedHash, reused, loop, tracks: [{name, program, programName,
    /// family, drums, channel?, notes, muted}], notes: [..], peakDbfs?,
    /// bank}`.
    pub fn to_json(&self) -> Value {
        json!({
            "resource": self.resource,
            "filename": self.filename,
            "duration": self.duration,
            "beatGrid": grid(self.bpm, self.time_signature),
            "renderedHash": self.rendered_hash,
            "reused": self.reused,
            "loop": self.looped,
            "tracks": self.tracks.iter().map(|t| {
                let mut v = json!({
                    "name": t.name, "program": t.program, "programName": t.program_name,
                    "family": t.family, "drums": t.drums, "notes": t.notes, "muted": t.muted,
                });
                if let Some(c) = t.channel {
                    v["channel"] = json!(c);
                }
                v
            }).collect::<Vec<_>>(),
            "notes": self.notes,
            "peakDbfs": self.peak_dbfs.filter(|p| p.is_finite()).map(|p| (p * 10.0).round() / 10.0),
            "bank": self.bank,
        })
    }

    /// One line per fact, for a CLI or a tool's text answer.
    pub fn report(&self) -> String {
        let mut lines = vec![format!(
            "{}: {} {} ({:.3} s, {:.2} BPM {}/{}{})",
            self.resource,
            if self.reused { "reused" } else { "rendered" },
            self.filename,
            self.duration,
            self.bpm,
            self.time_signature[0],
            self.time_signature[1],
            if self.looped { ", seamless loop" } else { "" }
        )];
        for t in &self.tracks {
            lines.push(format!(
                "  {}{}: {} — {} ({}), {} note(s){}",
                t.name,
                t.channel
                    .map(|c| format!(" [ch {}]", c + 1))
                    .unwrap_or_default(),
                if t.drums { "drums" } else { "program" },
                t.program_name,
                t.program,
                t.notes,
                if t.muted { ", muted" } else { "" }
            ));
        }
        for note in &self.notes {
            lines.push(format!("  note: {note}"));
        }
        lines.join("\n")
    }
}

/// The authored grid: beat 1 at the file's start.
pub fn grid(bpm: f64, time_signature: [u32; 2]) -> Value {
    json!({ "bpm": (bpm * 1e6).round() / 1e6, "offset": 0, "timeSignature": time_signature })
}

fn plain(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && !name.starts_with('.')
        && name != ".."
}

fn index_of(doc: &Value, id: &str) -> Result<usize, String> {
    doc.get("resources")
        .and_then(Value::as_array)
        .and_then(|list| list.iter().position(|r| r["id"] == id))
        .ok_or_else(|| format!("no resource `{id}` in the project"))
}

/// The resource's block, typed, and its `.mid` bytes for a file source.
fn read_block(
    resource: &Value,
    resources_dir: &Path,
) -> Result<(MidiBlock, Option<Vec<u8>>), String> {
    let id = resource["id"].as_str().unwrap_or_default();
    let value = resource
        .get(midi::KEY)
        .filter(|v| v.is_object())
        .ok_or_else(|| format!("{id}: the resource has no `midi` block"))?;
    let block = MidiBlock::from_value(value).map_err(|e| format!("{id}: {e}"))?;
    let mid = if block.source() == "file" {
        let name = block
            .file
            .as_deref()
            .ok_or_else(|| format!("{id}: a file source names no `file`"))?;
        if !plain(name) {
            return Err(format!(
                "{id}: midi.file \"{name}\" must be a plain file name in Resources/.midi/"
            ));
        }
        let path = mid_path(resources_dir, name);
        Some(std::fs::read(&path).map_err(|e| format!("{id}: {}: {e}", path.display()))?)
    } else {
        None
    };
    Ok((block, mid))
}

/// Where an imported `.mid` keeps its bytes.
pub fn mid_path(resources_dir: &Path, name: &str) -> PathBuf {
    resources_dir.join(midi::MIDI_DIR).join(name)
}

/// The name a take is written under: the resource's own `.wav` name, a
/// `.mp3`/`.m4a` one renamed to `.wav`, else its display name (or id) as
/// a `.wav` — never one another resource uses.
fn take_name(doc: &Value, index: usize) -> String {
    let resource = &doc["resources"][index];
    let id = resource["id"].as_str().unwrap_or("midi");
    let existing = resource["filename"].as_str().unwrap_or_default();
    let candidate = if plain(existing) {
        match existing.rsplit_once('.') {
            Some((_, ext)) if ext.eq_ignore_ascii_case("wav") => existing.to_string(),
            Some((stem, _)) => format!("{stem}.wav"),
            None => format!("{existing}.wav"),
        }
    } else {
        let display = resource["displayName"].as_str().unwrap_or_default().trim();
        let base: String = display
            .chars()
            .map(|c| {
                if c == '/' || c == '\\' || c == ':' {
                    '-'
                } else {
                    c
                }
            })
            .collect();
        if plain(&base) {
            format!("{base}.wav")
        } else {
            format!("midi-{id}.wav")
        }
    };
    let taken = |name: &str| {
        doc["resources"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .any(|(i, r)| i != index && r["filename"] == name)
    };
    if taken(&candidate) {
        format!("midi-{id}.wav")
    } else {
        candidate
    }
}

fn track_infos(score: &Score, file: bool) -> Vec<TrackInfo> {
    score
        .tracks
        .iter()
        .map(|t| TrackInfo {
            name: t.name.clone(),
            program: t.program,
            program_name: if t.drums {
                format!("{} Kit", crate::gm::kit_name(t.program))
            } else {
                crate::gm::program_name(t.program).to_string()
            },
            family: if t.drums {
                "Drums".into()
            } else {
                crate::gm::family(t.program).into()
            },
            drums: t.drums,
            channel: file.then_some(t.channel),
            notes: t.notes,
            muted: t.muted,
        })
        .collect()
}

/// Why the resource's take is not current with `bank_sha256`, or None
/// when it is: no take yet, its file gone, or a receipt that no longer
/// matches. Reads the `.mid`; renders nothing.
pub fn stale(
    doc: &Value,
    resources_dir: &Path,
    resource_id: &str,
    bank_sha256: &str,
) -> Result<Option<String>, String> {
    let index = index_of(doc, resource_id)?;
    let resource = &doc["resources"][index];
    let (_, mid) = read_block(resource, resources_dir)?;
    let block = &resource[midi::KEY];
    let filename = resource["filename"].as_str().unwrap_or_default();
    if filename.is_empty() {
        return Ok(Some("not rendered yet".into()));
    }
    if !resources_dir.join(filename).is_file() {
        return Ok(Some(format!("its file {filename} is missing")));
    }
    let print = midi::receipt(block, mid.as_deref(), bank_sha256);
    match block.get("renderedHash").and_then(Value::as_str) {
        Some(held) if held == print => Ok(None),
        Some(_) => Ok(Some("the block changed since it was rendered".into())),
        None => Ok(Some("no receipt".into())),
    }
}

/// Settles `resource_id` in `doc` (a project's metadata.json as JSON),
/// writing its take into `resources_dir`. A file source that lists no
/// tracks gets the file's tracks first (part of what the receipt covers).
pub fn settle_doc(
    doc: &mut Value,
    resources_dir: &Path,
    resource_id: &str,
    bank: &Bank,
) -> Result<Settled, String> {
    let index = index_of(doc, resource_id)?;
    let (mut block, mid) = read_block(&doc["resources"][index], resources_dir)?;
    if let Some(bytes) = &mid {
        if block.tracks.as_ref().is_none_or(Vec::is_empty) {
            let tracks = score::file_tracks(bytes).map_err(|e| format!("{resource_id}: {e}"))?;
            doc["resources"][index][midi::KEY]["tracks"] =
                serde_json::to_value(&tracks).map_err(|e| e.to_string())?;
            block.tracks = Some(tracks);
        }
    }
    let score = score::expand(&block, mid.as_deref()).map_err(|e| format!("{resource_id}: {e}"))?;
    let print = midi::receipt(
        &doc["resources"][index][midi::KEY],
        mid.as_deref(),
        &bank.sha256,
    );
    let filename = take_name(doc, index);
    let destination = resources_dir.join(&filename);
    let held = doc["resources"][index][midi::KEY]["renderedHash"].as_str() == Some(print.as_str());
    let reused = held && destination.is_file();
    let mut notes = score.notes.clone();
    let mut peak = None;
    let duration = if reused {
        match doc["resources"][index]["duration"].as_f64() {
            Some(d) if doc["resources"][index]["filename"] == filename.as_str() => d,
            _ => wav_seconds(&destination)?,
        }
    } else {
        let take = render::render(&score, bank).map_err(|e| format!("{resource_id}: {e}"))?;
        std::fs::create_dir_all(resources_dir).map_err(|e| e.to_string())?;
        crate::sounds::write_atomic(&destination, &render::wav(&take))?;
        notes.extend(take.notes.iter().cloned());
        peak = Some(take.peak_dbfs);
        take.seconds()
    };
    {
        let resource = &mut doc["resources"][index];
        let fresh = !reused;
        resource["kind"] = json!("audio");
        resource["filename"] = json!(filename);
        resource["duration"] = json!(duration);
        // The grid is the sequence's; whatever else the grid held stays.
        let mut beat_grid = grid(score.bpm, score.time_signature);
        if let (Some(old), Some(new)) = (
            resource.get("beatGrid").and_then(Value::as_object),
            beat_grid.as_object_mut(),
        ) {
            for (k, v) in old {
                new.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        resource["beatGrid"] = beat_grid;
        if fresh {
            if let Some(object) = resource.as_object_mut() {
                object.remove("trimStart");
                object.remove("trimEnd");
                object.remove("trimKeyframes");
            }
        }
        resource[midi::KEY]["renderedHash"] = json!(print);
        resource[midi::KEY]["bank"] = json!({ "name": bank.name, "sha256": bank.sha256 });
    }
    Ok(Settled {
        resource: resource_id.to_string(),
        filename,
        duration,
        bpm: score.bpm,
        time_signature: score.time_signature,
        rendered_hash: print,
        reused,
        looped: score.looped,
        tracks: track_infos(&score, mid.is_some()),
        notes,
        peak_dbfs: peak,
        bank: bank.name.clone(),
    })
}

/// A WAV file's length in seconds, from its header.
fn wav_seconds(path: &Path) -> Result<f64, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut at = 12usize;
    let (mut rate, mut block_align) = (0u32, 0u16);
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]])
            as usize;
        let body = at + 8;
        if id == b"fmt " && body + 16 <= bytes.len() {
            rate = u32::from_le_bytes([
                bytes[body + 4],
                bytes[body + 5],
                bytes[body + 6],
                bytes[body + 7],
            ]);
            block_align = u16::from_le_bytes([bytes[body + 12], bytes[body + 13]]);
        } else if id == b"data" && rate > 0 && block_align > 0 {
            let size = size.min(bytes.len() - body);
            return Ok(size as f64 / f64::from(block_align) / f64::from(rate));
        }
        at = body + size + (size & 1);
    }
    Err(format!(
        "{}: not a WAV file this can measure",
        path.display()
    ))
}

/// Settles `resource_id` in the project at `project_dir`: its
/// metadata.json read raw (every unknown key kept), settled, stamped (a
/// MIDI resource needs the lossless reader) and written back atomically.
pub fn settle(project_dir: &Path, resource_id: &str, bank: &Bank) -> Result<Settled, String> {
    let meta_path = project_dir.join("metadata.json");
    let text = std::fs::read_to_string(&meta_path)
        .map_err(|_| format!("no metadata.json in {}", project_dir.display()))?;
    let mut doc: Value = serde_json::from_str(&text).map_err(|e| format!("decode: {e}"))?;
    let before = doc.clone();
    let settled = settle_doc(&mut doc, &project_dir.join("Resources"), resource_id, bank)?;
    if doc != before {
        stamp(&mut doc);
        let out = serde_json::to_string(&doc).map_err(|e| e.to_string())?;
        crate::sounds::write_atomic(&meta_path, out.as_bytes())?;
    }
    Ok(settled)
}

/// Raises `minReaderVersion` to what the document now needs, never lower.
pub fn stamp(doc: &mut Value) {
    let Ok(meta) = promo_model::ProjectMetadata::from_json(&doc.to_string()) else {
        return;
    };
    let needed = meta.minimum_reader_version();
    let held = doc["minReaderVersion"].as_i64().unwrap_or(0);
    if needed > held {
        doc["minReaderVersion"] = json!(needed);
    }
}

/// The MIDI resources of a document, by id.
pub fn midi_resources(doc: &Value) -> Vec<String> {
    doc.get("resources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|r| r.get(midi::KEY).is_some_and(Value::is_object))
        .filter_map(|r| r["id"].as_str().map(str::to_string))
        .collect()
}

/// What an imported file holds.
#[derive(Debug, Clone)]
pub struct Imported {
    /// Its name in `Resources/.midi/`.
    pub file: String,
    pub tracks: Vec<promo_model::midi::MidiTrack>,
    pub bpm: f64,
    pub time_signature: [u32; 2],
    pub seconds: f64,
    pub notes: Vec<String>,
}

impl Imported {
    pub fn to_json(&self) -> Value {
        json!({
            "file": self.file,
            "tracks": self.tracks,
            "tempo": (self.bpm * 1e6).round() / 1e6,
            "timeSignature": self.time_signature,
            "seconds": self.seconds,
            "notes": self.notes,
        })
    }
}

/// Keeps an imported `.mid`'s own bytes in `Resources/.midi/` under
/// `name` (made a plain `.mid` name; an existing different file of that
/// name is not overwritten — the new one gets a numbered name), and says
/// what it holds. Refuses bytes that are not a MIDI file.
pub fn import_mid(resources_dir: &Path, name: &str, bytes: &[u8]) -> Result<Imported, String> {
    let file = crate::smf::read(bytes)?;
    let tracks = score::file_tracks(bytes)?;
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem: String = stem
        .trim()
        .trim_start_matches('.')
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c == ':' {
                '-'
            } else {
                c
            }
        })
        .collect();
    let stem = if stem.is_empty() {
        "Imported".to_string()
    } else {
        stem
    };
    let dir = resources_dir.join(midi::MIDI_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut chosen = format!("{stem}.mid");
    let mut n = 2;
    loop {
        let path = dir.join(&chosen);
        match std::fs::read(&path) {
            Ok(existing) if existing == bytes => break,
            Ok(_) => {
                chosen = format!("{stem} {n}.mid");
                n += 1;
            }
            Err(_) => {
                crate::sounds::write_atomic(&path, bytes)?;
                break;
            }
        }
    }
    let bpm = file.first_qpm * f64::from(file.time_signature[1]) / 4.0;
    let mut notes = Vec::new();
    if file.tempo_changes > 0 {
        notes.push(format!(
            "tempo changes: the file changes tempo {} time(s); the beat grid keeps the first \
             tempo ({bpm:.2} BPM)",
            file.tempo_changes
        ));
    }
    Ok(Imported {
        file: chosen,
        tracks,
        bpm,
        time_signature: file.time_signature,
        seconds: file.end,
        notes,
    })
}

/// Validate's MIDI findings, one line per MIDI resource whose take is not
/// current — no take, its file gone, or a block edited since its render
/// (checked against the bank the take records, so no bank is needed to
/// say so).
pub fn findings(doc: &Value, resources_dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for id in midi_resources(doc) {
        let Some(resource) = doc["resources"]
            .as_array()
            .and_then(|list| list.iter().find(|r| r["id"] == id.as_str()))
        else {
            continue;
        };
        let name = resource["displayName"]
            .as_str()
            .filter(|n| !n.is_empty())
            .unwrap_or(&id);
        let bank = resource[midi::KEY]["bank"]["sha256"]
            .as_str()
            .unwrap_or_default();
        let why = match stale(doc, resources_dir, &id, bank) {
            Ok(None) => continue,
            Ok(Some(why)) => why,
            Err(e) => e,
        };
        out.push(format!(
            "MIDI not rendered: \"{name}\" ({why}) — it plays silent or stale until promo_midi \
             (or `promo midi render <project> {id}`) renders it"
        ));
    }
    out
}
