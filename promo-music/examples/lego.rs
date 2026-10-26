// Dev-only: one lego run on a context stored as safetensors ("audio",
// (2, N) or (1, 2, N) f32), written as safetensors ("audio", (2, N)).
// usage: lego <models/ace-step-base> <ctx.safetensors> <out.safetensors> <track> <caption> <seed>
use candle_core::{Device, Tensor};
use std::collections::HashMap;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dev = promo_music::default_device();
    let dtype = promo_music::default_dtype(&dev);
    let m = promo_music::pipeline::AceStep::load(std::path::Path::new(&a[1]), dev, dtype).unwrap();
    let t = candle_core::safetensors::load(&a[2], &Device::Cpu).unwrap()["audio"].clone();
    let t = t.reshape((2, t.elem_count() / 2)).unwrap();
    let req = promo_music::pipeline::Lego {
        left: t.get(0).unwrap().to_vec1().unwrap(),
        right: t.get(1).unwrap().to_vec1().unwrap(),
        track_name: a[4].clone(),
        caption: a[5].clone(),
        keyscale: Some("C minor".into()),
        bpm: 120.0,
        time_signature: 4,
        seed: a[6].parse().unwrap(),
        steps: 50,
        guidance: 7.0,
        shift: 3.0,
    };
    let mut tm = promo_music::pipeline::Timings::default();
    let (l, r) = m.lego(&req, &mut |_| {}, &|| false, &mut tm).unwrap().unwrap();
    eprintln!("{tm:?}");
    let n = l.len();
    let out = Tensor::from_vec([l, r].concat(), (2, n), &Device::Cpu).unwrap();
    candle_core::safetensors::save(&HashMap::from([("audio".to_string(), out)]), &a[3]).unwrap();
}
