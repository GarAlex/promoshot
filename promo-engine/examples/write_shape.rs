//! `cargo run -p promo-engine --example write_shape -- sphere out.glb`: one
//! of the curved bodies (sphere, torus, vase) as a file.
fn main() {
    let mut args = std::env::args().skip(1);
    let kind = args.next().expect("sphere | torus | vase");
    let out = args.next().expect("path");
    let kind = promo_engine::model::ShapeKind::parse(&kind).expect("sphere | torus | vase");
    std::fs::write(&out, promo_engine::model::shape_glb(kind)).expect("write");
    println!("wrote {out}");
}
