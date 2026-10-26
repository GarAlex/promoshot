//! The `midi` block on an audio resource (docs/MIDI.md in core-private):
//! sound authored as MIDI — step lanes, notes in beats, an effect preset or
//! an imported `.mid` — that `promo-midi` renders to an ordinary WAV in
//! `Resources/`. The resource stays `kind: "audio"`; the block rides in the
//! resource's bag beside `filename`, the way `speech` does
//! ([`crate::unread::APP_RESOURCE_KEYS`] names it), so the apps own it and
//! every writer keeps it.
//!
//! [`MidiBlock`] is the typed reading of that bag value. Every struct here
//! keeps the keys it does not model in `extra`, so a block a newer build
//! wrote reads and re-encodes without loss. Writers edit the bag value
//! itself; the typed form is for reading.
//!
//! The receipt ([`receipt`]) is SHA-256 over the block's canonical
//! encoding — keys sorted, nulls dropped, numbers by value — minus
//! `renderedHash`, `bank` and `take`, then the `.mid` bytes when there are any,
//! then the bank's SHA-256. A take whose receipt matches is reused;
//! anything else renders again.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The bag key the block lives under.
pub const KEY: &str = "midi";

/// The hidden folder in `Resources/` that keeps an imported `.mid`'s own
/// bytes. Like `.beats/` and `.words/` it is not a resource: the inventory
/// lists files only, and a `.mid` is no media kind it adopts.
pub const MIDI_DIR: &str = ".midi";

/// The `midi` block (MIDI.md §1).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiBlock {
    /// `steps` (lanes and/or notes on the tracks), `file` (the `.mid` in
    /// `Resources/.midi/<file>`) or `effect` (a preset). `notes` reads as
    /// `steps`. Absent: `file` when a file is named, `effect` when an
    /// effect is, else `steps`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Beats per minute, a beat being one count of the time signature.
    /// Default 120. A `.mid` keeps its own tempo map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tempo: Option<f64>,
    /// `[beats per bar, beat unit]`, default `[4, 4]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_signature: Option<[u32; 2]>,
    /// The song's length in bars (default: what the notes reach). A
    /// `loop: true` loops all of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bars: Option<u32>,
    /// 0 straight … 1 a full triplet shuffle: every odd step is late by
    /// `swing / 3` of a step (a straight pair becomes 2:1). Steps only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swing: Option<f64>,
    /// The loop: `{from, to}` in bars, 1-based, `to` exclusive — the take
    /// is exactly that range, held notes wrapping and tails folded onto
    /// the head. Absent (or `false`): no loop, the whole song as a
    /// one-shot. `true` loops the whole length.
    #[serde(rename = "loop", default, skip_serializing_if = "Option::is_none")]
    pub looped: Option<MidiLoop>,
    /// Tempo changes, `[{beat, bpm}]` in beats from the start; the first
    /// holds from beat 0. Absent: `tempo` throughout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tempo_map: Option<Vec<MidiTempo>>,
    /// The imported `.mid` this block came from, in `Resources/.midi/`:
    /// kept for "Revert to file", never read by the render.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original: Option<String>,
    /// dB on top of the take's loudness normalisation (default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracks: Option<Vec<MidiTrack>>,
    /// Slice 1's imported file, in `Resources/.midi/`: a `file` block
    /// whose tracks carry no notes is expanded from it on read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<MidiEffect>,
    /// The bank the take was rendered with, written by the render.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bank: Option<MidiBankId>,
    /// The receipt, written by the render — never by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered_hash: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One track: an instrument and its mix. For a `file` source a track is a
