// Dev-only: times the VAE's heavy ops on the default device.
use candle_core::{DType, Device, Tensor};
use std::time::Instant;
fn sync(t: &Tensor) { let _ = t.sum_all().unwrap().to_dtype(DType::F32).unwrap().to_scalar::<f32>().unwrap(); }
fn main() {
    let dev = promo_music::default_device();
    let dt = promo_music::default_dtype(&dev);
    for &(c, len, k, dil) in &[(128usize, 491_520usize, 7usize, 1usize), (128, 491_520, 7, 9), (1024, 4096, 7, 1), (128, 491_520, 1, 1)] {
        let x = Tensor::randn(0f32, 1., (1, c, len), &dev).unwrap().to_dtype(dt).unwrap();
        let w = Tensor::randn(0f32, 0.1, (c, c, k), &dev).unwrap().to_dtype(dt).unwrap();
        sync(&x.conv1d(&w, (k-1)*dil/2, 1, dil, 1).unwrap());
        let t = Instant::now();
        let y = x.conv1d(&w, (k-1)*dil/2, 1, dil, 1).unwrap(); sync(&y);
        println!("conv1d c{c} len{len} k{k} d{dil}: {:.1?}", t.elapsed());
    }
    for &(cin, cout, len, s) in &[(256usize, 128usize, 245_760usize, 2usize), (2048, 1024, 256, 10), (512, 256, 61_440, 4)] {
        let x = Tensor::randn(0f32, 1., (1, cin, len), &dev).unwrap().to_dtype(dt).unwrap();
        let w = Tensor::randn(0f32, 0.1, (cin, cout, 2*s), &dev).unwrap().to_dtype(dt).unwrap();
        let r = x.conv_transpose1d(&w, s.div_ceil(2), 0, s, 1, 1).unwrap();
        sync(&promo_music::vae::conv_transpose_k2s(&x, &w, s, s / 2).unwrap());
        let t = Instant::now();
        let y = promo_music::vae::conv_transpose_k2s(&x, &w, s, s / 2).unwrap(); sync(&y);
        let e = (&y - &r).unwrap().abs().unwrap().flatten_all().unwrap().max(0).unwrap().to_dtype(DType::F32).unwrap().to_scalar::<f32>().unwrap();
        println!("convT(matmul) cin{cin} cout{cout} len{len} s{s}: {:.1?} maxdiff {e:.2e}", t.elapsed());
    }
    let x = Tensor::randn(0f32, 1., (1, 128, 491_520), &dev).unwrap().to_dtype(dt).unwrap();
    let a = Tensor::ones((1,128,1), dt, &dev).unwrap();
    for _ in 0..2 {
    let t = Instant::now();
    let y = (&x + x.broadcast_mul(&a).unwrap().sin().unwrap().sqr().unwrap().broadcast_mul(&a).unwrap()).unwrap(); sync(&y);
    println!("snake 128x491520: {:.1?}", t.elapsed());
    }
    let _ = Device::Cpu;
}
