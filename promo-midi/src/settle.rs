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
    /// Integrated loudness of the take, LUFS (None: silent or unknown).
    pub loudness: Option<f64>,
    /// The normalisation gain the take got, dB.
    pub gain_db: Option<f64>,
    pub bank: String,
    /// The generated layers the take mixed (or would have).
    pub layers: Vec<LayerInfo>,
}

/// One generated layer as a result lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerInfo {
    pub name: String,
    pub file: Option<String>,
    /// Its file is in `Resources/.midi/`.
    pub present: bool,
    pub muted: bool,
    /// `current` or `contextChanged` (as the settle left it).
    pub status: Option<String>,
}

impl Settled {
    /// `{resource, filename, duration, beatGrid {bpm, offset, timeSignature},
    /// renderedHash, reused, loop, tracks: [{name, program, programName,
    /// family, drums, channel?, notes, muted}], notes: [..], peakDbfs?,
    /// loudness (LUFS) , gainDb, bank}`.
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
            "loudness": self.loudness.map(|l| (l * 10.0).round() / 10.0),
            "gainDb": self.gain_db.map(|g| (g * 10.0).round() / 10.0),
            "bank": self.bank,
            "layers": self.layers.iter().map(|l| json!({
                "name": l.name, "file": l.file, "present": l.present, "muted": l.muted,
                "status": l.status,
            })).collect::<Vec<_>>(),
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
        for l in &self.layers {
            lines.push(format!(
                "  {}: generated layer {}{}{}",
                l.name,
                l.file.as_deref().unwrap_or("(no file)"),
                if l.present { "" } else { " — missing" },
                match l.status.as_deref() {
                    Some(midi::STATUS_CONTEXT_CHANGED) =>
                        ", context changed since it was generated",
                    _ if l.muted => ", muted",
                    _ => "",
                }
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
    // Only a slice-1 file block (no notes inline) reads its .mid.
    let mid = if block.source() == "file" && !score::has_notes(&block) {
        let name = block
            .file
            .as_deref()
            .or(block.original.as_deref())
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
    let (layers, _) = layer_bytes(block, resources_dir);
    let print = midi::receipt_with_layers(block, mid.as_deref(), &layers, bank_sha256);
    match block.get("renderedHash").and_then(Value::as_str) {
        Some(held) if held == print => Ok(None),
        Some(_) => Ok(Some("the block changed since it was rendered".into())),
        None => Ok(Some("no receipt".into())),
    }
}

/// The generated layers' files, in track order: each one's bytes (None
/// when the track names no plain file or the file is not there), and the
/// names. Only `Resources/.midi/` is read.
pub fn layer_bytes(
    block: &Value,
    resources_dir: &Path,
) -> (Vec<midi::LayerBytes>, Vec<Option<String>>) {
    let mut bytes = Vec::new();
    let mut names = Vec::new();
    for track in block["tracks"].as_array().into_iter().flatten() {
        let Some(generated) = track.get("generated").filter(|g| !g.is_null()) else {
            continue;
        };
        let name = generated["file"]
            .as_str()
            .filter(|f| plain(f))
            .map(str::to_string);
        bytes.push(
            name.as_deref()
                .and_then(|n| std::fs::read(mid_path(resources_dir, n)).ok()),
        );
        names.push(name);
    }
    (bytes, names)
}

/// A resource's index, its slice-1 `.mid` bytes, its score, receipt and
/// its layers' audio.
struct Prepared {
    index: usize,
    mid: Option<Vec<u8>>,
    score: Score,
    print: String,
    layers: Vec<Option<render::Stereo>>,
    present: Vec<bool>,
}

/// The block, its score and its receipt under `bank`.
fn prepare(
    doc: &Value,
    resources_dir: &Path,
    resource_id: &str,
    bank: &Bank,
) -> Result<Prepared, String> {
    let index = index_of(doc, resource_id)?;
    let (block, mid) = read_block(&doc["resources"][index], resources_dir)?;
    let score = score::expand(&block, mid.as_deref()).map_err(|e| format!("{resource_id}: {e}"))?;
    let value = &doc["resources"][index][midi::KEY];
    let (bytes, _) = layer_bytes(value, resources_dir);
    let print = midi::receipt_with_layers(value, mid.as_deref(), &bytes, &bank.sha256);
    let mut layers = Vec::new();
    for (layer, data) in score.layers.iter().zip(&bytes) {
        layers.push(match data {
            Some(data) => Some(render::read_stereo(data).map_err(|e| {
                format!(
                    "{resource_id}: generated layer \"{}\" ({}): {e}",
                    layer.name,
                    layer.file.as_deref().unwrap_or_default()
                )
            })?),
            None => None,
        });
    }
    let present = bytes.iter().map(Option::is_some).collect();
    Ok(Prepared {
        index,
        mid,
        score,
        print,
        layers,
        present,
    })
}

/// Marks each generated track of the resource's block against the MIDI
/// it now holds: `contextChanged` when its `contextHash` no longer
/// matches, `current` again when it does (an edit undone). Nothing is
/// regenerated. True when a status changed.
pub fn mark_context(resource: &mut Value, mid: Option<&[u8]>) -> bool {
    let Some(block) = resource.get(midi::KEY).filter(|b| b.is_object()) else {
        return false;
    };
    let heard = midi::context_hash(block, mid);
    let mut changed = false;
    if let Some(tracks) = resource[midi::KEY]["tracks"].as_array_mut() {
        for track in tracks {
            let Some(generated) = track.get_mut("generated").and_then(Value::as_object_mut) else {
                continue;
            };
            let Some(held) = generated.get("contextHash").and_then(Value::as_str) else {
                continue;
            };
            let status = if held == heard {
                midi::STATUS_CURRENT
            } else {
                midi::STATUS_CONTEXT_CHANGED
            };
            if generated.get("status").and_then(Value::as_str) != Some(status) {
                generated.insert("status".into(), json!(status));
                changed = true;
            }
        }
    }
    changed
}

fn layer_infos(score: &Score, present: &[bool]) -> Vec<LayerInfo> {
    score
        .layers
        .iter()
        .enumerate()
        .map(|(i, l)| LayerInfo {
            name: l.name.clone(),
            file: l.file.clone(),
            present: present.get(i).copied().unwrap_or(false),
            muted: l.muted,
            status: l.status.clone(),
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn settled(
    resource_id: &str,
    filename: String,
    duration: f64,
    score: &Score,
    print: String,
    reused: bool,
    file: bool,
    take: Option<&render::Take>,
    held: &Value,
    bank: &Bank,
    layers: Vec<LayerInfo>,
) -> Settled {
    let mut notes = score.notes.clone();
    if let Some(take) = take {
        notes.extend(take.notes.iter().cloned());
    }
    Settled {
        resource: resource_id.to_string(),
        filename,
        duration,
        bpm: score.bpm,
        time_signature: score.time_signature,
        rendered_hash: print,
        reused,
        looped: score.looped,
        tracks: track_infos(score, file),
        notes,
        peak_dbfs: take
            .map(|t| t.peak_dbfs)
            .or_else(|| held["peakDbfs"].as_f64()),
        loudness: take.map_or_else(|| held["loudness"].as_f64(), |t| t.loudness),
        gain_db: take.map(|t| t.gain_db).or_else(|| held["gainDb"].as_f64()),
        bank: bank.name.clone(),
        layers,
    }
}

/// Settles `resource_id` in `doc` (a project's metadata.json as JSON),
/// writing its take into `resources_dir`: filename, duration, the
/// sequence's `beatGrid`, `looped` (a loop's take has no trims), and in
/// the block the receipt, the bank and `take {loudness, gainDb,
/// peakDbfs}`.
pub fn settle_doc(
    doc: &mut Value,
    resources_dir: &Path,
    resource_id: &str,
    bank: &Bank,
) -> Result<Settled, String> {
    let Prepared {
        index,
        mid,
        mut score,
        print,
        layers,
        present,
    } = prepare(doc, resources_dir, resource_id, bank)?;
    // The layers hear the MIDI as it is now: an edit marks them.
    mark_context(&mut doc["resources"][index], mid.as_deref());
    if let Ok(block) = MidiBlock::from_value(&doc["resources"][index][midi::KEY]) {
        let marked = score::layer_statuses(&block);
        for (layer, status) in score.layers.iter_mut().zip(marked) {
            layer.status = status;
        }
    }
    let filename = take_name(doc, index);
    let destination = resources_dir.join(&filename);
    let held = doc["resources"][index][midi::KEY]["renderedHash"].as_str() == Some(print.as_str());
    let reused = held && destination.is_file();
    let mut take = None;
    let duration = if reused {
        match doc["resources"][index]["duration"].as_f64() {
            Some(d) if doc["resources"][index]["filename"] == filename.as_str() => d,
            _ => wav_seconds(&destination)?,
        }
    } else {
        let rendered =
            render::render_mix(&score, bank, &layers).map_err(|e| format!("{resource_id}: {e}"))?;
        std::fs::create_dir_all(resources_dir).map_err(|e| e.to_string())?;
        crate::sounds::write_atomic(&destination, &render::wav(&rendered))?;
        let seconds = rendered.seconds();
        take = Some(rendered);
        seconds
    };
    let record = doc["resources"][index][midi::KEY]["take"].clone();
    let result = settled(
        resource_id,
        filename.clone(),
        duration,
        &score,
        print.clone(),
        reused,
        mid.is_some(),
        take.as_ref(),
        &record,
        bank,
        layer_infos(&score, &present),
    );
    {
        let resource = &mut doc["resources"][index];
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
        if let Some(object) = resource.as_object_mut() {
            if !reused || score.looped {
                object.remove("trimStart");
                object.remove("trimEnd");
                object.remove("trimKeyframes");
            }
            // One Loop per resource: the take loops exactly when the block does.
            if score.looped {
                object.insert("looped".into(), json!(true));
            } else {
                object.remove("looped");
            }
        }
        resource[midi::KEY]["renderedHash"] = json!(print);
        resource[midi::KEY]["bank"] = json!({ "name": bank.name, "sha256": bank.sha256 });
        if let Some(take) = &take {
            let round = |x: f64| (x * 100.0).round() / 100.0;
            let mut record = json!({ "gainDb": round(take.gain_db) });
            if let Some(l) = take.loudness {
                record["loudness"] = json!(round(l));
            }
            if take.peak_dbfs.is_finite() {
                record["peakDbfs"] = json!(round(take.peak_dbfs));
            }
            resource[midi::KEY]["take"] = record;
        }
    }
    Ok(result)
}

/// The same render as a settle, written to `out` (an audition in the
/// cache) — the project untouched, nothing reused.
pub fn preview(
    doc: &Value,
    resources_dir: &Path,
    resource_id: &str,
    bank: &Bank,
    out: &Path,
) -> Result<Settled, String> {
    let Prepared {
        mid,
        score,
        print,
        layers,
        present,
        ..
    } = prepare(doc, resources_dir, resource_id, bank)?;
    let take =
        render::render_mix(&score, bank, &layers).map_err(|e| format!("{resource_id}: {e}"))?;
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    crate::sounds::write_atomic(out, &render::wav(&take))?;
    Ok(settled(
        resource_id,
        out.to_string_lossy().into_owned(),
        take.seconds(),
        &score,
        print,
        false,
        mid.is_some(),
        Some(&take),
        &Value::Null,
        bank,
        layer_infos(&score, &present),
    ))
}

/// Every id a pointer in `node` names — a `resourceID`, any
/// `…ResourceID`, a theme plate, a morph's bodies (the inspect walk's
/// rule, promo-cli `inspect::pointed_at`).
fn pointed_at(node: &Value, parent: Option<&str>, out: &mut Vec<String>) {
    match node {
        Value::Object(map) => {
            for (key, value) in map {
                let pointer = key == "resourceID"
                    || key.ends_with("ResourceID")
                    || key == "themePlateID"
                    || (parent == Some("morph") && (key == "from" || key == "to"));
                if pointer {
                    if let Some(id) = value.as_str() {
                        if !out.iter().any(|o| o == id) {
                            out.push(id.to_string());
                        }
                    }
                }
                pointed_at(value, Some(key), out);
            }
        }
        Value::Array(items) => {
            for item in items {
                pointed_at(item, parent, out);
            }
        }
        _ => {}
    }
}

/// The MIDI resources a layer uses: what the project's layers and settings
/// point at, then what those resources point at in turn (a composition's
/// layers, nested to any depth), in document order.
pub fn used_midi(doc: &Value) -> Vec<String> {
    let mut used = Vec::new();
    pointed_at(&doc["layers"], None, &mut used);
    pointed_at(&doc["compositionSettings"], None, &mut used);
    let resources: &[Value] = doc["resources"].as_array().map_or(&[], Vec::as_slice);
    let mut walked = 0;
    while walked < used.len() {
        let id = used[walked].clone();
        walked += 1;
        if let Some(r) = resources.iter().find(|r| r["id"] == id.as_str()) {
            // A resource's own pointers (a composition's layers) — not its
            // midi block, which names none.
            pointed_at(r, None, &mut used);
        }
    }
    let midi = midi_resources(doc);
    midi.into_iter().filter(|id| used.contains(id)).collect()
}

/// Settles every MIDI resource a layer uses, each answered on its own.
pub fn settle_used(
    doc: &mut Value,
    resources_dir: &Path,
    bank: &Bank,
) -> Vec<(String, Result<Settled, String>)> {
    used_midi(doc)
        .into_iter()
        .map(|id| {
            let result = settle_doc(doc, resources_dir, &id, bank);
            (id, result)
        })
        .collect()
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

/// What an imported file holds, in the block's own shape (UX v2 §1).
#[derive(Debug, Clone)]
pub struct Imported {
    /// Its name in `Resources/.midi/`: the block's `original`.
    pub original: String,
    /// One per channel that plays, notes (beats) and controls inline.
    pub tracks: Vec<promo_model::midi::MidiTrack>,
    pub tempo_map: Vec<promo_model::midi::MidiTempo>,
    pub bpm: f64,
    pub time_signature: [u32; 2],
    /// Whole bars the notes reach.
    pub bars: u32,
    pub beats: f64,
    pub notes: Vec<String>,
}

impl Imported {
    /// `{original, file, tracks, tempo, tempoMap, timeSignature, bars,
    /// beats, notes}` — `file` is `original` again, for slice-1 readers.
    pub fn to_json(&self) -> Value {
        json!({
            "original": self.original,
            "file": self.original,
            "tracks": self.tracks,
            "tempo": (self.bpm * 1e6).round() / 1e6,
            "tempoMap": self.tempo_map,
            "timeSignature": self.time_signature,
            "bars": self.bars,
            "beats": self.beats,
            "notes": self.notes,
        })
    }

    /// The block an import starts as: notes inline, the file's tempo map
    /// and meter, the `.mid` kept as `original`, no loop.
    pub fn block(&self) -> Value {
        let mut block = json!({
            "source": "notes",
            "tempo": (self.bpm * 1e6).round() / 1e6,
            "timeSignature": self.time_signature,
            "bars": self.bars,
            "tracks": self.tracks,
            "original": self.original,
        });
        if self.tempo_map.len() > 1 {
            block["tempoMap"] = json!(self.tempo_map);
        }
        block
    }
}

/// Keeps an imported `.mid`'s own bytes in `Resources/.midi/` under
/// `name` (made a plain `.mid` name; an existing different file of that
/// name is not overwritten — the new one gets a numbered name), and reads
/// it into tracks of notes. Refuses bytes that are not a MIDI file.
pub fn import_mid(resources_dir: &Path, name: &str, bytes: &[u8]) -> Result<Imported, String> {
    let file = crate::smf::read(bytes)?;
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
    let bpm = file.first_bpm();
    let bar = f64::from(file.time_signature[0]);
    let mut notes = Vec::new();
    if file.tempo_changes > 0 {
        notes.push(format!(
            "tempo changes: the file changes tempo {} time(s); they are kept in tempoMap and \
             played, and the beat grid keeps the first tempo ({bpm:.2} BPM)",
            file.tempo_changes
        ));
    }
    Ok(Imported {
        original: chosen,
        bars: ((file.end_beats / bar - 1e-9).ceil() as u32).max(1),
        beats: file.end_beats,
        tracks: file.tracks,
        tempo_map: file.tempo_map,
        bpm,
        time_signature: file.time_signature,
        notes,
    })
}

/// Validate's MIDI findings, one line per USED MIDI resource whose take is not
/// current — no take, its file gone, or a block edited since its render
/// (checked against the bank the take records, so no bank is needed to
/// say so).
pub fn findings(doc: &Value, resources_dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stale_any = false;
    for id in used_midi(doc) {
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
        out.extend(layer_findings(resource, resources_dir, name, &id));
        let why = match stale(doc, resources_dir, &id, bank) {
            Ok(None) => continue,
            Ok(Some(why)) => why,
            Err(e) => e,
        };
        stale_any = true;
        out.push(format!(
            "MIDI not rendered: \"{name}\" ({why}) — it plays silent or stale until promo_midi \
             (or `promo midi render <project> {id}`) renders it"
        ));
    }
    if stale_any && crate::sounds::installed_default().is_none() {
        out.push(crate::sounds::missing_bank_message());
    }
    out
}

/// Validate's words for a resource's generated layers: a file that is not
/// in `Resources/.midi/`, and a layer whose MIDI has changed since it was
/// generated (by its `contextHash`, so it needs no settle to say so).
fn layer_findings(resource: &Value, resources_dir: &Path, name: &str, id: &str) -> Vec<String> {
    let mut out = Vec::new();
    let block = &resource[midi::KEY];
    let mid = read_block(resource, resources_dir)
        .ok()
        .and_then(|(_, mid)| mid);
    let heard = midi::context_hash(block, mid.as_deref());
    for track in block["tracks"].as_array().into_iter().flatten() {
        let Some(generated) = track.get("generated").filter(|g| g.is_object()) else {
            continue;
        };
        let track_name = track["name"].as_str().unwrap_or("Generated");
        let file = generated["file"].as_str().unwrap_or_default();
        let regenerate = format!("`promo midi generate <project> {id} --track \"{track_name}\"`");
        if file.is_empty() || !plain(file) || !mid_path(resources_dir, file).is_file() {
            out.push(format!(
                "Generated layer missing: \"{track_name}\" on \"{name}\" — its file {} is not \
                 in Resources/.midi/, so it plays silent; generate it again ({regenerate})",
                if file.is_empty() { "(none)" } else { file }
            ));
            continue;
        }
        let changed = generated["status"] == midi::STATUS_CONTEXT_CHANGED
            || generated["contextHash"]
                .as_str()
                .is_some_and(|held| held != heard);
        if changed {
            out.push(format!(
                "Generated layer context changed: \"{track_name}\" on \"{name}\" was generated \
                 over MIDI that has changed since — it still plays as generated; regenerate it \
                 ({regenerate}) to follow the edit"
            ));
        }
    }
    out
}
