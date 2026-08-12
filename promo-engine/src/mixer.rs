//! Streaming PCM mixer (Phase 5): mixes N interleaved f32 inputs into an
//! output chunk, each input scaled by its piecewise-linear amplitude curve
//! (`promo_timeline::VolumePoint` breakpoints — the exact automation the
//! AVFoundation mix executes today, so swapping the executor changes
//! nothing audible). Portable, no I/O: hosts feed decoded PCM and encode
//! the result.

use promo_timeline::VolumePoint;

/// One mixer input: interleaved samples placed on the output timeline.
pub struct MixInput<'a> {
    /// Interleaved f32 PCM.
    pub samples: &'a [f32],
    /// Output time of `samples[0]`.
    pub start_time: f64,
    /// Amplitude breakpoints (empty = unity gain).
    pub points: &'a [VolumePoint],
}

/// Amplitude of a breakpoint curve at `t`: linear between points, clamped
/// outside, unity when empty.
fn level_at(t: f64, points: &[VolumePoint]) -> f32 {
    let Some(first) = points.first() else {
        return 1.0;
    };
    if t <= first.time {
        return first.volume;
    }
    let last = points[points.len() - 1];
    if t >= last.time {
        return last.volume;
    }
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if t >= a.time && t <= b.time {
            let span = b.time - a.time;
            let p = if span > 0.0 {
                ((t - a.time) / span) as f32
            } else {
                1.0
            };
            return a.volume + (b.volume - a.volume) * p;
        }
    }
    last.volume
}

/// Mixes every input into `output` (interleaved, pre-zeroed or accumulating),
/// where `output[0]` is at `chunk_start_time`. Sample-accurate placement,
/// per-frame amplitude evaluation. Call repeatedly with consecutive chunks
/// to stream arbitrarily long timelines with bounded memory.
pub fn mix_chunk(
    output: &mut [f32],
    channels: usize,
    sample_rate: f64,
    chunk_start_time: f64,
    inputs: &[MixInput],
) {
    if channels == 0 || sample_rate <= 0.0 {
        return;
    }
    let frames = output.len() / channels;
    for input in inputs {
        let input_frames = input.samples.len() / channels;
        if input_frames == 0 {
            continue;
        }
        // First output frame this input touches.
        let offset_frames = (input.start_time - chunk_start_time) * sample_rate;
        let first_out = offset_frames.max(0.0).round() as usize;
        // Input frame corresponding to first_out.
        let skip_in = (-offset_frames).max(0.0).round() as usize;
        if first_out >= frames || skip_in >= input_frames {
            continue;
        }
        let run = (frames - first_out).min(input_frames - skip_in);
        for f in 0..run {
            let t = chunk_start_time + (first_out + f) as f64 / sample_rate;
            let amp = level_at(t, input.points);
            let out_base = (first_out + f) * channels;
            let in_base = (skip_in + f) * channels;
            for c in 0..channels {
                output[out_base + c] += input.samples[in_base + c] * amp;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unity_mix_sums_inputs() {
        let a = [0.5f32; 8];
        let b = [0.25f32; 8];
        let mut out = [0.0f32; 8];
        mix_chunk(
            &mut out,
            2,
            48_000.0,
            0.0,
            &[
                MixInput {
                    samples: &a,
                    start_time: 0.0,
                    points: &[],
                },
                MixInput {
                    samples: &b,
                    start_time: 0.0,
                    points: &[],
                },
            ],
        );
        assert!(out.iter().all(|&s| (s - 0.75).abs() < 1e-6));
    }

    #[test]
    fn ramp_applies_midpoint_amplitude() {
        // 1 s of mono at 8 Hz; ramp 1.0 → 0.0 over the second.
        let samples = [1.0f32; 8];
        let points = [
            VolumePoint {
                time: 0.0,
                volume: 1.0,
            },
            VolumePoint {
                time: 1.0,
                volume: 0.0,
            },
        ];
        let mut out = [0.0f32; 8];
        mix_chunk(
            &mut out,
            1,
            8.0,
            0.0,
            &[MixInput {
                samples: &samples,
                start_time: 0.0,
                points: &points,
            }],
        );
        assert!((out[0] - 1.0).abs() < 1e-6);
        assert!(
            (out[4] - 0.5).abs() < 1e-6,
            "midpoint amplitude, got {}",
            out[4]
        );
        assert!((out[7] - 0.125).abs() < 1e-6);
    }

    #[test]
    fn offset_input_lands_sample_accurately() {
        let samples = [1.0f32; 4];
        let mut out = [0.0f32; 16];
        // Mono 8 Hz: input starts at t=0.5 → frame 4.
        mix_chunk(
            &mut out,
            1,
            8.0,
            0.0,
            &[MixInput {
                samples: &samples,
                start_time: 0.5,
                points: &[],
            }],
        );
        assert_eq!(&out[0..4], &[0.0; 4]);
        assert_eq!(&out[4..8], &[1.0; 4]);
        assert_eq!(&out[8..16], &[0.0; 8]);
    }

    #[test]
    fn chunked_streaming_matches_single_pass() {
        // 2 s mono at 100 Hz with a ramp; mix once vs two 1 s chunks.
        let samples: Vec<f32> = (0..200).map(|i| (i as f32 * 0.31).sin()).collect();
        let points = [
            VolumePoint {
                time: 0.2,
                volume: 0.9,
            },
            VolumePoint {
                time: 1.7,
                volume: 0.1,
            },
        ];
        let mut single = vec![0.0f32; 200];
        mix_chunk(
            &mut single,
            1,
            100.0,
            0.0,
            &[MixInput {
                samples: &samples,
                start_time: 0.0,
                points: &points,
            }],
        );
        let mut first = vec![0.0f32; 100];
        let mut second = vec![0.0f32; 100];
        mix_chunk(
            &mut first,
            1,
            100.0,
            0.0,
            &[MixInput {
                samples: &samples,
                start_time: 0.0,
                points: &points,
            }],
        );
        mix_chunk(
            &mut second,
            1,
            100.0,
            1.0,
            &[MixInput {
                samples: &samples[100..],
                start_time: 1.0,
                points: &points,
            }],
        );
        for i in 0..100 {
            assert!((single[i] - first[i]).abs() < 1e-6);
            assert!((single[100 + i] - second[i]).abs() < 1e-6, "frame {i}");
        }
    }
}
