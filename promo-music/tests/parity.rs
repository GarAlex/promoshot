//! Parity against the PyTorch reference (dev-only). The reference tensors
//! come from the scratchpad scripts (`ace-port-ref/ref_*.py`, run in the
//! ACE-Step venv on CPU float32); these tests skip unless
//! `PROMOSHOT_ACE_REF` names their folder and `PROMOSHOT_ACE_CKPT` the
//! checkpoints (the Hugging Face layout).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use candle_core::{DType, Device, Tensor, D};
use candle_nn::VarBuilder;
use promo_music::vae;

fn dirs() -> Option<(PathBuf, PathBuf)> {
    let r = std::env::var_os("PROMOSHOT_ACE_REF")?;
    let c = std::env::var_os("PROMOSHOT_ACE_CKPT")?;
    Some((PathBuf::from(r), PathBuf::from(c)))
}

fn load(path: &std::path::Path) -> HashMap<String, Tensor> {
    candle_core::safetensors::load(path, &Device::Cpu).expect("reference tensors")
}

/// (max |a-b|, max |a-b| / max |b|, SNR dB of a against b).
fn compare(a: &Tensor, b: &Tensor) -> (f32, f32, f64) {
    let a = a.to_dtype(DType::F32).unwrap().to_device(&Device::Cpu).unwrap().flatten_all().unwrap();
    let b = b.to_dtype(DType::F32).unwrap().to_device(&Device::Cpu).unwrap().flatten_all().unwrap();
    assert_eq!(a.dims(), b.dims(), "shape");
    let d = (&a - &b).unwrap();
    let max_d = d.abs().unwrap().max(D::Minus1).unwrap().to_scalar::<f32>().unwrap();
    let max_b = b.abs().unwrap().max(D::Minus1).unwrap().to_scalar::<f32>().unwrap();
    let e: f64 = d.sqr().unwrap().sum_all().unwrap().to_dtype(DType::F64).unwrap().to_scalar().unwrap();
    let s: f64 = b.sqr().unwrap().sum_all().unwrap().to_dtype(DType::F64).unwrap().to_scalar().unwrap();
    (max_d, max_d / max_b.max(1e-12), 10.0 * (s / e.max(1e-30)).log10())
}

fn report(what: &str, a: &Tensor, b: &Tensor) -> (f32, f32, f64) {
    let r = compare(a, b);
    eprintln!("{what}: max abs {:.3e}, max rel {:.3e}, SNR {:.1} dB", r.0, r.1, r.2);
    r
}

fn device() -> Device {
    promo_music::default_device()
}

#[test]
fn m1_vae_encode_decode_32s() {
    let Some((refs, ckpt)) = dirs() else { return };
    let dev = device();
    let dtype = promo_music::default_dtype(&dev);
    let r = load(&refs.join("vae32.safetensors"));
    let vb = unsafe {
        VarBuilder::from_mmaped_safetensors(&[ckpt.join("vae/diffusion_pytorch_model.safetensors")], dtype, &dev).unwrap()
    };
    let enc = vae::Encoder::load(&vb).unwrap();
    let dec = vae::Decoder::load(&vb).unwrap();
    let audio = r["audio"].unsqueeze(0).unwrap().to_dtype(dtype).unwrap().to_device(&dev).unwrap();
    let t = Instant::now();
    let (mean, std) = enc.moments(&audio).unwrap();
    let mean_cpu = mean.to_device(&Device::Cpu).unwrap();
    eprintln!("encode 32 s: {:.2?} on {dev:?} {dtype:?}", t.elapsed());
    let (_, rel_m, snr_m) = report("vae mean", &mean_cpu.squeeze(0).unwrap(), &r["mean"]);
    let (_, _, snr_s) = report("vae std", &std.squeeze(0).unwrap(), &r["std"]);
    let t = Instant::now();
    let z = r["mean"].unsqueeze(0).unwrap().to_dtype(dtype).unwrap().to_device(&dev).unwrap();
    let y = dec.decode(&z).unwrap();
    let y = y.to_device(&Device::Cpu).unwrap();
    eprintln!("decode 32 s: {:.2?}", t.elapsed());
    let (_, _, snr_d) = report("vae decode", &y.squeeze(0).unwrap(), &r["decoded"]);
    let floor = if dtype == DType::F32 { 60.0 } else { 30.0 };
    assert!(snr_m > floor && snr_s > floor && snr_d > floor, "{rel_m}");
}

