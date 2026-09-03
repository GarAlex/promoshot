//! `cargo run -p promo-engine --example write_slab -- slab.glb`: the
//! generated phone slab (Body and Screen slots), as a file for a template.
fn main() {
    let out = std::env::args().nth(1).expect("path");
    std::fs::write(&out, promo_engine::model::sample_slab_glb()).expect("write");
    println!("wrote {out}");
}
