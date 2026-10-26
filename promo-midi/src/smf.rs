//! An imported Standard MIDI File, read with midly into the block's own
//! shape (MIDI.md UX v2 §1): one track per channel that plays notes, its
//! notes in beats, its controller moves, bends and later program changes
//! as `controls`, and the file's tempo map in beats. A beat is one count
//! of the file's (first) meter, so a 6/8 file counts eighths.

use midly::{MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};
use promo_model::midi::{MidiControl, MidiNote, MidiTempo, MidiTrack};
use serde_json::json;

#[derive(Debug, Clone)]
pub struct File {
    pub tracks: Vec<MidiTrack>,
    /// `[{beat, bpm}]`, the first at beat 0.
    pub tempo_map: Vec<MidiTempo>,
    /// How many tempo changes follow the first.
    pub tempo_changes: usize,
    pub time_signature: [u32; 2],
    /// The last channel event, in beats.
    pub end_beats: f64,
}

impl File {
    /// The first tempo, in beats a minute.
    pub fn first_bpm(&self) -> f64 {
        self.tempo_map.first().map_or(120.0, |t| t.bpm)
    }
}

fn tidy(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

/// Reads `bytes` as an SMF (format 0, 1 or 2).
pub fn read(bytes: &[u8]) -> Result<File, String> {
    let smf = Smf::parse(bytes).map_err(|e| format!("not a MIDI file: {e}"))?;
    struct Raw<'a> {
        tick: u64,
        track: usize,
        kind: TrackEventKind<'a>,
    }
    let mut raw: Vec<Raw> = Vec::new();
    let mut names: Vec<Option<String>> = Vec::new();
    for (track, events) in smf.tracks.iter().enumerate() {
        let mut tick = 0u64;
        let mut name = None;
        for event in events {
            tick += u64::from(event.delta.as_int());
            if let TrackEventKind::Meta(MetaMessage::TrackName(text)) = event.kind {
                let text = String::from_utf8_lossy(text).trim().to_string();
                if name.is_none() && !text.is_empty() {
                    name = Some(text);
                }
            }
            raw.push(Raw {
                tick,
                track,
                kind: event.kind,
            });
        }
        names.push(name);
    }
    raw.sort_by_key(|r| (r.tick, r.track));

    // The meter first: it says what a beat is.
    let time_signature = raw
        .iter()
        .find_map(|r| match r.kind {
            TrackEventKind::Meta(MetaMessage::TimeSignature(n, d, _, _)) => {
                Some([u32::from(n.max(1)), 1u32 << d.min(5)])
            }
            _ => None,
        })
        .unwrap_or([4, 4]);
    let per_quarter = f64::from(time_signature[1]) / 4.0;
    let beat_of = |tick: u64| -> f64 {
        match smf.header.timing {
            Timing::Metrical(tpq) => tick as f64 / f64::from(tpq.as_int().max(1)) * per_quarter,
            // SMPTE time: seconds, counted at 120 quarters a minute.
            Timing::Timecode(fps, sub) => {
                tick as f64 / (f64::from(fps.as_f32()) * f64::from(sub.max(1))) * 2.0 * per_quarter
            }
        }
    };

    let mut tempo_map: Vec<MidiTempo> = Vec::new();
    // Per channel: the track being built, keys sounding (FIFO per key).
    struct Building {
        channel: u8,
        name: Option<String>,
        program: Option<u8>,
        bank: Option<u8>,
        pan: Option<u8>,
        notes: Vec<MidiNote>,
        controls: Vec<MidiControl>,
        sounding: Vec<(u8, f64, u8)>,
    }
    let mut channels: Vec<Building> = Vec::new();
    let mut end = 0.0f64;
    for r in &raw {
        let beat = beat_of(r.tick);
        match r.kind {
            TrackEventKind::Meta(MetaMessage::Tempo(t)) => {
                if matches!(smf.header.timing, Timing::Timecode(..)) {
                    continue;
                }
                let bpm = tidy(60e6 / f64::from(t.as_int().max(1)) * per_quarter);
                match tempo_map.last_mut() {
                    Some(last) if (last.beat - beat).abs() < 1e-9 => last.bpm = bpm,
                    Some(last) if (last.bpm - bpm).abs() < 1e-6 => {}
                    _ => tempo_map.push(MidiTempo {
                        beat: tidy(beat),
                        bpm,
                        ..Default::default()
                    }),
                }
            }
            TrackEventKind::Midi { channel, message } => {
                let ch = channel.as_int();
                let at = match channels.iter().position(|c| c.channel == ch) {
                    Some(at) => at,
                    None => {
                        channels.push(Building {
                            channel: ch,
                            name: None,
                            program: None,
                            bank: None,
                            pan: None,
                            notes: Vec::new(),
                            controls: Vec::new(),
                            sounding: Vec::new(),
                        });
                        channels.len() - 1
                    }
                };
                let c = &mut channels[at];
                let before_notes = c.notes.is_empty() && c.sounding.is_empty();
                let control = |cc: Option<u8>,
                               v: Option<u8>,
                               bend: Option<u16>,
                               program: Option<u8>| MidiControl {
                    t: tidy(beat),
                    cc,
                    v,
                    bend,
                    program,
                    ..Default::default()
                };
                match message {
                    MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                        if before_notes && c.name.is_none() {
                            c.name = names.get(r.track).cloned().flatten();
                        }
                        c.sounding.push((key.as_int(), beat, vel.as_int()));
                    }
                    MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                        if let Some(i) = c.sounding.iter().position(|s| s.0 == key.as_int()) {
                            let (n, start, v) = c.sounding.remove(i);
                            c.notes.push(MidiNote {
                                t: tidy(start),
                                d: tidy((beat - start).max(1.0 / 960.0)),
                                n,
                                v: Some(v),
                                ..Default::default()
                            });
                        }
                    }
                    // Before the first note: the track's own settings.
                    MidiMessage::ProgramChange { program } if before_notes => {
                        c.program = Some(program.as_int())
                    }
                    MidiMessage::ProgramChange { program } => {
                        c.controls
                            .push(control(None, None, None, Some(program.as_int())))
                    }
                    MidiMessage::Controller { controller, value } => {
                        let (cc, v) = (controller.as_int(), value.as_int());
                        match cc {
                            0 if before_notes => c.bank = Some(v),
                            10 if before_notes => c.pan = Some(v),
                            _ => c.controls.push(control(Some(cc), Some(v), None, None)),
                        }
                    }
                    MidiMessage::PitchBend { bend } => {
                        c.controls
                            .push(control(None, None, Some(bend.0.as_int()), None))
                    }
                    _ => continue,
                }
                end = end.max(beat);
            }
            _ => {}
        }
    }
    if tempo_map.first().is_none_or(|t| t.beat > 0.0) {
        tempo_map.insert(
            0,
            MidiTempo {
                beat: 0.0,
                bpm: tidy(120.0 * per_quarter),
                ..Default::default()
            },
        );
    }
    let tempo_changes = tempo_map.len() - 1;
    let mut tracks = Vec::new();
    channels.sort_by_key(|c| c.channel);
    for mut c in channels {
        // A note never released ends with the file.
        for (n, start, v) in c.sounding.drain(..) {
            c.notes.push(MidiNote {
                t: tidy(start),
                d: tidy((end - start).max(1.0 / 960.0)),
                n,
                v: Some(v),
                ..Default::default()
            });
        }
        if c.notes.is_empty() {
            continue;
        }
        c.notes
            .sort_by(|a, b| a.t.total_cmp(&b.t).then(a.n.cmp(&b.n)));
        let drums = c.channel == 9;
        let program = c.program.unwrap_or(0);
        tracks.push(MidiTrack {
            name: Some(c.name.unwrap_or_else(|| {
                if drums {
                    format!("{} Kit", crate::gm::kit_name(program))
                } else {
                    crate::gm::program_name(program).to_string()
                }
            })),
            program: Some(program),
            bank: if drums {
                Some(json!("drums"))
            } else {
                c.bank.filter(|b| *b > 0).map(|b| json!(b))
            },
            pan: c.pan.map(|p| {
                let p = f64::from(p) - 64.0;
                tidy(if p >= 0.0 { p / 63.0 } else { p / 64.0 })
            }),
            channel: Some(c.channel),
            notes: Some(c.notes),
            controls: (!c.controls.is_empty()).then_some(c.controls),
            ..Default::default()
        });
    }
    Ok(File {
        tracks,
        tempo_map,
        tempo_changes,
        time_signature,
        end_beats: tidy(end),
    })
}
