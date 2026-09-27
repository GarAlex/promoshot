//! Every project in the repository, as this build writes it, carries
//! nothing this build does not read — so the lossless-reader stamp floor
//! (`unread::floor_stamp`) never fires on a file the format fully knows.
//! A key a hand-written serializer adds without the schema knowing it, or
//! a value missing from a closed list, fails here with its path. A demo's
//! `runs/` are agents' past output, not the format's, and stay out.

use std::path::{Path, PathBuf};

use promo_model::{quick_schema_recipes, unread, ProjectMetadata};

fn project_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name != "target" && name != "runs" && !name.starts_with('.') {
                project_files(&path, out);
            }
        } else if name.ends_with(".json") {
            // Whatever decodes as a project below is one: metadata.json,
            // the fixtures, the demos' references, the wizard's goldens.
            out.push(path);
        }
    }
}

#[test]
fn every_project_in_the_repository_is_fully_read() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    project_files(&root, &mut files);
    assert!(files.len() > 20, "found only {} project files", files.len());
    let mut texts: Vec<(String, String)> = files
        .iter()
        .filter_map(|f| Some((f.display().to_string(), std::fs::read_to_string(f).ok()?)))
        .collect();
    texts.extend(
        quick_schema_recipes()
            .into_iter()
            .enumerate()
            .map(|(i, r)| (format!("recipe {i}"), r.to_string())),
    );
    let mut unread = Vec::new();
    let mut checked = 0;
    for (name, text) in &texts {
        let Ok(meta) = ProjectMetadata::from_json(text) else {
            continue;
        };
        let written: serde_json::Value = serde_json::from_str(&meta.to_json().unwrap()).unwrap();
        checked += 1;
        if let Some(path) = unread::first_unread(&written) {
            unread.push(format!("{name}: {path}"));
        }
    }
    assert!(checked > 20, "decoded only {checked}");
    // The one fixture that carries a value from a newer build on purpose
    // (its tolerant-decode case) is the detector's positive control.
    let control = "project-4.json: exports[1].kind = \"hologram\"";
    assert!(
        unread.iter().any(|u| u.ends_with(control)),
        "the detector missed the fixture's unknown value: {unread:?}"
    );
    unread.retain(|u| !u.ends_with(control));
    assert!(
        unread.is_empty(),
        "carries what this build does not read:\n{}",
        unread.join("\n")
    );
}
