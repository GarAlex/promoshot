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
        "tempo": 120, "bars": 2, "swing": 1.0, "loop": true,
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
        score.notes.iter().any(|n| n.contains("1 note(s)")),
        "step 20 is outside its pattern: {:?}",
        score.notes
    );
}

fn ons(score: &score::Score, track: usize) -> Vec<f64> {
    let mut ons: Vec<f64> = score.tracks[track]
        .events
        .iter()
        .filter(|e| matches!(e.msg, score::Msg::On { .. }))
        .map(|e| e.time)
        .collect();
    ons.sort_by(f64::total_cmp);
    ons
}

/// A tempo map moves the notes with it; the grid keeps the first tempo
/// and says the tempo changes.
#[test]
fn notes_follow_the_tempo_map() {
    let block = MidiBlock::from_value(&json!({
        "tempo": 120, "tempoMap": [{ "beat": 0, "bpm": 120 }, { "beat": 4, "bpm": 60 }],
        "tracks": [{ "notes": [{ "t": 2, "d": 1, "n": 60 }, { "t": 6, "d": 1, "n": 62 }] }]
    }))
    .unwrap();
    let score = score::expand(&block, None).unwrap();
    assert!(!score.looped, "no loop: a one-shot");
    assert_eq!(
        ons(&score, 0),
        [1.0, 4.0],
        "beat 4 is 2 s, then a beat a second"
    );
    assert_eq!(score.bpm, 120.0);
    assert!(
        score.notes.iter().any(|n| n.starts_with("tempo changes")),
        "{:?}",
        score.notes
    );
    assert!((score.length - 5.0).abs() < 1e-9, "the last release");
}