/// MIDI channel of the file (`channel`), its notes read from the file.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiTrack {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// GM program 0–127 (for drums: the kit, 0 standard).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<u8>,
    /// `"drums"` for the percussion bank, or a bank-select number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bank: Option<Value>,
    /// Linear gain on the track, 0–2 (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<f64>,
    /// −1 left … 1 right. Absent: centre (a file's own pan).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pan: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mute: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solo: Option<bool>,
    /// A file track's MIDI channel, 0–15 (9 is percussion).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<MidiSteps>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<Vec<MidiNote>>,
    /// Controller moves, pitch bends and program changes in time, as an
    /// imported file had them (sustain, expression, bends…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controls: Option<Vec<MidiControl>>,
    /// A generated layer (MIDI.md §7): an audio file in `Resources/.midi/`
    /// made by a model that heard the rest of the block, mixed like a
    /// track. A track with this plays no notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated: Option<MidiGenerated>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `track.generated` (MIDI.md §7): what made the layer and the file it
/// made. Only `file` reaches the take (with its bytes); the rest is the
/// record a Regenerate starts from.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiGenerated {
    /// `ace-step`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The provider's task, `lego` (a layer over the context).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    /// The bars it plays, 1-based, `to` exclusive (absent: the whole take).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<MidiRange>,
    /// Its file in `Resources/.midi/`, 48 kHz stereo WAV, sample 0 at the
    /// take's start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// [`context_hash`] of the block it heard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_hash: Option<String>,
    /// ACE-Step's `track_name` (strings, synth, drums…): it decides the gate
    /// and whether the mix high-passes the layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_name: Option<String>,
    /// The key the layer was asked for ("C minor").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyscale: Option<String>,
    /// `false`: no high-pass in the mix; a number: its corner in Hz;
    /// absent: 90 Hz on a tonal layer (its low register stacks on the bass).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high_pass: Option<Value>,
    /// `current`, or `contextChanged` once the MIDI it heard has changed —
    /// marked by a settle, never regenerated silently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `generated.status` for a layer whose context is unchanged.
pub const STATUS_CURRENT: &str = "current";
/// `generated.status` once the MIDI the layer heard has changed.
pub const STATUS_CONTEXT_CHANGED: &str = "contextChanged";

/// One move at beat `t`: a controller (`cc` 0–127 to `v` 0–127), a pitch
/// bend (`bend` 0–16383, 8192 centre) or a program change (`program`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MidiControl {
    pub t: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bend: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<u8>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `midi.loop`: a bar range, or (read only) `true`/`false`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MidiLoop {
    Whole(bool),
    Range(MidiRange),
}

