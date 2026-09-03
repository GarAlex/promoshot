//! `cargo run -p promo-engine --example write_cube -- out.glb`: the sample
//! cube every model test uses, as a file — for a scratch project or a demo.
fn main() {
    let out = std::env::args().nth(1).expect("path");
    std::fs::write(&out, promo_engine::model::sample_cube_glb()).expect("write");
    println!("wrote {out}");
}
