//! `cargo run -p promo-engine --example write_device -- phone out.glb`: one
//! of the built-in device bodies (phone, tablet, laptop) as a file.
fn main() {
    let mut args = std::env::args().skip(1);
    let kind = args.next().expect("phone | tablet | laptop");
    let out = args.next().expect("path");
    let kind = promo_engine::model::DeviceKind::parse(&kind).expect("phone | tablet | laptop");
    std::fs::write(&out, promo_engine::model::device_glb(kind)).expect("write");
    println!("wrote {out}");
}
