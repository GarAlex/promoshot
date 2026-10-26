//! The block expanded to what the synth plays: tracks of timed MIDI
//! messages in seconds, with each track's instrument and mix, the take's
//! length and its beat grid.

use promo_model::midi::{MidiBlock, MidiTrack};

use crate::tempo::TempoMap;

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
    /// The grid's tempo: the first one the take plays.
    pub bpm: f64,
    pub time_signature: [u32; 2],
    /// What the take did not keep, in words.
    pub notes: Vec<String>,
    /// The loudness the take is normalised to, LUFS.
    pub target_lufs: f64,
    /// `midi.level`, dB on top.
    pub trim_db: f64,
}

/// What music is normalised to: the loudness stage's own default target
/// (a `normalize` effect with no `targetLufs`).
pub const MUSIC_LUFS: f64 = promo_media::loudness::DEFAULT_TARGET_LUFS;
/// What a sound effect is normalised to: 4 LU under music. A one-shot's
/// integrated loudness is measured over a few 400 ms blocks of mostly
/// attack, which reads louder than it sits in a mix; at −16 a ding or a
/// hit would jump over the narration and the bed it is meant to punctuate.
pub const EFFECT_LUFS: f64 = -20.0;

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

/// Expands the block. `mid` is the slice-1 `file` block's `.mid` bytes —
/// read only when its tracks carry no notes.
pub fn expand(block: &MidiBlock, mid: Option<&[u8]>) -> Result<Score, String> {
    match block.source() {
        "effect" => effect(block),
        "steps" => Ok(notes(block, block.tracks.clone().unwrap_or_default(), None)),
        "file" if has_notes(block) => {
            Ok(notes(block, block.tracks.clone().unwrap_or_default(), None))
        }
        "file" => {
            let file = crate::smf::read(mid.ok_or("midi: a file source with no .mid bytes")?)?;
            Ok(notes(block, merged_tracks(block, &file), Some(&file)))
        }
        other => Err(format!(
            "midi: unknown source \"{other}\" — steps, notes, file or effect"
        )),
    }
}

/// Whether any track carries notes inline (an imported block since v2).
pub fn has_notes(block: &MidiBlock) -> bool {
    block
        .tracks
        .iter()
        .flatten()
        .any(|t| t.notes.as_ref().is_some_and(|n| !n.is_empty()))
}

/// A slice-1 file block's tracks: the file's, each with the mix the block
/// lists for its channel.
pub fn merged_tracks(block: &MidiBlock, file: &crate::smf::File) -> Vec<MidiTrack> {
    let listed: &[MidiTrack] = block.tracks.as_deref().unwrap_or(&[]);
    file.tracks
        .iter()
        .map(|t| {
            let mut track = t.clone();
            if let Some(own) = listed
                .iter()
                .find(|l| l.channel.is_some() && l.channel == t.channel)
            {
                track.name = own.name.clone().or(track.name);
                track.program = own.program.or(track.program);
                track.bank = own.bank.clone().or(track.bank);
                track.volume = own.volume;
                track.pan = own.pan.or(track.pan);
                track.mute = own.mute;
                track.solo = own.solo;
            }
            track
        })
        .collect()
}

/// One track's events in BEATS from the song's start.
fn beat_events(
    track: &MidiTrack,
    bars: u32,
    bar_beats: f64,
    swing: f64,
    dropped: &mut usize,
) -> Vec<(f64, Msg)> {
    let mut out = Vec::new();
    let mut skipped = 0usize;
    let mut strike = |t: f64, d: f64, key: u8, vel: u8, out: &mut Vec<(f64, Msg)>| {
        if t < 0.0 || !t.is_finite() {
            *dropped += 1;
            return;
        }
        let d = if d.is_finite() && d > 0.0 {
            d
        } else {
            1.0 / 64.0
        };
        let key = key.min(127);
        out.push((
            t,
            Msg::On {
                key,
                vel: vel.clamp(1, 127),
            },
        ));
        out.push((t + d, Msg::Off { key }));
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
                        &mut out,
                    );
                    index += cycle;
                }
            }
        }
    }
    for note in track.notes.iter().flatten() {
        strike(note.t, note.d, note.n, note.v.unwrap_or(100), &mut out);
    }
    for c in track.controls.iter().flatten() {
        if !c.t.is_finite() || c.t < 0.0 {
            continue;
        }
        if let Some(p) = c.program {
            out.push((c.t, Msg::Program(p.min(127))));
        }
        if let Some(b) = c.bend {
            out.push((c.t, Msg::Bend(b.min(16383))));
        }
        if let (Some(cc), Some(v)) = (c.cc, c.v) {
            out.push((c.t, Msg::Control(cc.min(127), v.min(127))));
        }
    }
    *dropped += skipped;
    out
}

