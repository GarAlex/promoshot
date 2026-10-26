//! promo-midi end to end: expansion without a bank, and — with the bank
//! installed (`PROMOSHOT_SOUNDS`, `promoshot-mcp sounds install
//! generaluser`) — renders, loops, grids, receipts and imports. A test
//! that needs the bank says so and passes without it.

use std::path::{Path, PathBuf};

use promo_midi::{score, settle, sounds, Bank};
use promo_model::midi::MidiBlock;
use serde_json::{json, Value};

fn bank() -> Option<std::sync::Arc<Bank>> {
    match sounds::installed_default() {
        Some(path) => Some(Bank::open(&path).expect("the installed bank opens")),
        None => {
            eprintln!("SKIPPED: {}", sounds::missing_bank_message());
            None
        }
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "promo-midi-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Resources")).unwrap();
    dir
}

fn project(dir: &Path, midi: Value) -> Value {
    let doc = json!({
        "id": "P", "name": "T", "createdAt": 0, "state": "recorded", "subtitles": [],
        "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "compositionSettings": {},
        "resources": [{ "id": "M", "kind": "audio", "filename": "", "displayName": "Loop 1",
            "addedAt": 0, "midi": midi }],
    });
    std::fs::write(dir.join("metadata.json"), doc.to_string()).unwrap();
    doc
}

fn four_on_the_floor() -> Value {
    json!({
        "source": "steps", "tempo": 120, "timeSignature": [4, 4], "bars": 4, "loop": true,
        "tracks": [
            { "name": "Drums", "bank": "drums", "program": 0, "volume": 0.9,
              "steps": { "resolution": 16, "length": 16, "lanes": [
                { "note": 36, "on": [0, 4, 8, 12], "velocity": 110 },
                { "note": 42, "on": [2, 6, 10, 14], "velocity": 80 },
                { "note": 38, "on": [4, 12] } ] } },
            { "name": "Bass", "program": 33, "notes": [
                { "t": 0, "d": 1, "n": 36 }, { "t": 2, "d": 1, "n": 43 },
                { "t": 4, "d": 1, "n": 41 }, { "t": 14, "d": 2.5, "n": 36 } ] }
        ]
    })
}

// ------------------------------------------------------------ no bank needed

#[test]
fn steps_repeat_swing_and_stay_inside_the_loop() {
    let block = MidiBlock::from_value(&json!({
        "tempo": 120, "bars": 2, "swing": 1.0,
        "tracks": [{ "steps": { "resolution": 16, "length": 16,
            "lanes": [{ "note": 42, "on": [0, 1, 20] }] },
            "notes": [{ "t": 7.5, "d": 1, "n": 60 }, { "t": 8, "d": 1, "n": 62 }] }]
    }))
    .unwrap();
    let score = score::expand(&block, None).unwrap();
    assert!(score.looped);
    assert!((score.length - 4.0).abs() < 1e-12, "2 bars at 120 = 4 s");
    let mut ons: Vec<f64> = score.tracks[0]
        .events
        .iter()
        .filter(|e| matches!(e.msg, score::Msg::On { .. }))
        .map(|e| e.time)
        .collect();
    ons.sort_by(f64::total_cmp);
    // Step 0 and 1 in each bar (the 16-step pattern repeats), step 1 late by
    // a third of a step at full swing; step 20 is outside the pattern; the
    // note at beat 8 is past the loop.
    let step = 0.125;
    let expect = [0.0, step + step / 3.0, 2.0, 2.0 + step + step / 3.0, 3.75];
    assert_eq!(ons.len(), expect.len(), "{ons:?}");
    for (a, b) in ons.iter().zip(expect) {
        assert!((a - b).abs() < 1e-9, "{ons:?}");
    }
    assert!(
        score.notes.iter().any(|n| n.contains("2 note(s)")),
        "{:?}",
        score.notes
    );
}

#[test]
fn every_effect_preset_expands() {
    for preset in promo_midi::effects::PRESETS {
        let block = MidiBlock::from_value(&json!({
            "source": "effect", "effect": { "preset": preset.id, "pitch": 2, "length": 1.0 } }))
        .unwrap();
        let score = score::expand(&block, None).unwrap();
        assert!(!score.looped);
        assert_eq!(score.tracks.len(), 1);
        assert!(score.tracks[0].notes > 0, "{}", preset.id);
        assert!(
            score.tracks[0].events.iter().all(|e| e.time <= 1.0 + 1e-9),
            "{}",
            preset.id
        );
    }
    let unknown = MidiBlock::from_value(&json!({ "effect": { "preset": "boing" } })).unwrap();
    assert!(score::expand(&unknown, None).unwrap_err().contains("rise"));
}