/// `loop: {from, to}` renders exactly those bars: notes before or after
/// are left out, a control set before the range holds at the head, and
/// a note held over the end keeps its release (the render folds it).
#[test]
fn a_loop_range_is_exactly_its_bars() {
    let block = MidiBlock::from_value(&json!({
        "tempo": 120, "loop": { "from": 2, "to": 3 },
        "tracks": [{ "program": 0,
            "controls": [{ "t": 1, "cc": 7, "v": 90 }, { "t": 5, "cc": 64, "v": 127 }],
            "notes": [{ "t": 0, "d": 1, "n": 60 }, { "t": 4, "d": 1, "n": 62 },
                      { "t": 7, "d": 2, "n": 64 }, { "t": 8, "d": 1, "n": 65 }] }]
    }))
    .unwrap();
    let score = score::expand(&block, None).unwrap();
    assert!(score.looped);
    assert!((score.length - 2.0).abs() < 1e-12, "one bar at 120");
    assert_eq!(ons(&score, 0), [0.0, 1.5]);
    let events = &score.tracks[0].events;
    assert!(
        events
            .iter()
            .any(|e| e.time == 0.0 && e.msg == score::Msg::Control(7, 90)),
        "{events:?}"
    );
    assert!(events
        .iter()
        .any(|e| (e.time - 0.5).abs() < 1e-9 && e.msg == score::Msg::Control(64, 127)));
    assert!(
        events
            .iter()
            .any(|e| (e.time - 2.5).abs() < 1e-9 && e.msg == score::Msg::Off { key: 64 }),
        "the held note's release is past the end, to fold"
    );
    let whole = MidiBlock::from_value(&json!({ "loop": true, "bars": 2,
        "tracks": [{ "notes": [{ "t": 0, "d": 1, "n": 60 }] }] }))
    .unwrap();
    assert!(
        (score::expand(&whole, None).unwrap().length - 4.0).abs() < 1e-12,
        "`true` is the whole song"
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
    assert!((file.first_bpm() - 100.0).abs() < 1e-9);
    assert_eq!(file.tempo_changes, 1);
    assert_eq!(file.tempo_map[1].beat, 2.0);
    assert_eq!(file.tempo_map[1].bpm, 120.0);
    assert_eq!(file.end_beats, 3.0);
    let tracks = score::file_tracks(&bytes).unwrap();
    assert_eq!(tracks.len(), 2);
    assert_eq!(tracks[0].name.as_deref(), Some("Keys"));
    assert_eq!(tracks[0].program, Some(4));
    assert_eq!(tracks[0].channel, Some(0));
    let keys = tracks[0].notes.as_ref().unwrap();
    assert_eq!(keys.len(), 3, "notes inline, in beats");
    assert_eq!(
        (keys[1].t, keys[1].d, keys[1].n, keys[1].v),
        (1.0, 1.0, 64, Some(90))
    );
    assert!(tracks[1].is_drums());
    assert_eq!(tracks[1].name.as_deref(), Some("Standard Kit"));

    let dir = scratch("import");
    let imported = promo_midi::import_mid(&dir.join("Resources"), "Song.mid", &bytes).unwrap();
    assert_eq!(imported.original, "Song.mid");
    assert_eq!(imported.bars, 1);
    let block = imported.block();
    assert_eq!(block["source"], "notes");
    assert_eq!(block["original"], "Song.mid");
    assert_eq!(block["tempoMap"].as_array().unwrap().len(), 2);
    assert!(block.get("loop").is_none(), "an import starts as no loop");
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
            .original,
        "Song.mid"
    );
    std::fs::write(dir.join("Resources/.midi/Other.mid"), b"someone else's").unwrap();
    let other = promo_midi::import_mid(&dir.join("Resources"), "Other.mid", &bytes).unwrap();
    assert_eq!(other.original, "Other 2.mid");
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
        loudness: None,
        gain_db: 0.0,
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
    let odd = MidiBlock::from_value(
        &json!({ "tempo": 97, "timeSignature": [3, 4], "bars": 3, "loop": true,
        "tracks": [{ "program": 0, "notes": [{ "t": 0, "d": 1, "n": 60 }] }] }),
    )
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
    assert_eq!(resource["looped"], true, "one Loop: the take loops");
    assert!(resource["midi"]["take"]["loudness"].is_number());
    assert!(first.loudness.is_some() && first.gain_db.is_some());
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
    assert_eq!(
        again.gain_db,
        first.gain_db.map(|g| (g * 100.0).round() / 100.0),
        "the record answers a reuse"
    );
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

/// A slice-1 `file` block (no notes inline) still renders, expanded from
/// its `.mid` on read — the same take an import's v2 block renders.
#[test]
fn a_slice_one_file_block_and_its_import_sound_the_same() {
    let Some(bank) = bank() else { return };
    let dir = scratch("file");
    let imported = promo_midi::import_mid(&dir.join("Resources"), "Song.mid", &tiny_smf()).unwrap();
    project(&dir, json!({ "source": "file", "file": imported.original }));
    let settled = settle::settle(&dir, "M", &bank).unwrap();
    assert!(!settled.looped);
    assert_eq!(settled.tracks.len(), 2);
    assert_eq!(settled.tracks[0].program_name, "Electric Piano 1");
    assert_eq!(settled.tracks[1].family, "Drums");
    assert!(settled.notes.iter().any(|n| n.starts_with("tempo changes")));
    assert!((settled.bpm - 100.0).abs() < 1e-9);
    assert_eq!(settled.time_signature, [3, 4]);
    let legacy = std::fs::read(dir.join("Resources/Loop 1.wav")).unwrap();

    let v2 = scratch("file-v2");
    promo_midi::import_mid(&v2.join("Resources"), "Song.mid", &tiny_smf()).unwrap();
    project(&v2, imported.block());
    let again = settle::settle(&v2, "M", &bank).unwrap();
    assert_eq!(again.tracks.len(), 2);
    assert!(
        std::fs::read(v2.join("Resources/Loop 1.wav")).unwrap() == legacy,
        "one render"
    );

    // Editing a note is an edit of the block, never of the .mid.
    let mut doc: Value =
        serde_json::from_str(&std::fs::read_to_string(v2.join("metadata.json")).unwrap()).unwrap();
    doc["resources"][0]["midi"]["tracks"][0]["notes"][0]["n"] = json!(48);
    doc["resources"][0]["midi"]["tracks"][1]["mute"] = json!(true);
    std::fs::write(v2.join("metadata.json"), doc.to_string()).unwrap();
    let edited = settle::settle(&v2, "M", &bank).unwrap();
    assert!(!edited.reused && edited.tracks[1].muted);
    assert_eq!(
        std::fs::read(v2.join("Resources/.midi/Song.mid")).unwrap(),
        tiny_smf()
    );
}

/// Every take lands on its loudness target (music −16 LUFS, effects
/// −20), `level` on top, under −1 dBTP; the result says what it did.
#[test]
fn takes_are_normalised_with_a_true_peak_ceiling() {
    let Some(bank) = bank() else { return };
    let render = |midi: Value| {
        let block = MidiBlock::from_value(&midi).unwrap();
        promo_midi::render::render(&score::expand(&block, None).unwrap(), &bank).unwrap()
    };
    let pad = json!({ "tempo": 120, "loop": true, "bars": 2, "tracks": [{ "program": 89,
        "notes": [{ "t": 0, "d": 8, "n": 60 }, { "t": 0, "d": 8, "n": 67 }] }] });
    let music = render(pad.clone());
    assert!(
        (music.loudness.unwrap() + 16.0).abs() < 0.3,
        "{:?}",
        music.loudness
    );
    assert!(
        music.gain_db > 0.0,
        "the bank renders quiet: {}",
        music.gain_db
    );
    let mut trimmed = pad;
    trimmed["level"] = json!(-6);
    assert!((render(trimmed).loudness.unwrap() + 22.0).abs() < 0.3);
    let coin = render(json!({ "source": "effect", "effect": { "preset": "coin" } }));
    let peak =
        promo_midi::render::true_peak(&coin.left).max(promo_midi::render::true_peak(&coin.right));
    assert!(20.0 * f64::from(peak).log10() <= -0.99, "true peak {peak}");
    let target = coin.loudness.unwrap();
    assert!(target <= -19.7, "an effect sits under music: {target}");
    // Loud drums meet the ceiling before the target, and say so.
    let drums = render(four_on_the_floor());
    let peak =
        promo_midi::render::true_peak(&drums.left).max(promo_midi::render::true_peak(&drums.right));
    assert!(20.0 * f64::from(peak).log10() <= -0.99);
}

/// Only what a layer uses is settled — a nested composition's layer
/// counts — and validate speaks for those alone.
#[test]
fn settle_used_walks_layers_and_nested_compositions() {
    let Some(bank) = bank() else { return };
    let dir = scratch("used");
    let midi = json!({ "tracks": [{ "notes": [{ "t": 0, "d": 1, "n": 60 }] }] });
    let audio = |id: &str| {
        json!({ "id": id, "kind": "audio", "filename": "", "displayName": id,
        "addedAt": 0, "midi": midi })
    };
    let mut doc = json!({
        "id": "P", "name": "T", "createdAt": 0, "state": "recorded", "subtitles": [],
        "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "compositionSettings": {},
        "layers": [{ "id": "c", "name": "C", "sortIndex": 0, "kind": "video", "isEnabled": true,
            "startTime": 0, "duration": 2, "resourceID": "comp", "keyframes": [] }],
        "resources": [audio("inner"), audio("unused"),
            { "id": "comp", "kind": "composition", "filename": "", "displayName": "comp",
              "addedAt": 0, "composition": { "layers": [{ "id": "a", "name": "A", "sortIndex": 0,
                "kind": "audio", "isEnabled": true, "startTime": 0, "duration": 2,
                "resourceID": "inner", "keyframes": [] }] } }],
    });
    assert_eq!(promo_midi::used_midi(&doc), ["inner"]);
    let findings = promo_midi::findings(&doc, &dir.join("Resources"));
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(findings[0].contains("\"inner\""));
    let results = promo_midi::settle_used(&mut doc, &dir.join("Resources"), &bank);
    assert_eq!(results.len(), 1);
    assert!(results[0].1.is_ok());
    assert!(dir.join("Resources/inner.wav").is_file());
    assert!(!dir.join("Resources/unused.wav").exists());
    assert!(promo_midi::findings(&doc, &dir.join("Resources")).is_empty());
}

/// A preview is the settle's render, written where it is asked and
/// nowhere else; the project is untouched.
#[test]
fn a_preview_renders_into_the_cache_only() {
    let Some(bank) = bank() else { return };
    let dir = scratch("preview");
    let doc = project(&dir, four_on_the_floor());
    let out = dir.join("cache/preview.wav");
    let result = promo_midi::preview(&doc, &dir.join("Resources"), "M", &bank, &out).unwrap();
    assert!(out.is_file());
    assert!((result.duration - 8.0).abs() < 1e-12);
    assert!(result.loudness.is_some() && result.gain_db.is_some());
    assert_eq!(std::fs::read_dir(dir.join("Resources")).unwrap().count(), 0);
    let mut settled = doc.clone();
    settle::settle_doc(&mut settled, &dir.join("Resources"), "M", &bank).unwrap();
    assert!(
        std::fs::read(&out).unwrap() == std::fs::read(dir.join("Resources/Loop 1.wav")).unwrap()
    );
    assert_eq!(settled["resources"][0]["looped"], true);
}