/// Bars, 1-based, `to` exclusive.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MidiRange {
    pub from: f64,
    pub to: f64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A tempo from `beat` on.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MidiTempo {
    pub beat: f64,
    pub bpm: f64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl MidiTrack {
    /// Whether the track is a generated layer rather than a synth.
    pub fn is_generated(&self) -> bool {
        self.generated.is_some()
    }

    /// Whether the track plays the percussion bank.
    pub fn is_drums(&self) -> bool {
        match &self.bank {
            Some(Value::String(s)) => s.eq_ignore_ascii_case("drums"),
            _ => self.channel == Some(9),
        }
    }

    /// The bank-select number, for a numbered bank.
    pub fn bank_number(&self) -> Option<u8> {
        self.bank
            .as_ref()
            .and_then(Value::as_u64)
            .map(|b| b.min(127) as u8)
    }
}

/// A step grid: `resolution` steps per bar, lanes of hits.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiSteps {
    /// Steps per bar, default 16.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<u32>,
    /// The pattern's own length in steps, repeated to fill the bars.
    /// Absent: the whole loop, so `on` counts across every bar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<u32>,
    #[serde(default)]
    pub lanes: Vec<MidiLane>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One lane: a key struck on the listed steps (0-based).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiLane {
    pub note: u8,
    #[serde(default)]
    pub on: Vec<u32>,
    /// 1–127, default 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity: Option<u8>,
    /// How long each hit holds, in steps (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<f64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One note: start `t` and duration `d` in beats, key `n`, velocity `v`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MidiNote {
    pub t: f64,
    pub d: f64,
    pub n: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v: Option<u8>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A sound-effect preset and its settings.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiEffect {
    pub preset: String,
    /// Semitones up (or down) from the preset's own pitch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitch: Option<f64>,
    /// Seconds; the preset stretches to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<f64>,
    /// GM program in place of the preset's own instrument.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<u8>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A bank's identity: its name and the SHA-256 of its `.sf2`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MidiBankId {
    pub name: String,
    pub sha256: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl MidiBlock {
    /// The block read from its bag value.
    pub fn from_value(value: &Value) -> Result<MidiBlock, String> {
        serde_json::from_value(value.clone()).map_err(|e| format!("midi: {e}"))
    }

    /// The source, its default applied: `steps`, `file` or `effect`.
    pub fn source(&self) -> &str {
        match self.source.as_deref() {
            Some("notes") => "steps",
            Some(source) => source,
            None if self.file.is_some() => "file",
            None if self.effect.is_some() => "effect",
            None => "steps",
        }
    }

    pub fn tempo(&self) -> f64 {
        self.tempo
            .filter(|t| t.is_finite() && *t > 0.0)
            .unwrap_or(120.0)
            .clamp(20.0, 400.0)
    }

    pub fn time_signature(&self) -> [u32; 2] {
        match self.time_signature {
            Some([n, d]) if (1..=32).contains(&n) && [1, 2, 4, 8, 16, 32].contains(&d) => [n, d],
            _ => [4, 4],
        }
    }

    /// The loop as a bar range (1-based, `to` exclusive), `whole_bars`
    /// standing in for `loop: true`; None for no loop.
    pub fn loop_bars(&self, whole_bars: f64) -> Option<(f64, f64)> {
        match &self.looped {
            None | Some(MidiLoop::Whole(false)) => None,
            Some(MidiLoop::Whole(true)) => Some((1.0, 1.0 + whole_bars)),
            Some(MidiLoop::Range(r)) if r.from.is_finite() && r.to.is_finite() => {
                let from = r.from.max(1.0);
                (r.to > from).then_some((from, r.to))
            }
            Some(MidiLoop::Range(_)) => None,
        }
    }

    /// Whether the take is a seamless loop.
    pub fn is_loop(&self) -> bool {
        self.loop_bars(1.0).is_some()
    }

    /// The level trim in dB, clamped to ±24.
    pub fn level_db(&self) -> f64 {
        self.level
            .filter(|l| l.is_finite())
            .unwrap_or(0.0)
            .clamp(-24.0, 24.0)
    }
}

impl crate::ProjectResource {
    /// The resource's `midi` block, typed — None when it has none.
    pub fn midi(&self) -> Option<Result<MidiBlock, String>> {
        self.extra.get(KEY).map(MidiBlock::from_value)
    }
}

/// Whether the project has a MIDI-sequenced resource — content an older
/// lossless reader may simply ignore and keeps (rung 48).
pub fn carries_midi(meta: &crate::ProjectMetadata) -> bool {
    meta.resources
        .iter()
        .flatten()
        .any(|r| r.extra.contains_key(KEY))
}

/// The renderer's version (promo-midi's expansion and synthesis): part of
/// every receipt, bumped whenever the same block and bank could sound
/// different, so takes from an older renderer render again.
pub const RENDERER: u32 = 2;

/// The canonical encoding of a JSON value: object keys sorted, null
/// members dropped, every number written from its f64 value (an integral
/// one without a fraction), strings as serde_json escapes them. Two
/// writers that agree on the values agree on these bytes — Swift's `120`
/// and Rust's `120.0` included.
pub fn canonical(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            let x = n.as_f64().unwrap_or(0.0);
            if x.is_finite() && x.fract() == 0.0 && x.abs() < 9.0e15 {
                out.push_str(&format!("{}", x as i64));
            } else {
                out.push_str(&format!("{x}"));
            }
        }
        Value::String(s) => out.push_str(&Value::String(s.clone()).to_string()),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map
                .iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, _)| k)
                .collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*key).clone()).to_string());
                out.push(':');
                write_canonical(&map[*key], out);
            }
            out.push('}');
        }
    }
}