const REF: &str = "loop32_strings_1234/lego_ref.safetensors";

fn model() -> Option<(HashMap<String, Tensor>, promo_music::pipeline::AceStep)> {
    let (refs, ckpt) = dirs()?;
    let r = load(&refs.join(REF));
    let dev = device();
    let dtype = promo_music::default_dtype(&dev);
    let t = Instant::now();
    let m = promo_music::pipeline::AceStep::load(&ckpt, dev, dtype).expect("model");
    eprintln!("load: {:.2?} ({dtype:?})", t.elapsed());
    Some((r, m))
}

fn ids(t: &Tensor) -> Vec<u32> {
    t.flatten_all().unwrap().to_vec1::<f32>().unwrap().into_iter().map(|v| v as u32).collect()
}

fn floor(m: &promo_music::pipeline::AceStep, f32_db: f64, half_db: f64) -> f64 {
    if m.dtype == DType::F32 { f32_db } else { half_db }
}

#[test]
fn m2_text_encoder() {
    let Some((r, m)) = model() else { return };
    let p = promo_music::pipeline::prompt("strings", "lush legato string section, sustained chords, cinematic", 120.0, 4, Some("C minor"), 32);
    let tok = m.tokenizer().encode(&p, 256).unwrap();
    assert_eq!(tok, ids(&r["text.input_ids"]), "prompt tokens");
    let lt = m.tokenizer().encode(promo_music::pipeline::lyrics(), 2048).unwrap();
    assert_eq!(lt, ids(&r["lyric.input_ids"]), "lyric tokens");
    let t = Instant::now();
    let h = m.text_encoder().hidden(&tok).unwrap();
    eprintln!("text encoder ({} tokens): {:.2?}", tok.len(), t.elapsed());
    let (_, _, snr) = report("text hidden", &h, &r["text.hidden"]);
    let (_, _, snr_l) = report("lyric embed", &m.text_encoder().embed(&lt).unwrap(), &r["lyric.hidden"]);
    assert!(snr > floor(&m, 60.0, 25.0) && snr_l > floor(&m, 100.0, 40.0));
}

fn to(m: &promo_music::pipeline::AceStep, t: &Tensor) -> Tensor {
    t.to_dtype(m.dtype).unwrap().to_device(&m.device).unwrap()
}

#[test]
fn m3_condition_and_one_dit_step() {
    let Some((r, m)) = model() else { return };
    // The condition encoder from the reference's inputs.
    let cond = m
        .condition_encoder()
        .forward(&to(&m, &r["cond_in.text_hidden_states"]), &to(&m, &r["cond_in.lyric_hidden_states"]), &m.timbre().unwrap())
        .unwrap();
    let (_, _, snr_c) = report("encoder hidden states", &cond, &r["cond.encoder_hidden_states"]);
    report("timbre ref", &m.timbre().unwrap(), &r["cond_in.refer_audio_acoustic_hidden_states_packed"]);
    // The context from the reference's src latents.
    let ctx = m.context(&r["cond_in.src_latents"]).unwrap();
    let (_, _, snr_x) = report("context latents", &ctx, &r["cond.context_latents"]);
    // One step from the reference's inputs (batch 2, conditioned + null).
    let enc = &r["step0.enc"];
    let kv = m.dit().cross_kv(&to(&m, enc)).unwrap();
    let null_ok = report("null branch", &m.dit().null_condition(&to(&m, &enc.get(0).unwrap().unsqueeze(0).unwrap())).unwrap(), &enc.get(1).unwrap().unsqueeze(0).unwrap());
    let t0 = r["step0.t"].to_vec1::<f32>().unwrap();
    let t = Instant::now();
    let v = m.dit().forward(&to(&m, &r["step0.x"]), &to(&m, &r["step0.ctx"]), &t0, &kv).unwrap();
    let v = v.to_device(&Device::Cpu).unwrap();
    eprintln!("one DiT step (batch 2, {} latents): {:.2?}", r["step0.x"].dim(1).unwrap(), t.elapsed());
    let (_, _, snr_v) = report("step 0 velocity", &v, &r["step0.v"]);
    let r1 = r["step1.t"].to_vec1::<f32>().unwrap();
    let v1 = m.dit().forward(&to(&m, &r["step1.x"]), &to(&m, &r["step1.ctx"]), &r1, &kv).unwrap();
    report("step 1 velocity", &v1, &r["step1.v"]);
    assert!(null_ok.2 > 100.0);
    assert!(snr_c > floor(&m, 60.0, 25.0) && snr_x > floor(&m, 60.0, 30.0) && snr_v > floor(&m, 50.0, 20.0));
}

