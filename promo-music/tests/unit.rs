//! What needs no weights: the prompt, the schedule, guidance, the
//! transposed convolution, the noise, and the generator's absence.

use candle_core::{Device, Tensor};
use promo_music::{pipeline, rng::Normal, sampler, vae};

#[test]
fn prompt_is_the_servers() {
    // The text the ACE-Step server logged for the experiment's request.
    let p = pipeline::prompt(
        "strings",
        "lush legato string section, cinematic",
        120.0,
        4,
        Some("C minor"),
        8,
    );
    assert_eq!(
        p,
        "# Instruction\nGenerate the STRINGS track based on the audio context:\n\n# Caption\nlush legato string section, cinematic\n\n# Metas\n- bpm: 120\n- timesignature: 4\n- keyscale: C minor\n- duration: 8 seconds\n<|endoftext|>\n"
    );
    let none = pipeline::prompt("synth", "pad", 0.0, 0, None, 32);
    assert!(
        none.contains("- bpm: N/A\n- timesignature: N/A\n- keyscale: N/A\n- duration: 32 seconds")
    );
    assert_eq!(
        pipeline::lyrics(),
        "# Languages\nen\n\n# Lyric\n[Instrumental]<|endoftext|>"
    );
}

#[test]
fn schedule_is_shifted_linspace() {
    let t = sampler::timesteps(50, 3.0);
    assert_eq!(t.len(), 51);
    assert_eq!((t[0], t[50]), (1.0, 0.0));
    // shift·t/(1+(shift−1)·t) at t = 0.5: 0.75.
    assert!((t[25] - 0.75).abs() < 1e-6, "{}", t[25]);
    assert!(t.windows(2).all(|w| w[0] > w[1]));
}

#[test]
fn apg_removes_the_parallel_part_and_clips() {
    let (frames, channels) = (4, 1);
    let cond = [1.0f32, 0.0, 0.0, 0.0];
    // A difference parallel to cond is removed entirely (η = 0).
    let uncond = [0.5f32, 0.0, 0.0, 0.0];
    for mode in [sampler::ApgMode::Torch, sampler::ApgMode::Mlx] {
        let mut apg = sampler::Apg::new(mode);
        let out = apg.guide(&cond, &uncond, frames, channels, 7.0);
        assert!(
            out.iter().zip(&cond).all(|(a, b)| (a - b).abs() < 1e-6),
            "{out:?}"
        );
    }
    // An orthogonal difference is scaled by (g − 1), its norm clipped to 2.5.
    let uncond = [1.0f32, -10.0, 0.0, 0.0];
    let mut apg = sampler::Apg::new(sampler::ApgMode::Torch);
    let out = apg.guide(&cond, &uncond, frames, channels, 7.0);
    assert!((out[1] - 6.0 * 2.5).abs() < 1e-4, "{out:?}");
    // Momentum: torch adds −0.75 of the last difference, MLX all of it.
    let mut t = sampler::Apg::new(sampler::ApgMode::Torch);
    let mut m = sampler::Apg::new(sampler::ApgMode::Mlx);
    let small = [1.0f32, -0.1, 0.0, 0.0];
    t.guide(&cond, &small, frames, channels, 2.0);
    m.guide(&cond, &small, frames, channels, 2.0);
    let ot = t.guide(&cond, &small, frames, channels, 2.0);
    let om = m.guide(&cond, &small, frames, channels, 2.0);
    assert!((ot[1] - 0.1 * 0.25).abs() < 1e-5, "{ot:?}");
    assert!((om[1] - 0.1 * 2.0).abs() < 1e-5, "{om:?}");
}

#[test]
fn transposed_conv_matches_candle() {
    let dev = Device::Cpu;
    for stride in [2usize, 4, 6, 10] {
        let x = Tensor::randn(0f32, 1., (1, 8, 13), &dev).unwrap();
        let w = Tensor::randn(0f32, 1., (8, 5, 2 * stride), &dev).unwrap();
        let a = x.conv_transpose1d(&w, stride / 2, 0, stride, 1, 1).unwrap();
        let b = vae::conv_transpose_k2s(&x, &w, stride, stride / 2).unwrap();
        assert_eq!(a.dims(), b.dims());
        let d: f32 = (a - b)
            .unwrap()
            .abs()
            .unwrap()
            .flatten_all()
            .unwrap()
            .max(0)
            .unwrap()
            .to_scalar()
            .unwrap();
        assert!(d < 1e-4, "stride {stride}: {d}");
    }
}

#[test]
fn noise_is_seeded() {
    let a = Normal::new(1234).fill(1000);
    assert_eq!(a, Normal::new(1234).fill(1000));
    assert_ne!(a, Normal::new(1235).fill(1000));
    let v = Normal::new(7).fill(100_000);
    let mean = v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64;
    let var = v.iter().map(|&x| (x as f64 - mean).powi(2)).sum::<f64>() / v.len() as f64;
    assert!(
        mean.abs() < 0.02 && (var - 1.0).abs() < 0.02,
        "{mean} {var}"
    );
}

#[test]
fn not_registered_without_weights() {
    std::env::set_var(
        "PROMOSHOT_MUSIC_MODELS",
        std::env::temp_dir().join("promo-music-none"),
    );
    assert!(!promo_music::install_generator(false));
}