/// A tiny SMF: a tempo map with one change, a 3/4 meter, a named piano
/// track and a drum track.
fn tiny_smf() -> Vec<u8> {
    use midly::num::{u15, u24, u28, u4, u7};
    use midly::{
        Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind,
    };
    let ev = |delta: u32, kind: TrackEventKind<'static>| TrackEvent {
        delta: u28::new(delta),
        kind,
    };
    let midi = |channel: u8, message: MidiMessage| TrackEventKind::Midi {
        channel: u4::new(channel),
        message,
    };
    let on = |key: u8, vel: u8| MidiMessage::NoteOn {
        key: u7::new(key),
        vel: u7::new(vel),
    };
    let off = |key: u8| MidiMessage::NoteOff {
        key: u7::new(key),
        vel: u7::new(0),
    };
    let conductor = vec![
        ev(
            0,
            TrackEventKind::Meta(MetaMessage::Tempo(u24::new(600_000))),
        ), // 100 qpm
        ev(
            0,
            TrackEventKind::Meta(MetaMessage::TimeSignature(3, 2, 24, 8)),
        ),
        ev(
            960,
            TrackEventKind::Meta(MetaMessage::Tempo(u24::new(500_000))),
        ), // 120 qpm
        ev(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
    ];
    let piano = vec![
        ev(0, TrackEventKind::Meta(MetaMessage::TrackName(b"Keys"))),
        ev(
            0,
            midi(
                0,
                MidiMessage::ProgramChange {
                    program: u7::new(4),
                },
            ),
        ),
        ev(0, midi(0, on(60, 100))),
        ev(480, midi(0, off(60))),
        ev(0, midi(0, on(64, 90))),
        ev(480, midi(0, off(64))),
        ev(0, midi(0, on(67, 90))),
        ev(480, midi(0, off(67))),
        ev(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
    ];
    let drums = vec![
        ev(0, midi(9, on(36, 120))),
        ev(240, midi(9, off(36))),
        ev(240, midi(9, on(38, 100))),
        ev(240, midi(9, off(38))),
        ev(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
    ];
    let smf = Smf {
        header: Header::new(Format::Parallel, Timing::Metrical(u15::new(480))),
        tracks: vec![conductor, piano, drums],
    };
    let mut bytes = Vec::new();
    smf.write_std(&mut bytes).unwrap();
    bytes
}

#[test]
fn a_mid_file_reads_its_tracks_tempo_and_meter() {
    let bytes = tiny_smf();
    let file = promo_midi::smf::read(&bytes).unwrap();
    assert_eq!(file.time_signature, [3, 4]);
    assert!((file.first_qpm - 100.0).abs() < 1e-9);
    assert_eq!(file.tempo_changes, 1);
    // Two quarters at 100 qpm (1.2 s), then one at 120 (0.5 s).
    assert!((file.end - 1.7).abs() < 1e-9, "{}", file.end);
    let tracks = score::file_tracks(&bytes).unwrap();
    assert_eq!(tracks.len(), 2);
    assert_eq!(tracks[0].name.as_deref(), Some("Keys"));
    assert_eq!(tracks[0].program, Some(4));
    assert_eq!(tracks[0].channel, Some(0));
    assert!(tracks[1].is_drums());
    assert_eq!(tracks[1].name.as_deref(), Some("Standard Kit"));

    let dir = scratch("import");
    let imported = promo_midi::import_mid(&dir.join("Resources"), "Song.mid", &bytes).unwrap();
    assert_eq!(imported.file, "Song.mid");
    assert!(
        imported.notes[0].starts_with("tempo changes"),
        "{:?}",
        imported.notes
    );
    assert_eq!(
        std::fs::read(dir.join("Resources/.midi/Song.mid")).unwrap(),
        bytes
    );
    // The same bytes again are the same file; other bytes get a new name.
    assert_eq!(
        promo_midi::import_mid(&dir.join("Resources"), "Song.mid", &bytes)
            .unwrap()
            .file,
        "Song.mid"
    );
    std::fs::write(dir.join("Resources/.midi/Other.mid"), b"someone else's").unwrap();
    let other = promo_midi::import_mid(&dir.join("Resources"), "Other.mid", &bytes).unwrap();
    assert_eq!(other.file, "Other 2.mid");
    assert!(promo_midi::import_mid(&dir.join("Resources"), "x.mid", b"not midi").is_err());
}

#[test]
fn stale_says_why_without_rendering() {
    let dir = scratch("stale");
    let doc = project(&dir, four_on_the_floor());
    let resources = dir.join("Resources");
    assert_eq!(
        settle::stale(&doc, &resources, "M", "ab")
            .unwrap()
            .as_deref(),
        Some("not rendered yet")
    );
    let mut rendered = doc.clone();
    rendered["resources"][0]["filename"] = json!("Loop 1.wav");
    assert!(settle::stale(&rendered, &resources, "M", "ab")
        .unwrap()
        .unwrap()
        .contains("missing"));
    std::fs::write(resources.join("Loop 1.wav"), b"x").unwrap();
    let print = promo_model::midi::receipt(&rendered["resources"][0]["midi"], None, "ab");
    rendered["resources"][0]["midi"]["renderedHash"] = json!(print);
    assert_eq!(
        settle::stale(&rendered, &resources, "M", "ab").unwrap(),
        None
    );
    rendered["resources"][0]["midi"]["tempo"] = json!(90);
    assert!(settle::stale(&rendered, &resources, "M", "ab")
        .unwrap()
        .is_some());
}

#[test]
fn the_wav_is_48k_stereo_24_bit() {
    let take = promo_midi::Take {
        left: vec![0.0, 1.0, -1.0],
        right: vec![0.5, 0.0, 0.0],
        peak_dbfs: 0.0,
        notes: vec![],
    };
    let wav = promo_midi::render::wav(&take);
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 48_000);
    assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), 24);
    assert_eq!(wav.len(), 44 + 3 * 2 * 3);
    assert_eq!(&wav[50..53], &[0xFF, 0xFF, 0x7F], "full scale is 2^23 - 1");
}