fn notes(block: &MidiBlock, tracks: Vec<MidiTrack>, file: Option<&crate::smf::File>) -> Score {
    let ts = match (block.time_signature, file) {
        (None, Some(file)) => file.time_signature,
        _ => block.time_signature(),
    };
    let map = match (&block.tempo_map, file) {
        (None, Some(file)) => TempoMap::new(&file.tempo_map, file.first_bpm()),
        _ => TempoMap::from_block(block),
    };
    let bar_beats = f64::from(ts[0]);
    let swing = block
        .swing
        .filter(|s| s.is_finite())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let mut notes_out = Vec::new();
    // The song's length in bars: stated, else what the content reaches.
    let content = tracks
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
        .unwrap_or_else(|| ((content / bar_beats - 1e-9).ceil() as u32).max(1))
        .min(4096);
    let range = block
        .loop_bars(f64::from(bars))
        .map(|(from, to)| ((from - 1.0) * bar_beats, (to - 1.0) * bar_beats));
    let start = range.map_or(0.0, |r| r.0);
    let zero = map.seconds(start);
    let mut dropped = 0usize;
    let mut out = Vec::new();
    let mut in_range = 0usize;
    for track in &tracks {
        let drums = track.is_drums();
        let program = track.program.unwrap_or(0).min(127);
        let mut events = opening(track, drums, program);
        let mut count = 0usize;
        let mut raw = beat_events(track, bars, bar_beats, swing, &mut dropped);
        raw.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.rank().cmp(&b.1.rank())));
        match range {
            None => {
                for (beat, msg) in raw {
                    if matches!(msg, Msg::On { .. }) {
                        count += 1;
                    }
                    events.push(Event {
                        time: map.seconds(beat),
                        msg,
                    });
                }
            }
            Some((from, to)) => {
                // Where the range starts the song is already under way:
                // the last of each control before it holds at the head.
                let mut state: Vec<Msg> = Vec::new();
                // A note struck in the range keeps its release, wherever
                // that falls: past the end, it wraps.
                let mut pending: Vec<(u8, bool)> = Vec::new();
                for (beat, msg) in raw {
                    match msg {
                        Msg::On { key, .. } => {
                            if beat >= from - 1e-9 && beat < to - 1e-9 {
                                count += 1;
                                events.push(Event {
                                    time: map.seconds(beat) - zero,
                                    msg,
                                });
                                pending.push((key, true));
                            } else {
                                pending.push((key, false));
                            }
                        }
                        Msg::Off { key } => {
                            if let Some(i) = pending.iter().position(|p| p.0 == key) {
                                let (_, kept) = pending.remove(i);
                                if kept {
                                    events.push(Event {
                                        time: (map.seconds(beat) - zero).max(0.0),
                                        msg,
                                    });
                                }
                            }
                        }
                        _ if beat < from - 1e-9 => {
                            state.retain(|m| !same_control(m, &msg));
                            state.push(msg);
                        }
                        _ if beat < to - 1e-9 => events.push(Event {
                            time: map.seconds(beat) - zero,
                            msg,
                        }),
                        _ => {}
                    }
                }
                let mut head: Vec<Event> = state
                    .into_iter()
                    .map(|msg| Event { time: 0.0, msg })
                    .collect();
                let open = opening(track, drums, program).len();
                let rest = events.split_off(open);
                events.append(&mut head);
                events.extend(rest);
            }
        }
        in_range += count;
        out.push(Track {
            name: track_name(track, drums, program),
            channel: if drums { 9 } else { 0 },
            program,
            drums,
            gain: gain(track),
            muted: muted(&tracks, track),
            notes: count,
            events,
        });
    }
    if dropped > 0 {
        notes_out.push(format!("{dropped} note(s) outside the song were left out"));
    }
    let (looped, length, bpm, varies) = match range {
        Some((from, to)) => {
            if in_range == 0 {
                notes_out.push("the loop's bars hold no notes".into());
            }
            (
                true,
                map.seconds(to) - zero,
                map.bpm_at(from),
                map.varies(from, to),
            )
        }
        None => (
            false,
            last_event(&out),
            map.bpm_at(0.0),
            map.varies(0.0, f64::INFINITY),
        ),
    };
    if varies {
        notes_out.push(format!(
            "tempo changes: the take plays them as written, and the beat grid keeps the first \
             tempo ({bpm:.2} BPM)"
        ));
    }
    Score {
        tracks: out,
        looped,
        length,
        bpm,
        time_signature: ts,
        notes: notes_out,
        target_lufs: MUSIC_LUFS,
        trim_db: block.level_db(),
    }
}

/// Whether two messages set the same thing (the last one wins).
fn same_control(a: &Msg, b: &Msg) -> bool {
    match (a, b) {
        (Msg::Control(x, _), Msg::Control(y, _)) => x == y,
        (Msg::Program(_), Msg::Program(_)) | (Msg::Bend(_), Msg::Bend(_)) => true,
        _ => false,
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
        looped: false,
        length,
        bpm: block.tempo(),
        time_signature: block.time_signature(),
        notes: Vec::new(),
        target_lufs: EFFECT_LUFS,
        trim_db: block.level_db(),
    })
}

/// The tracks an imported file lists, notes and controls inline.
pub fn file_tracks(mid: &[u8]) -> Result<Vec<MidiTrack>, String> {
    Ok(crate::smf::read(mid)?.tracks)
}
