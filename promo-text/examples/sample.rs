// Rasterizes one headline to a PNG for A/B against a platform text stack.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let family = args.first().cloned();
    let out = args.get(1).cloned().unwrap_or_else(|| "sample.png".into());
    let smoothing = args.get(2).and_then(|s| s.parse::<f64>().ok());
    let style = promo_text::TextStyle {
        font_family: family.clone(),
        font_size: 54.0,
        bold: true,
        align: promo_text::Align::Center,
        text_rgba: [255, 255, 255, 255],
        background_rgba: [0x0E, 0x17, 0x26, 255],
        padding: 40.0,
        corner_radius: 0.0,
        left_margin: 0.0,
        right_margin: 0.0,
        vertical_margin: 40.0,
        smoothing,
        ..Default::default()
    };
    let r = promo_text::rasterize("Formulas without friction", 1920.0, 200.0, &style)
        .expect("rasterized");
    println!("family={:?} -> {}x{}", family, r.width, r.height);
    image::save_buffer(&out, &r.rgba, r.width, r.height, image::ColorType::Rgba8).unwrap();
}
