//! The block expanded to what the synth plays: tracks of timed MIDI
//! messages in seconds, with each track's instrument and mix, the take's
//! length and its beat grid.

use promo_model::midi::{MidiBlock, MidiTrack};

/// A channel message.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Msg {
    On {
        key: u8,
        vel: u8,
    },
    Off {
        key: u8,
    },
    Program(u8),
    Control(u8, u8),
    /// 0–16383, 8192 centre.
    Bend(u16),
}

impl Msg {
    /// At one instant: controls and programs first, then releases, then
    /// strikes — so a key struck again where it ends sounds twice.
    pub(crate) fn rank(&self) -> u8 {
        match self {
            Msg::Program(_) | Msg::Control(..) | Msg::Bend(_) => 0,
            Msg::Off { .. } => 1,
            Msg::On { .. } => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Event {
    /// Seconds from the take's start.
    pub time: f64,
    pub msg: Msg,
}

/// One track as the synth plays it, on a synthesizer of its own.
#[derive(Debug, Clone)]
pub struct Track {
    pub name: String,
    /// 9 for percussion, else the file's channel or 0.
    pub channel: u8,
    pub program: u8,
    pub drums: bool,
    pub gain: f32,
    pub muted: bool,
    pub notes: usize,
    pub events: Vec<Event>,
}

#[derive(Debug, Clone)]
pub struct Score {
    pub tracks: Vec<Track>,
    pub looped: bool,
    /// A loop's exact length; a one-shot's last event.
    pub length: f64,
    pub bpm: f64,
    pub time_signature: [u32; 2],
    /// What the take did not keep, in words.
    pub notes: Vec<String>,
}

fn pan_cc(pan: f64) -> u8 {
    (((pan.clamp(-1.0, 1.0) + 1.0) / 2.0) * 127.0).round() as u8
}

fn gain(track: &MidiTrack) -> f32 {
    track
        .volume
        .filter(|v| v.is_finite())
        .unwrap_or(1.0)
        .clamp(0.0, 2.0) as f32
}

fn muted(tracks: &[MidiTrack], track: &MidiTrack) -> bool {
    if tracks.iter().any(|t| t.solo == Some(true)) {
        track.solo != Some(true)
    } else {
        track.mute == Some(true)
    }
}

/// The opening messages of a generated track: bank, program, pan.
fn opening(track: &MidiTrack, drums: bool, program: u8) -> Vec<Event> {
    let mut events = Vec::new();
    if !drums {
        if let Some(bank) = track.bank_number() {
            events.push(Event {
                time: 0.0,
                msg: Msg::Control(0, bank),
            });
        }
    }
    events.push(Event {
        time: 0.0,
        msg: Msg::Program(program),
    });
    if let Some(pan) = track.pan.filter(|p| p.is_finite()) {
        events.push(Event {
            time: 0.0,
            msg: Msg::Control(10, pan_cc(pan)),
        });
    }
    events
}

/// Expands the block. `mid` is the imported file's bytes for a `file`
/// source.
pub fn expand(block: &MidiBlock, mid: Option<&[u8]>) -> Result<Score, String> {
    match block.source() {
        "steps" => Ok(steps(block)),
        "effect" => effect(block),
        "file" => file(block, mid.ok_or("midi: a file source with no .mid bytes")?),
        other => Err(format!(
            "midi: unknown source \"{other}\" — steps, notes, file or effect"
        )),
    }
}

fn steps(block: &MidiBlock) -> Score {
    let tempo = block.tempo();
    let ts = block.time_signature();
    let beat = 60.0 / tempo;
    let bar_beats = f64::from(ts[0]);
    let tracks: &[MidiTrack] = block.tracks.as_deref().unwrap_or(&[]);
    let mut notes_out = Vec::new();
    // Bars: stated, else what the content reaches.
    let content_beats = tracks
        .iter()
        .map(|t| {
            let steps = t.steps.as_ref().map_or(0.0, |s| {
                let res = f64::from(s.resolution.unwrap_or(16).max(1));
                s.lanes
                    .iter()
                    .flat_map(|l| l.on.iter())
                    .map(|&i| (f64::from(i) + 1.0) * bar_beats / res)
                    .fold(0.0, f64::max)
            });
            let notes = t
                .notes
                .iter()
                .flatten()
                .map(|n| n.t + n.d.max(0.0))
                .fold(0.0, f64::max);
            steps.max(notes)
        })
        .fold(0.0, f64::max);
    let bars = block
        .bars
        .filter(|b| *b > 0)
        .unwrap_or_else(|| ((content_beats / bar_beats).ceil() as u32).max(1))
        .min(512);
    let loop_beats = f64::from(bars) * bar_beats;
    let looped = block.is_loop();
    let swing = block
        .swing
        .filter(|s| s.is_finite())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let mut dropped = 0usize;
    let mut skipped = 0usize;
    let mut out = Vec::new();
    for track in tracks {
        let drums = track.is_drums();
        let program = track.program.unwrap_or(0).min(127);
        let mut events = opening(track, drums, program);
        let mut count = 0usize;
        let mut strike = |t: f64, d: f64, key: u8, vel: u8, events: &mut Vec<Event>| {
            if t < 0.0 || !t.is_finite() || (looped && t >= loop_beats - 1e-9) {
                dropped += 1;
                return;
            }
            let d = if d.is_finite() && d > 0.0 {
                d
            } else {
                1.0 / 64.0
            };
            let key = key.min(127);
            events.push(Event {
                time: t * beat,
                msg: Msg::On {
                    key,
                    vel: vel.clamp(1, 127),
                },
            });
            events.push(Event {
                time: (t + d) * beat,
                msg: Msg::Off { key },
            });
            count += 1;
        };
        if let Some(grid) = &track.steps {
            let res = grid.resolution.unwrap_or(16).clamp(1, 192);
            let step = bar_beats / f64::from(res);
            let total = bars * res;
            let cycle = grid.length.filter(|l| *l > 0).unwrap_or(total);
            for lane in &grid.lanes {
                let vel = lane.velocity.unwrap_or(100);
                let hold = lane
                    .hold
                    .filter(|h| h.is_finite() && *h > 0.0)
                    .unwrap_or(1.0);
                for &on in &lane.on {
                    if on >= cycle {
                        skipped += 1;
                        continue;
                    }
                    let mut index = on;
                    while index < total {
                        let late = if index % 2 == 1 {
                            swing * step / 3.0
                        } else {
                            0.0
                        };
                        strike(
                            f64::from(index) * step + late,
                            hold * step,
                            lane.note,
                            vel,
                            &mut events,
                        );
                        index += cycle;
                    }
                }
            }
        }
        for note in track.notes.iter().flatten() {
            strike(note.t, note.d, note.n, note.v.unwrap_or(100), &mut events);
        }
        out.push(Track {
            name: track_name(track, drums, program),
            channel: if drums { 9 } else { 0 },
            program,
            drums,
            gain: gain(track),
            muted: muted(tracks, track),
            notes: count,
            events,
        });
    }
    dropped += skipped;
    if dropped > 0 {
        notes_out.push(format!(
            "{dropped} note(s) outside the loop's {bars} bar(s) were left out"
        ));
    }
    let length = if looped {
        loop_beats * beat
    } else {
        last_event(&out)
    };
    Score {
        tracks: out,
        looped,
        length,
        bpm: tempo,
        time_signature: ts,
        notes: notes_out,
    }
}

fn track_name(track: &MidiTrack, drums: bool, program: u8) -> String {
    track.name.clone().unwrap_or_else(|| {
        if drums {
            format!("{} Kit", crate::gm::kit_name(program))
        } else {
            crate::gm::program_name(program).to_string()
        }
    })
}

fn last_event(tracks: &[Track]) -> f64 {
    tracks
        .iter()
        .flat_map(|t| t.events.iter())
        .map(|e| e.time)
        .fold(0.0, f64::max)
}

fn effect(block: &MidiBlock) -> Result<Score, String> {
    let effect = block
        .effect
        .as_ref()
        .ok_or("midi: an effect source with no `effect`")?;
    let (program, mut events, length) = crate::effects::expand(effect)?;
    let template = block
        .tracks
        .as_deref()
        .and_then(|t| t.first())
        .cloned()
        .unwrap_or_default();
    let mut all = opening(&template, false, program);
    all.append(&mut events);
    let notes = all
        .iter()
        .filter(|e| matches!(e.msg, Msg::On { .. }))
        .count();
    let looped = block.looped.unwrap_or(false);
    let tempo = block.tempo();
    let ts = block.time_signature();
    let track = Track {
        name: template
            .name
            .clone()
            .unwrap_or_else(|| crate::gm::program_name(program).to_string()),
        channel: 0,
        program,
        drums: false,
        gain: gain(&template),
        muted: template.mute == Some(true),
        notes,
        events: all,
    };
    Ok(Score {
        tracks: vec![track],
        looped,
        length,
        bpm: tempo,
        time_signature: ts,
        notes: Vec::new(),
    })
}

/// The tracks an imported file lists: one per channel that plays notes,
/// named by its track (else its instrument), with its first program —
/// what a `file` block gets when it lists none.
pub fn file_tracks(mid: &[u8]) -> Result<Vec<MidiTrack>, String> {
    let file = crate::smf::read(mid)?;
    Ok(file
        .channels
        .iter()
        .map(|c| {
            let drums = c.channel == 9;
            MidiTrack {
                name: Some(c.name.clone().unwrap_or_else(|| {
                    if drums {
                        format!("{} Kit", crate::gm::kit_name(c.program))
                    } else {
                        crate::gm::program_name(c.program).to_string()
                    }
                })),
                program: Some(c.program),
                bank: drums.then(|| serde_json::json!("drums")),
                channel: Some(c.channel),
                ..Default::default()
            }
        })
        .collect())
}

fn file(block: &MidiBlock, mid: &[u8]) -> Result<Score, String> {
    let file = crate::smf::read(mid)?;
    let ts = match block.time_signature {
        Some(_) => block.time_signature(),
        None => file.time_signature,
    };
    // A beat is one count of the meter: quarters scaled by its unit.
    let bpm = file.first_qpm * f64::from(ts[1]) / 4.0;
    let listed: &[MidiTrack] = block.tracks.as_deref().unwrap_or(&[]);
    let mut notes = Vec::new();
    if file.tempo_changes > 0 {
        notes.push(format!(
            "tempo changes: the file changes tempo {} time(s); it plays as written, and the \
             beat grid keeps the first tempo ({bpm:.2} BPM)",
            file.tempo_changes
        ));
    }
    let mut tracks = Vec::new();
    for channel in &file.channels {
        let drums = channel.channel == 9;
        let own = listed
            .iter()
            .find(|t| t.channel == Some(channel.channel))
            .cloned()
            .unwrap_or_default();
        let mut events = Vec::new();
        // The file's own instrument unless the track names another.
        let program = own.program.unwrap_or(channel.program).min(127);
        let replace_program = program != channel.program;
        let replace_pan = own.pan.is_some_and(f64::is_finite);
        if replace_program {
            events.push(Event {
                time: 0.0,
                msg: Msg::Program(program),
            });
        }
        if let Some(pan) = own.pan.filter(|p| p.is_finite()) {
            events.push(Event {
                time: 0.0,
                msg: Msg::Control(10, pan_cc(pan)),
            });
        }
        events.extend(channel.events.iter().copied().filter(|e| match e.msg {
            Msg::Program(_) => !replace_program,
            Msg::Control(10, _) => !replace_pan,
            _ => true,
        }));
        let name = own
            .name
            .clone()
            .or_else(|| channel.name.clone())
            .unwrap_or_else(|| {
                if drums {
                    format!("{} Kit", crate::gm::kit_name(program))
                } else {
                    crate::gm::program_name(program).to_string()
                }
            });
        tracks.push(Track {
            name,
            channel: channel.channel,
            program,
            drums,
            gain: gain(&own),
            muted: muted(listed, &own),
            notes: channel.notes,
            events,
        });
    }
    let looped = block.is_loop();
    let length = if looped {
        let bar = f64::from(ts[0]) * 60.0 / bpm;
        let bars = block
            .bars
            .filter(|b| *b > 0)
            .unwrap_or_else(|| ((file.end / bar - 1e-6).ceil() as u32).max(1));
        let length = f64::from(bars) * bar;
        let mut cut = 0usize;
        for track in &mut tracks {
            let before = track.events.len();
            track
                .events
                .retain(|e| !(matches!(e.msg, Msg::On { .. }) && e.time >= length - 1e-9));
            cut += before - track.events.len();
        }
        if cut > 0 {
            notes.push(format!(
                "{cut} note(s) past the loop's {bars} bar(s) were left out"
            ));
        }
        length
    } else {
        file.end
    };
    Ok(Score {
        tracks,
        looped,
        length,
        bpm,
        time_signature: ts,
        notes,
    })
}