// --------------------------------------------------------------- bank tests

#[test]
fn the_same_block_renders_the_same_bytes() {
    let Some(bank) = bank() else { return };
    let block = MidiBlock::from_value(&four_on_the_floor()).unwrap();
    let score = score::expand(&block, None).unwrap();
    let a = promo_midi::render::wav(&promo_midi::render::render(&score, &bank).unwrap());
    let b = promo_midi::render::wav(&promo_midi::render::render(&score, &bank).unwrap());
    assert_eq!(a.len(), b.len());
    assert!(a == b, "two renders of one block differ");
}

#[test]
fn a_loop_is_exactly_its_bars_and_its_tail_wraps() {
    let Some(bank) = bank() else { return };
    // 4 bars of 4/4 at 120: 8 s, 384,000 frames, to the sample.
    let block = MidiBlock::from_value(&four_on_the_floor()).unwrap();
    let take = promo_midi::render::render(&score::expand(&block, None).unwrap(), &bank).unwrap();
    assert_eq!(take.frames(), 384_000);
    // 3 bars of 3/4 at 97: an awkward length, rounded once.
    let odd = MidiBlock::from_value(&json!({ "tempo": 97, "timeSignature": [3, 4], "bars": 3,
        "tracks": [{ "program": 0, "notes": [{ "t": 0, "d": 1, "n": 60 }] }] }))
    .unwrap();
    let take = promo_midi::render::render(&score::expand(&odd, None).unwrap(), &bank).unwrap();
    assert_eq!(
        take.frames(),
        (9.0 * 60.0 / 97.0 * 48_000.0f64).round() as usize
    );
    // A pad held over the loop's end: its sound is at the head, folded.
    let wrap = |looped: bool| {
        let block = MidiBlock::from_value(&json!({ "tempo": 120, "bars": 1, "loop": looped,
            "tracks": [{ "program": 89, "notes": [{ "t": 3, "d": 2, "n": 60 }] }] }))
        .unwrap();
        let take =
            promo_midi::render::render(&score::expand(&block, None).unwrap(), &bank).unwrap();
        take.left[..4_800].iter().fold(0f32, |m, x| m.max(x.abs()))
    };
    assert!(wrap(true) > 1e-3, "the tail wraps onto the head");
    assert!(wrap(false) < 1e-6, "a one-shot starts silent there");
}

