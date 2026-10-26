// Dev-only A/B: which input makes a take go wrong.
use candle_core::{Device, Tensor};
use promo_music::{pipeline::*, rng::Normal, sampler::*};
use std::collections::HashMap;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dev = promo_music::default_device();
    let m = AceStep::load(std::path::Path::new(&a[1]), dev.clone(), promo_music::default_dtype(&dev)).unwrap();
    let r = candle_core::safetensors::load(&a[2], &Device::Cpu).unwrap();
    let ctxa = candle_core::safetensors::load(&a[3], &Device::Cpu).unwrap()["audio"].unsqueeze(0).unwrap();
    let out = std::path::PathBuf::from(&a[4]);
    let to = |t: &Tensor| t.to_dtype(m.dtype).unwrap().to_device(&m.device).unwrap();
    let kv = m.cross_kv(&to(&r["mlx.enc"])).unwrap();
    let mut post = Normal::new(99);
    let own_src = m.encode(&ctxa, &mut |len| Tensor::from_vec(post.fill(64 * len), (1, 64, len), &Device::Cpu)).unwrap();
    let own_ctx = m.context(&own_src).unwrap();
    let mlx_ctx = to(&r["mlx.ctx"]);
    let own_noise = Tensor::from_vec(Normal::new(1234).fill(800 * 64), (1, 800, 64), &Device::Cpu).unwrap();
    eprintln!("src latents own vs mlx: own std {:?}", own_src.flatten_all().unwrap().to_device(&Device::Cpu).unwrap().to_vec1::<f32>().unwrap().iter().map(|v| v * v).sum::<f32>() / 51200.0);
    eprintln!("mlx src mean sq {:?}", r["mlx.ctx"].narrow(2, 0, 64).unwrap().sqr().unwrap().mean_all().unwrap().to_scalar::<f32>().unwrap());
    let _ = (&own_ctx, &own_noise);
    let torch = candle_core::safetensors::load(&a[5], &Device::Cpu).unwrap()["noise"].clone();
    let mut runs: Vec<(String, Tensor)> = vec![("torchnoise".into(), torch)];
    for s in [1u64, 2, 3, 5678] {
        runs.push((format!("own{s}"), Tensor::from_vec(Normal::new(s).fill(800 * 64), (1, 800, 64), &Device::Cpu).unwrap()));
    }
    for (name, noise) in runs.iter() {
        let (ctx, noise) = (&mlx_ctx, noise);
        let c = Conditioning { dit: m.dit(), context: ctx, kv: &kv };
        let mut apg = Apg::new(ApgMode::Mlx);
        let lat = sample_with(&c, noise, 50, 7.0, 3.0, &mut apg, &mut |_| true).unwrap().unwrap();
        let wav = m.decode(&lat).unwrap();
        candle_core::safetensors::save(&HashMap::from([("audio".to_string(), wav)]), out.join(format!("{name}.safetensors"))).unwrap();
        eprintln!("{name} done");
    }
}
