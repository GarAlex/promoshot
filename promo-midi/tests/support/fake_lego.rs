//! A fake lego model behind [`LayerGenerator`] (MIDI.md §7), for the tests
//! of promo-midi, the headless server and promo-ffi (`#[path]`). No model:
//! the "layer" is a click on every beat of the request's `bpm`,
//! `offset_ms` late — or, with `tones`, sustained sines a bar at a time —
//! as long as the context cut to whole latents (lego's 1920 frames), so a
//! test controls exactly how far off the grid, or off the chords, a layer
//! is. It records each request.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use promo_midi::generate::{LayerAudio, LayerGenerator, LegoRequest, CANCELLED, LATENT_FRAMES};
use promo_midi::render::Stereo;

#[derive(Default)]
pub struct State {
    /// How late the clicks are, ms.
    pub offset_ms: f64,
    /// Sustained tones instead of clicks: (seconds per bar, the
    /// frequencies of each bar, cycling).
    pub tones: Option<(f64, Vec<Vec<f64>>)>,
    /// Bars (indices into the `tones` cycle) left silent.
    pub silent_bars: Vec<usize>,
    /// Answer at this rate (48 kHz when None), to exercise resampling.
    pub answer_rate: Option<u32>,
    /// Fail with this reason.
    pub fail: Option<String>,
    /// Run until cancelled.
    pub hang: bool,
    /// Every request, its context dropped (its frame count kept).
    pub requests: Vec<(LegoRequest, usize)>,
}

#[derive(Clone, Default)]
pub struct FakeLego {
    pub state: Arc<Mutex<State>>,
}

impl FakeLego {
    pub fn new() -> FakeLego {
        FakeLego::default()
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        f(&mut self.state.lock().unwrap())
    }
}

impl LayerGenerator for FakeLego {
    fn generate(
        &self,
        mut request: LegoRequest,
        progress: &mut dyn FnMut(f64),
        cancel: &dyn Fn() -> bool,
    ) -> Result<LayerAudio, String> {
        let frames = request.context.frames();
        request.context = Stereo::default();
        let (offset, tones, silent, rate, fail, hang) = {
            let mut st = self.state.lock().unwrap();
            st.requests.push((request.clone(), frames));
            (
                st.offset_ms,
                st.tones.clone(),
                st.silent_bars.clone(),
                st.answer_rate.unwrap_or(48_000),
                st.fail.clone(),
                st.hang,
            )
        };
        for step in 0..request.steps {
            if cancel() {
                return Err(CANCELLED.into());
            }
            progress(f64::from(step) / f64::from(request.steps));
        }
        if hang {
            loop {
                if cancel() {
                    return Err(CANCELLED.into());
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
        if let Some(why) = fail {
            return Err(why);
        }
        let latent = frames / LATENT_FRAMES * LATENT_FRAMES;
        let n = (latent as f64 * f64::from(rate) / 48_000.0) as usize;
        let mono: Vec<f32> = match tones {
            Some((bar, cycle)) => (0..n)
                .map(|i| {
                    let t = i as f64 / f64::from(rate);
                    let index = ((t / bar) as usize) % cycle.len().max(1);
                    if silent.contains(&index) {
                        return 0.0;
                    }
                    let f = &cycle[index];
                    (f.iter()
                        .map(|hz| (2.0 * std::f64::consts::PI * hz * t).sin())
                        .sum::<f64>()
                        * 0.3
                        / f.len().max(1) as f64) as f32
                })
                .collect(),
            None => {
                let period = 60.0 / request.bpm * f64::from(rate);
                let at = offset / 1000.0 * f64::from(rate);
                let burst = (0.03 * f64::from(rate)) as usize;
                let mut x = vec![0f32; n];
                let mut k = 0.0;
                loop {
                    let start = (k * period + at).round() as isize;
                    if start >= n as isize {
                        break;
                    }
                    for i in 0..burst {
                        let j = start + i as isize;
                        if j >= 0 && (j as usize) < n {
                            let t = i as f64 / f64::from(rate);
                            x[j as usize] = ((2.0 * std::f64::consts::PI * 1000.0 * t).sin()
                                * (-t * 120.0).exp()
                                * 0.5) as f32;
                        }
                    }
                    k += 1.0;
                }
                x
            }
        };
        progress(1.0);
        // The rate travels as a WAV, as a model's output would be read.
        let audio = promo_midi::render::read_stereo(&wav16(&mono, rate))?;
        Ok(LayerAudio {
            audio,
            model: Some("fake-lego".into()),
        })
    }
}

fn wav16(mono: &[f32], rate: u32) -> Vec<u8> {
    let data = mono.len() * 4;
    let mut out = Vec::with_capacity(44 + data);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data as u32).to_le_bytes());
    for v in mono {
        let s = (f64::from(*v) * 32767.0) as i16;
        out.extend_from_slice(&s.to_le_bytes());
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}
