//! P5 perf baseline: PCM mix throughput — 4 stereo 48 kHz inputs with
//! automation + duck breakpoints, streamed in 4096-frame chunks.

use criterion::{criterion_group, criterion_main, Criterion};
use promo_engine::{mix_chunk, MixInput};
use promo_timeline::VolumePoint;

fn mixer(c: &mut Criterion) {
    let sample_rate = 48_000.0;
    let channels = 2usize;
    let seconds = 60.0;
    let frames = (seconds * sample_rate) as usize;

    let inputs_pcm: Vec<Vec<f32>> = (0..4)
        .map(|i| {
            (0..frames * channels)
                .map(|s| ((s as f32) * 0.001 * (i as f32 + 1.0)).sin() * 0.2)
                .collect()
        })
        .collect();
    let curves: Vec<Vec<VolumePoint>> = (0..4)
        .map(|i| {
            (0..24)
                .map(|k| VolumePoint {
                    time: seconds * k as f64 / 24.0,
                    volume: 0.2 + ((k + i) % 5) as f32 * 0.2,
                })
                .collect()
        })
        .collect();

    let mut group = c.benchmark_group("pcm_mix");
    group.sample_size(20);
    group.bench_function("mix_60s_4in_stereo_48k_chunked", |b| {
        let chunk_frames = 4096;
        let mut out = vec![0.0f32; chunk_frames * channels];
        b.iter(|| {
            let mut mixed = 0.0f64;
            let mut frame = 0usize;
            while frame < frames {
                let n = chunk_frames.min(frames - frame);
                let start_time = frame as f64 / sample_rate;
                out[..n * channels].fill(0.0);
                let inputs: Vec<MixInput> = (0..4)
                    .map(|i| MixInput {
                        samples: &inputs_pcm[i],
                        start_time: 0.0,
                        points: &curves[i],
                    })
                    .collect();
                mix_chunk(
                    &mut out[..n * channels],
                    channels,
                    sample_rate,
                    start_time,
                    &inputs,
                );
                mixed += out[0] as f64;
                frame += n;
            }
            mixed
        });
    });
    group.finish();
}

criterion_group!(benches, mixer);
criterion_main!(benches);
