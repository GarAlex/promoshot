//! Sound-effect presets (MIDI.md §3, slice 3 tunes them): each a short
//! gesture on a GM instrument, transposed by `pitch`, stretched to
//! `length`, played on `program` in place of its own.
//!
//! These are first drafts with the real shape — id, instrument, nominal
//! length, and an expansion to timed events — so the editor, the receipt
//! and an agent's block are settled before the sounds are.

use crate::score::{Event, Msg};

/// One preset: its id (what `effect.preset` spells), a title, the GM
/// program it plays on, and its nominal length in seconds.
#[derive(Debug, Clone, Copy)]
pub struct Preset {
    pub id: &'static str,
    pub title: &'static str,
    pub program: u8,
    pub length: f64,
}

pub const PRESETS: [Preset; 8] = [
    Preset {
        id: "rise",
        title: "Rise",
        program: 50,
        length: 2.0,
    },
    Preset {
        id: "hit",
        title: "Hit",
        program: 55,
        length: 0.6,
    },
    Preset {
        id: "whoosh",
        title: "Whoosh",
        program: 119,
        length: 0.8,
    },
    Preset {
        id: "ding",
        title: "Ding",
        program: 9,
        length: 0.5,
    },
    Preset {
        id: "stinger",
        title: "Stinger",
        program: 61,
        length: 1.2,
    },
    Preset {
        id: "sparkle",
        title: "Sparkle",
        program: 8,
        length: 0.8,
    },
    Preset {
        id: "coin",
        title: "Chiptune coin",
        program: 80,
        length: 0.5,
    },
    Preset {
        id: "powerUp",
        title: "Power-up",
        program: 80,
        length: 0.8,
    },
];

pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// The preset's events in seconds, on its program (or `program`), and the
/// length it was stretched to.
pub fn expand(effect: &promo_model::midi::MidiEffect) -> Result<(u8, Vec<Event>, f64), String> {
    let Some(preset) = preset(&effect.preset) else {
        let ids: Vec<&str> = PRESETS.iter().map(|p| p.id).collect();
        return Err(format!(
            "unknown effect preset \"{}\" — one of {}",
            effect.preset,
            ids.join(", ")
        ));
    };
    let length = effect
        .length
        .filter(|l| l.is_finite() && *l > 0.0)
        .unwrap_or(preset.length)
        .clamp(0.05, 30.0);
    let scale = length / preset.length;
    let shift = effect
        .pitch
        .filter(|p| p.is_finite())
        .unwrap_or(0.0)
        .round() as i32;
    let key = |k: i32| (k + shift).clamp(0, 127) as u8;
    let program = effect.program.unwrap_or(preset.program).min(127);
    let mut events = Vec::new();
    let mut note = |t: f64, d: f64, k: i32, v: u8| {
        events.push(Event {
            time: t * scale,
            msg: Msg::On {
                key: key(k),
                vel: v,
            },
        });
        events.push(Event {
            time: (t + d) * scale,
            msg: Msg::Off { key: key(k) },
        });
    };
    // Gestures are written at the preset's nominal length and scaled.
    match preset.id {
        "rise" => {
            for k in [48, 55, 60] {
                note(0.0, 1.9, k, 90);
            }
        }
        "hit" => {
            for k in [48, 60, 67] {
                note(0.0, 0.25, k, 127);
            }
        }
        "whoosh" => note(0.0, 0.6, 60, 120),
        "ding" => note(0.0, 0.15, 84, 110),
        "stinger" => {
            for (t, d) in [(0.0, 0.12), (0.18, 0.7)] {
                for k in [60, 64, 67, 72] {
                    note(t, d, k, 118);
                }
            }
        }
        "sparkle" => {
            for (i, k) in [84, 88, 91, 93, 96, 100, 103, 108].into_iter().enumerate() {
                note(i as f64 * 0.06, 0.25, k, 90);
            }
        }
        "coin" => {
            note(0.0, 0.07, 83, 110);
            note(0.07, 0.35, 88, 110);
        }
        _ => {
            // powerUp: three rising arpeggios.
            let mut t = 0.0;
            for root in [60, 65, 67] {
                for step in [0, 4, 7, 12] {
                    note(t, 0.05, root + step, 105);
                    t += 0.05;
                }
            }
        }
    }
    // Swells and sweeps: expression and pitch bend, after the notes are in.
    match preset.id {
        "rise" => {
            // Bend range one octave (RPN 0), then sweep up while swelling.
            for (cc, v) in [(101, 0), (100, 0), (6, 12), (38, 0)] {
                events.push(Event {
                    time: 0.0,
                    msg: Msg::Control(cc, v),
                });
            }
            for i in 0..=40 {
                let f = f64::from(i) / 40.0;
                let t = f * 1.9 * scale;
                events.push(Event {
                    time: t,
                    msg: Msg::Bend((8192.0 + f * f * 8191.0).round() as u16),
                });
                events.push(Event {
                    time: t,
                    msg: Msg::Control(11, (30.0 + 97.0 * f).round() as u8),
                });
            }
        }
        "whoosh" => {
            for i in 0..=24 {
                let f = f64::from(i) / 24.0;
                let swell = (std::f64::consts::PI * f).sin();
                events.push(Event {
                    time: f * 0.6 * scale,
                    msg: Msg::Control(11, (50.0 + 77.0 * swell).round() as u8),
                });
            }
        }
        _ => {}
    }
    Ok((program, events, length))
}