#[test]
fn m4_full_lego_from_dumped_noise() {
    let Some((r, m)) = model() else { return };
    let steps = std::env::var("PROMOSHOT_ACE_STEPS").ok().and_then(|s| s.parse().ok()).unwrap_or(50);
    // The whole run from the reference's context audio, with its noises.
    let (refs, _) = dirs().unwrap();
    let audio = r["audio"].clone();
    let _ = audio;
    let ctx_wav = refs.join("vae32.safetensors");
    let ctx = load(&ctx_wav)["audio"].unsqueeze(0).unwrap();
    let mut calls = 0;
    let src = m
        .encode(&ctx, &mut |len| {
            let n = r[&format!("enc{calls}.noise")].clone();
            calls += 1;
            assert_eq!(n.dim(2).unwrap(), len);
            Ok(n)
        })
        .unwrap();
    let (_, _, snr_src) = report("src latents (tiled encode)", &src, &r["cond_in.src_latents"]);
    let p = promo_music::pipeline::prompt("strings", "lush legato string section, sustained chords, cinematic", 120.0, 4, Some("C minor"), 32);
    let t = Instant::now();
    let cond = m.condition(&p).unwrap();
    report("encoder hidden states (own text)", &cond, &r["cond.encoder_hidden_states"]);
    let kv = m.cross_kv(&cond).unwrap();
    let context = m.context(&src).unwrap();
    eprintln!("conditioning: {:.2?}", t.elapsed());
    let c = promo_music::sampler::Conditioning { dit: m.dit(), context: &context, kv: &kv };
    let t = Instant::now();
    let out = promo_music::sampler::sample(&c, &r["noise"], steps, 7.0, 3.0, &mut |_| true).unwrap().unwrap();
    let dt = t.elapsed();
    eprintln!("diffusion {steps} steps: {dt:.2?} ({:.3} s/step)", dt.as_secs_f64() / steps as f64);
    if steps != 50 {
        return;
    }
    let (_, _, snr_lat) = report("final latents", &out, &r["target_latents"]);
    let t = Instant::now();
    let wav = m.decode(&out).unwrap();
    eprintln!("decode: {:.2?}", t.elapsed());
    let (_, _, snr_a) = report("final audio", &wav, &r["audio"]);
    // The decode alone from the reference's latents.
    let wav_ref = m.decode(&r["target_latents"]).unwrap();
    let (_, _, snr_dec) = report("decode of reference latents", &wav_ref, &r["audio"]);
    if let Ok(dir) = std::env::var("PROMOSHOT_ACE_OUT") {
        let path = PathBuf::from(dir).join(format!("rust_{:?}.safetensors", m.dtype));
        candle_core::safetensors::save(&HashMap::from([("audio".to_string(), wav.clone()), ("latents".to_string(), out.clone())]), path).unwrap();
    }
    assert!(snr_src > floor(&m, 60.0, 25.0) && snr_dec > floor(&m, 60.0, 25.0));
    assert!(snr_lat > floor(&m, 20.0, 5.0) && snr_a > floor(&m, 15.0, 3.0), "{snr_lat} {snr_a}");
}