#[test]
fn settle_writes_the_take_grid_and_receipt_then_reuses_it() {
    let Some(bank) = bank() else { return };
    let dir = scratch("settle");
    let mut midi = four_on_the_floor();
    midi["futureKey"] = json!({ "kept": true });
    project(&dir, midi);
    let first = settle::settle(&dir, "M", &bank).unwrap();
    assert!(!first.reused);
    assert_eq!(first.filename, "Loop 1.wav");
    assert!((first.duration - 8.0).abs() < 1e-12);
    let doc: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("metadata.json")).unwrap()).unwrap();
    let resource = &doc["resources"][0];
    assert_eq!(
        resource["beatGrid"],
        json!({ "bpm": 120.0, "offset": 0, "timeSignature": [4, 4] })
    );
    assert_eq!(resource["duration"], json!(8.0));
    assert_eq!(resource["midi"]["renderedHash"], json!(first.rendered_hash));
    assert_eq!(resource["midi"]["bank"]["sha256"], json!(bank.sha256));
    assert_eq!(
        resource["midi"]["futureKey"],
        json!({ "kept": true }),
        "unknown keys survive"
    );
    assert!(doc["minReaderVersion"].as_i64().unwrap() >= promo_model::LOSSLESS_READER);
    // The grid is stated: the beat detector never runs on this song.
    let meta = promo_model::ProjectMetadata::from_json(&doc.to_string()).unwrap();
    assert!(meta.resources.as_ref().unwrap()[0].beat_grid.is_some());
    let wav = dir.join("Resources/Loop 1.wav");
    let bytes = std::fs::read(&wav).unwrap();
    assert_eq!(bytes.len(), 44 + 384_000 * 6);

    // Unchanged: reused, nothing written.
    let again = settle::settle(&dir, "M", &bank).unwrap();
    assert!(again.reused);
    assert_eq!(again.rendered_hash, first.rendered_hash);
    assert_eq!(std::fs::read(&wav).unwrap(), bytes);
    assert_eq!(
        settle::stale(&doc, &dir.join("Resources"), "M", &bank.sha256).unwrap(),
        None
    );

    // Edited: rendered again under a new receipt.
    let mut edited = doc.clone();
    edited["resources"][0]["midi"]["tempo"] = json!(100);
    std::fs::write(dir.join("metadata.json"), edited.to_string()).unwrap();
    let third = settle::settle(&dir, "M", &bank).unwrap();
    assert!(!third.reused);
    assert_ne!(third.rendered_hash, first.rendered_hash);
    assert!((third.duration - 9.6).abs() < 1e-12);
}

#[test]
fn an_imported_file_settles_with_its_tracks() {
    let Some(bank) = bank() else { return };
    let dir = scratch("file");
    let imported = promo_midi::import_mid(&dir.join("Resources"), "Song.mid", &tiny_smf()).unwrap();
    project(&dir, json!({ "source": "file", "file": imported.file }));
    let settled = settle::settle(&dir, "M", &bank).unwrap();
    assert!(!settled.looped);
    assert_eq!(settled.tracks.len(), 2);
    assert_eq!(settled.tracks[0].program_name, "Electric Piano 1");
    assert_eq!(settled.tracks[1].family, "Drums");
    assert!(settled.notes.iter().any(|n| n.starts_with("tempo changes")));
    // 3/4 at 100 quarters a minute: 100 BPM in quarter beats.
    assert!((settled.bpm - 100.0).abs() < 1e-9);
    assert_eq!(settled.time_signature, [3, 4]);
    assert!(settled.duration >= 1.7, "{}", settled.duration);
    let doc: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("metadata.json")).unwrap()).unwrap();
    assert_eq!(doc["resources"][0]["midi"]["tracks"][0]["name"], "Keys");
    // Muting the drums is a mix edit: a new take.
    let mut edited = doc.clone();
    edited["resources"][0]["midi"]["tracks"][1]["mute"] = json!(true);
    std::fs::write(dir.join("metadata.json"), edited.to_string()).unwrap();
    let muted = settle::settle(&dir, "M", &bank).unwrap();
    assert!(!muted.reused && muted.tracks[1].muted);
}