/// The receipt a take is rendered under, lowercase hex: SHA-256 over the
/// canonical block minus `renderedHash`, `bank` and `take` (what the
/// render writes), a zero byte, the
/// `.mid` bytes (when the source has a file), a zero byte, and the bank's
/// SHA-256 as hex text, a zero byte and [`RENDERER`]. The bank enters by its hash alone, so a renamed
/// bank is the same bank and a different file is a re-render.
pub fn receipt(block: &Value, mid: Option<&[u8]>, bank_sha256: &str) -> String {
    receipt_with_layers(block, mid, &[], bank_sha256)
}

/// A generated layer's bytes as the receipt takes them: `None` for a file
/// that is not there (so its return renders again).
pub type LayerBytes = Option<Vec<u8>>;

/// [`receipt`] for a block with generated layers: of each `generated`
/// only `file`, `highPass` and `trackName` count (the mix's high-pass
/// follows them; caption, seed, status and the rest made the file, they
/// do not change the take), and after [`RENDERER`] each layer's
/// bytes in track order — a zero byte, then the length and bytes, or
/// `0xFF` for a missing file. With no layers it is [`receipt`] exactly,
/// so a block without generated tracks keeps the receipt it had.
pub fn receipt_with_layers(
    block: &Value,
    mid: Option<&[u8]>,
    layers: &[LayerBytes],
    bank_sha256: &str,
) -> String {
    use sha2::Digest;
    let mut stripped = block.clone();
    if let Some(map) = stripped.as_object_mut() {
        map.remove("renderedHash");
        map.remove("bank");
        map.remove("take");
        if let Some(tracks) = map.get_mut("tracks").and_then(Value::as_array_mut) {
            for track in tracks {
                if let Some(generated) = track.get_mut("generated").and_then(Value::as_object_mut) {
                    generated
                        .retain(|k, _| matches!(k.as_str(), "file" | "highPass" | "trackName"));
                }
            }
        }
    }
    let mut hasher = sha2::Sha256::new();
    hasher.update(canonical(&stripped).as_bytes());
    hasher.update([0u8]);
    if let Some(bytes) = mid {
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    hasher.update([0u8]);
    hasher.update(bank_sha256.trim().to_ascii_lowercase().as_bytes());
    hasher.update([0u8]);
    hasher.update(RENDERER.to_le_bytes());
    for layer in layers {
        hasher.update([0u8]);
        match layer {
            Some(bytes) => {
                hasher.update((bytes.len() as u64).to_le_bytes());
                hasher.update(bytes);
            }
            None => hasher.update([0xFFu8]),
        }
    }
    hex(hasher.finalize().as_slice())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What a generated layer heard, as a hash (MIDI.md §7): the canonical
/// block with every generated track taken out and the mix left out — the
/// render's own keys, `level`, and each track's `volume`, `pan`, `mute`
/// and `solo` — then the `.mid` bytes of a slice-1 file block. Notes,
/// steps, programs, tempo, meter and loop count, so an edit to the music
/// marks the layer `contextChanged`; balancing the mix does not.
pub fn context_hash(block: &Value, mid: Option<&[u8]>) -> String {
    use sha2::Digest;
    let mut stripped = block.clone();
    if let Some(map) = stripped.as_object_mut() {
        for key in ["renderedHash", "bank", "take", "level"] {
            map.remove(key);
        }
        if let Some(tracks) = map.get_mut("tracks").and_then(Value::as_array_mut) {
            tracks.retain(|t| t.get("generated").is_none_or(Value::is_null));
            for track in tracks.iter_mut() {
                if let Some(track) = track.as_object_mut() {
                    for key in ["volume", "pan", "mute", "solo"] {
                        track.remove(key);
                    }
                }
            }
        }
    }
    let mut hasher = sha2::Sha256::new();
    hasher.update(canonical(&stripped).as_bytes());
    hasher.update([0u8]);
    if let Some(bytes) = mid {
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    hex(hasher.finalize().as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn block() -> Value {
        json!({
            "source": "steps", "tempo": 120, "timeSignature": [4, 4], "bars": 4,
            "swing": 0.25, "loop": {"from": 1, "to": 3, "fade": 1}, "futureKey": {"deep": [1, 2]},
            "tempoMap": [{"beat": 0, "bpm": 120, "ramp": "x"}, {"beat": 8, "bpm": 90}],
            "original": "Song.mid", "level": -3,
            "tracks": [{ "name": "Drums", "program": 0, "bank": "drums",
                "volume": 0.8, "pan": -0.5, "mute": false, "laneColour": "red",
                "steps": { "resolution": 16, "length": 16, "pager": 2,
                    "lanes": [{ "note": 36, "on": [0, 4, 8, 12], "velocity": 100, "accent": true }] },
                "notes": [{ "t": 0, "d": 0.5, "n": 60, "v": 100, "tie": "x" }],
                "controls": [{ "t": 1, "cc": 64, "v": 127, "lsb": 3 }, { "t": 2, "bend": 9000 }] }],
            "effect": { "preset": "rise", "pitch": 2, "length": 1.5, "program": 81, "curve": "exp" },
            "bank": { "name": "GeneralUser GS 2.0.3", "sha256": "ab", "build": 7 },
            "renderedHash": "cd"
        })
    }

    /// Keys this build does not model survive a read and a re-encode, at
    /// every depth the block has.
    #[test]
    fn unknown_keys_round_trip() {
        let original = block();
        let typed = MidiBlock::from_value(&original).unwrap();
        assert_eq!(typed.extra["futureKey"], json!({"deep": [1, 2]}));
        let track = &typed.tracks.as_ref().unwrap()[0];
        assert!(track.is_drums());
        assert_eq!(track.extra["laneColour"], "red");
        assert_eq!(track.steps.as_ref().unwrap().extra["pager"], 2);
        assert_eq!(track.steps.as_ref().unwrap().lanes[0].extra["accent"], true);
        assert_eq!(track.notes.as_ref().unwrap()[0].extra["tie"], "x");
        assert_eq!(typed.effect.as_ref().unwrap().extra["curve"], "exp");
        assert_eq!(track.controls.as_ref().unwrap()[0].extra["lsb"], 3);
        assert_eq!(typed.tempo_map.as_ref().unwrap()[0].extra["ramp"], "x");
        match typed.looped.as_ref().unwrap() {
            MidiLoop::Range(r) => assert_eq!(r.extra["fade"], 1),
            other => panic!("{other:?}"),
        }
        assert_eq!(typed.bank.as_ref().unwrap().extra["build"], 7);
        let back = serde_json::to_value(&typed).unwrap();
        assert_eq!(canonical(&back), canonical(&original));
    }

    /// The bag carries it through a resource's decode and encode, and the
    /// resource reads it typed.
    #[test]
    fn the_resource_keeps_and_reads_its_block() {
        let resource = json!({ "id": "M", "kind": "audio", "filename": "Loop.wav",
            "displayName": "Loop", "addedAt": 0, "midi": block() });
        let decoded: crate::ProjectResource = serde_json::from_value(resource.clone()).unwrap();
        let typed = decoded.midi().unwrap().unwrap();
        assert_eq!(typed.source(), "steps");
        assert_eq!(typed.tempo(), 120.0);
        let encoded = serde_json::to_value(&decoded).unwrap();
        assert_eq!(canonical(&encoded["midi"]), canonical(&block()));
    }

    #[test]
    fn canonical_is_sorted_and_spelling_blind() {
        let a = json!({"b": 120, "a": [0.5, null], "c": null});
        let b: Value = serde_json::from_str(r#"{"a":[0.5,null],"b":120.0}"#).unwrap();
        assert_eq!(canonical(&a), r#"{"a":[0.5,null],"b":120}"#);
        assert_eq!(canonical(&a), canonical(&b));
    }

    /// The receipt ignores what the render writes (the receipt itself and
    /// the bank's name) and follows everything that changes the sound.
    #[test]
    fn the_receipt_follows_the_sound() {
        let base = receipt(&block(), None, "AB");
        let mut written = block();
        written["renderedHash"] = json!("other");
        written["bank"]["name"] = json!("Renamed");
        assert_eq!(receipt(&written, None, "ab"), base);
        let mut edited = block();
        edited["tempo"] = json!(121);
        assert_ne!(receipt(&edited, None, "ab"), base);
        assert_ne!(receipt(&block(), Some(b"MThd"), "ab"), base);
        assert_ne!(receipt(&block(), None, "ac"), base);
    }

    fn with_layer() -> Value {
        let mut b = block();
        b["tracks"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "name": "Rhodes", "volume": 0.7,
            "generated": { "provider": "ace-step", "task": "lego", "caption": "warm Rhodes",
                "model": "acestep-v15-base", "seed": 1234, "range": { "from": 1, "to": 5 },
                "file": "Loop · Rhodes.wav", "contextHash": "ab", "status": "current",
                "steps": 32 } }));
        b
    }

    /// The generated track reads typed and keeps what it does not model.
    #[test]
    fn a_generated_track_round_trips() {
        let original = with_layer();
        let typed = MidiBlock::from_value(&original).unwrap();
        let track = &typed.tracks.as_ref().unwrap()[1];
        assert!(track.is_generated() && !typed.tracks.as_ref().unwrap()[0].is_generated());
        let g = track.generated.as_ref().unwrap();
        assert_eq!(g.seed, Some(1234));
        assert_eq!(g.range.as_ref().unwrap().to, 5.0);
        assert_eq!(g.status.as_deref(), Some(STATUS_CURRENT));
        assert_eq!(g.extra["steps"], 32);
        let back = serde_json::to_value(&typed).unwrap();
        assert_eq!(canonical(&back), canonical(&original));
    }

    /// Of a layer, the receipt follows the file and its bytes, not its
    /// record; without layers it is the plain receipt.
    #[test]
    fn the_receipt_follows_the_layer_bytes() {
        assert_eq!(
            receipt_with_layers(&block(), None, &[], "ab"),
            receipt(&block(), None, "ab")
        );
        let base = receipt_with_layers(&with_layer(), None, &[Some(b"RIFF1".to_vec())], "ab");
        let mut marked = with_layer();
        marked["tracks"][1]["generated"]["status"] = json!("contextChanged");
        marked["tracks"][1]["generated"]["caption"] = json!("other");
        assert_eq!(
            receipt_with_layers(&marked, None, &[Some(b"RIFF1".to_vec())], "ab"),
            base
        );
        assert_ne!(
            receipt_with_layers(&with_layer(), None, &[Some(b"RIFF2".to_vec())], "ab"),
            base
        );
        assert_ne!(
            receipt_with_layers(&with_layer(), None, &[None], "ab"),
            base
        );
        let mut renamed = with_layer();
        renamed["tracks"][1]["generated"]["file"] = json!("Other.wav");
        assert_ne!(
            receipt_with_layers(&renamed, None, &[Some(b"RIFF1".to_vec())], "ab"),
            base
        );
    }

    /// The context is the music without the layers and without the mix.
    #[test]
    fn the_context_is_the_music_not_the_mix() {
        let heard = context_hash(&block(), None);
        assert_eq!(
            context_hash(&with_layer(), None),
            heard,
            "layers are not context"
        );
        let mut mixed = block();
        mixed["tracks"][0]["volume"] = json!(0.2);
        mixed["tracks"][0]["mute"] = json!(true);
        mixed["level"] = json!(6);
        mixed["renderedHash"] = json!("x");
        assert_eq!(context_hash(&mixed, None), heard);
        let mut edited = block();
        edited["tracks"][0]["notes"][0]["n"] = json!(61);
        assert_ne!(context_hash(&edited, None), heard);
        let mut retimed = block();
        retimed["tempo"] = json!(100);
        assert_ne!(context_hash(&retimed, None), heard);
        assert_ne!(context_hash(&block(), Some(b"MThd")), heard);
    }

    fn project(resource: Value) -> crate::ProjectMetadata {
        let doc = json!({ "id": "P", "name": "T", "createdAt": 0, "state": "recorded",
            "subtitles": [], "trimStart": 0, "trimEnd": 0, "videoDuration": 0,
            "compositionSettings": {}, "resources": [resource] });
        crate::ProjectMetadata::from_json(&doc.to_string()).unwrap()
    }

    /// An ignorable field since the lossless reader: a MIDI resource
    /// stamps 48, not a rung of its own, and validate does not call the
    /// block unread (it is the apps' bag key).
    #[test]
    fn a_midi_resource_stamps_the_lossless_reader() {
        let plain = project(json!({ "id": "M", "kind": "audio", "filename": "",
            "displayName": "Loop", "addedAt": 0 }));
        assert!(plain.minimum_reader_version() < crate::LOSSLESS_READER);
        let midi = project(json!({ "id": "M", "kind": "audio", "filename": "",
            "displayName": "Loop", "addedAt": 0, "midi": { "source": "steps" } }));
        assert_eq!(midi.minimum_reader_version(), crate::LOSSLESS_READER);
        // A generated layer is one more ignorable field: still 48.
        let layered = project(json!({ "id": "M", "kind": "audio", "filename": "",
            "displayName": "Loop", "addedAt": 0, "midi": with_layer() }));
        assert_eq!(layered.minimum_reader_version(), crate::LOSSLESS_READER);
        assert!(crate::unread::APP_RESOURCE_KEYS.contains(&KEY));
    }

    /// The imported bytes in Resources/.midi/ are no resource: a `.mid`
    /// is not a media kind the inventory adopts (and the folder is not a
    /// file in the listing).
    #[test]
    fn the_inventory_ignores_the_midi_folder() {
        let meta = project(json!({ "id": "M", "kind": "audio", "filename": "Loop.wav",
            "displayName": "Loop", "addedAt": 0, "midi": { "file": "Song.mid" } }));
        let files = vec!["Loop.wav".to_string(), "Song.mid".to_string()];
        let resolved = crate::effective_resources(&meta, &files);
        assert_eq!(resolved.len(), 1, "{resolved:?}");
        assert_eq!(resolved[0].resource.id, "M");
    }

    #[test]
    fn defaults_follow_the_source() {
        let file = MidiBlock::from_value(&json!({"file": "Song.mid"})).unwrap();
        assert_eq!(file.source(), "file");
        assert!(!file.is_loop());
        let steps = MidiBlock::from_value(&json!({"source": "notes"})).unwrap();
        assert_eq!(steps.source(), "steps");
        assert!(!steps.is_loop(), "absent is no loop");
        assert_eq!(steps.time_signature(), [4, 4]);
        let whole = MidiBlock::from_value(&json!({"loop": true})).unwrap();
        assert_eq!(whole.loop_bars(4.0), Some((1.0, 5.0)));
        let range = MidiBlock::from_value(&json!({"loop": {"from": 2, "to": 4}})).unwrap();
        assert_eq!(range.loop_bars(8.0), Some((2.0, 4.0)));
        let backwards = MidiBlock::from_value(&json!({"loop": {"from": 4, "to": 2}})).unwrap();
        assert!(!backwards.is_loop());
        let level = MidiBlock::from_value(&json!({"level": -40})).unwrap();
        assert_eq!(level.level_db(), -24.0);
    }
}
