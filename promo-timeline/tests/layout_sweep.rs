//! Issue #9's guard against a checker an agent learns to ignore: every
//! tracked project — the examples (the quick-schema recipes among them)
//! and the demos' references — lays out without a layout warning. A new
//! check that fires here is firing on work that looks right.

use std::path::{Path, PathBuf};

fn projects(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            // A demo's runs/ are git-ignored agent output, not the repo's.
            if name != "target" && name != "runs" && !name.starts_with('.') {
                projects(&path, out);
            }
        } else if name == "metadata.json"
            || (name.starts_with("reference") && name.ends_with(".json"))
        {
            out.push(path);
        }
    }
}

#[test]
fn no_tracked_project_warns_about_its_layout() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for dir in ["examples", "demos"] {
        projects(&root.join(dir), &mut files);
    }
    let mut checked = 0;
    let mut found = Vec::new();
    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        let Ok(mut meta) = promo_model::ProjectMetadata::from_json(&text) else {
            continue;
        };
        promo_timeline::resolve_timing(&mut meta, &Default::default());
        for warning in promo_timeline::layout_check::layout_warnings(&meta) {
            found.push(format!("{}: {warning}", file.display()));
        }
        checked += 1;
    }
    // 7 examples and 40 demo references are tracked.
    assert!(checked > 40, "only {checked} projects");
    assert!(found.is_empty(), "{found:#?}");
}
