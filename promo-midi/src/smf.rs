//! An imported Standard MIDI File, read with midly: each channel's events
//! in seconds through the file's own tempo map, the channel's name (its
//! first track's name) and first program, the first tempo and meter.

use crate::score::{Event, Msg};
use midly::{MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};

/// One channel of the file that plays notes.
#[derive(Debug, Clone)]
pub struct Channel {
    pub channel: u8,
    pub name: Option<String>,
    /// The first program change before its first note, else 0.
    pub program: u8,
    /// The bank-select (CC0) in force at its first note, if any.
    pub bank: Option<u8>,
    pub notes: usize,
    pub events: Vec<Event>,
}

#[derive(Debug, Clone)]
pub struct File {
    pub channels: Vec<Channel>,
    /// Quarter notes per minute at the start.
    pub first_qpm: f64,
    /// How many tempo changes follow the first.
    pub tempo_changes: usize,
    pub time_signature: [u32; 2],
    /// The last channel event, in seconds.
    pub end: f64,
}

/// Reads `bytes` as an SMF (format 0, 1 or 2).
pub fn read(bytes: &[u8]) -> Result<File, String> {
    let smf = Smf::parse(bytes).map_err(|e| format!("not a MIDI file: {e}"))?;
    // Every event at its absolute tick, in file order: (tick, track, index).
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
            if let TrackEventKind::Meta(MetaMessage::TrackName(bytes)) = event.kind {
                if name.is_none() {
                    let text = String::from_utf8_lossy(bytes).trim().to_string();
                    if !text.is_empty() {
                        name = Some(text);
                    }
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
    // Stable: equal ticks keep file order (track, then position).
    raw.sort_by_key(|r| (r.tick, r.track));

    // Tempo map: ticks to seconds.
    let (ticks_per_quarter, fixed_rate) = match smf.header.timing {
        Timing::Metrical(tpq) => (f64::from(tpq.as_int().max(1)), None),
        Timing::Timecode(fps, sub) => (1.0, Some(f64::from(fps.as_f32()) * f64::from(sub.max(1)))),
    };
    let mut first_qpm = None;
    let mut tempo_changes = 0usize;
    let mut time_signature = None;
    let mut us_per_quarter = 500_000.0f64;
    let mut last_tick = 0u64;
    let mut seconds = 0.0f64;
    let mut channels: Vec<Channel> = Vec::new();
    let mut end = 0.0f64;
    let mut bank_state = [None::<u8>; 16];
    for r in &raw {
        let delta = (r.tick - last_tick) as f64;
        seconds += match fixed_rate {
            Some(rate) => delta / rate,
            None => delta * us_per_quarter / 1e6 / ticks_per_quarter,
        };
        last_tick = r.tick;
        match r.kind {
            TrackEventKind::Meta(MetaMessage::Tempo(t)) => {
                let us = f64::from(t.as_int().max(1));
                let qpm = 60e6 / us;
                match first_qpm {
                    // A tempo stated twice at the start is one tempo.
                    None => first_qpm = Some(qpm),
                    Some(_) if r.tick == 0 => first_qpm = Some(qpm),
                    Some(_) if (us - us_per_quarter).abs() > 0.5 => tempo_changes += 1,
                    Some(_) => {}
                }
                us_per_quarter = us;
            }
            TrackEventKind::Meta(MetaMessage::TimeSignature(n, d, _, _)) => {
                if time_signature.is_none() {
                    time_signature = Some([u32::from(n.max(1)), 1u32 << d.min(5)]);
                }
            }
            TrackEventKind::Midi { channel, message } => {
                let ch = channel.as_int();
                let msg = match message {
                    MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => Msg::On {
                        key: key.as_int(),
                        vel: vel.as_int(),
                    },
                    MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                        Msg::Off { key: key.as_int() }
                    }
                    MidiMessage::Controller { controller, value } => {
                        if controller.as_int() == 0 {
                            bank_state[usize::from(ch)] = Some(value.as_int());
                        }
                        Msg::Control(controller.as_int(), value.as_int())
                    }
                    MidiMessage::ProgramChange { program } => Msg::Program(program.as_int()),
                    MidiMessage::PitchBend { bend } => Msg::Bend(bend.0.as_int()),
                    // Aftertouch: rustysynth has no use for it.
                    _ => continue,
                };
                let at = match channels.iter().position(|c| c.channel == ch) {
                    Some(at) => at,
                    None => {
                        channels.push(Channel {
                            channel: ch,
                            name: None,
                            program: 0,
                            bank: None,
                            notes: 0,
                            events: Vec::new(),
                        });
                        channels.len() - 1
                    }
                };
                let entry = &mut channels[at];
                match msg {
                    Msg::On { .. } => {
                        if entry.notes == 0 {
                            entry.name = names.get(r.track).cloned().flatten();
                            entry.bank = bank_state[usize::from(ch)];
                        }
                        entry.notes += 1;
                    }
                    Msg::Program(p) if entry.notes == 0 => entry.program = p,
                    _ => {}
                }
                entry.events.push(Event { time: seconds, msg });
                end = end.max(seconds);
            }
            _ => {}
        }
    }
    channels.retain(|c| c.notes > 0);
    channels.sort_by_key(|c| c.channel);
    Ok(File {
        channels,
        first_qpm: first_qpm.unwrap_or(120.0),
        tempo_changes,
        time_signature: time_signature.unwrap_or([4, 4]),
        end,
    })
}
