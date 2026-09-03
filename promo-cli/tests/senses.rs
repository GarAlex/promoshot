//! The model senses through the built binary: `promo model` and
//! `promo turntable`, as the MCP shells out to them.

use std::process::Command;

fn gpu_available() -> bool {
    promo_gpu::GpuContext::shared().is_some()
}

/// `promo model` names the slab's slots and the turning cube's clip;
/// `promo turntable` writes a sheet of N cells whose corners are the
/// ground and whose middles are the model.
#[test]
fn the_model_senses_probe_and_turn() {
    if !gpu_available() {
        eprintln!("no GPU adapter; skipping");
        return;
    }
    let dir = std::env::temp_dir().join(format!("promo-senses-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let slab = dir.join("slab.glb");
    std::fs::write(&slab, promo_engine::model::sample_slab_glb()).unwrap();
    let cube = dir.join("turn.glb");
    std::fs::write(&cube, promo_engine::model::sample_turning_cube_glb()).unwrap();
    let bin = env!("CARGO_BIN_EXE_promo");

    let probe = Command::new(bin)
        .args(["model", slab.to_str().unwrap(), "--json"])
        .output()
        .expect("run");
    assert!(
        probe.status.success(),
        "{}",
        String::from_utf8_lossy(&probe.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
    let slots: Vec<&str> = json["slots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(slots, ["Body", "Screen"]);

    let probe = Command::new(bin)
        .args(["model", cube.to_str().unwrap(), "--json"])
        .output()
        .expect("run");
    let json: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
    assert_eq!(json["clips"][0]["name"], "Turn");

    let sheet = dir.join("sheet.png");
    let turn = Command::new(bin)
        .args([
            "turntable",
            slab.to_str().unwrap(),
            "--out",
            sheet.to_str().unwrap(),
            "--count",
            "4",
            "--size",
            "96x96",
            "--json",
        ])
        .output()
        .expect("run");
    assert!(
        turn.status.success(),
        "{}",
        String::from_utf8_lossy(&turn.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&turn.stdout).unwrap();
    assert_eq!(json["cells"].as_array().unwrap().len(), 4);
    let img = image::open(&sheet).expect("sheet").to_rgba8();
    assert_eq!((img.width(), img.height()), (192, 192));
    let corner = img.get_pixel(1, 1);
    let middle = img.get_pixel(48, 48);
    assert_ne!(
        corner.0[..3],
        middle.0[..3],
        "the cell's middle is the model, its corner the ground"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
