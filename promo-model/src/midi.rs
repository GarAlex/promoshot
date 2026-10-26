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
//! `renderedHash` and `bank`, then the `.mid` bytes when there are any,
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
    /// The loop's length in bars (default 1 for steps; a file's own
    /// length rounded up to whole bars).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bars: Option<u32>,
    /// 0 straight … 1 a full triplet shuffle: every odd step is late by
    /// `swing / 3` of a step (a straight pair becomes 2:1). Steps only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swing: Option<f64>,
    /// A seamless loop of exactly `bars` (the tail folded onto the head);
    /// false renders a one-shot until its tail is silent. Default true for
    /// steps, false for files and effects.
    #[serde(rename = "loop", default, skip_serializing_if = "Option::is_none")]
    pub looped: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracks: Option<Vec<MidiTrack>>,
    /// The imported file's name in `Resources/.midi/`.
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
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl MidiTrack {
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

    /// Whether the take is a seamless loop.
    pub fn is_loop(&self) -> bool {
        self.looped.unwrap_or(self.source() == "steps")
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
pub const RENDERER: u32 = 1;

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
/// canonical block minus `renderedHash` and `bank`, a zero byte, the
/// `.mid` bytes (when the source has a file), a zero byte, and the bank's
/// SHA-256 as hex text, a zero byte and [`RENDERER`]. The bank enters by its hash alone, so a renamed
/// bank is the same bank and a different file is a re-render.
pub fn receipt(block: &Value, mid: Option<&[u8]>, bank_sha256: &str) -> String {
    use sha2::Digest;
    let mut stripped = block.clone();
    if let Some(map) = stripped.as_object_mut() {
        map.remove("renderedHash");
        map.remove("bank");
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
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn block() -> Value {
        json!({
            "source": "steps", "tempo": 120, "timeSignature": [4, 4], "bars": 4,
            "swing": 0.25, "loop": true, "futureKey": {"deep": [1, 2]},
            "tracks": [{ "name": "Drums", "program": 0, "bank": "drums",
                "volume": 0.8, "pan": -0.5, "mute": false, "laneColour": "red",
                "steps": { "resolution": 16, "length": 16, "pager": 2,
                    "lanes": [{ "note": 36, "on": [0, 4, 8, 12], "velocity": 100, "accent": true }] },
                "notes": [{ "t": 0, "d": 0.5, "n": 60, "v": 100, "tie": "x" }] }],
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
        assert!(steps.is_loop());
        assert_eq!(steps.time_signature(), [4, 4]);
    }
}
