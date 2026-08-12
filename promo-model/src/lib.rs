//! promo-model: the project data model (RecordingProject, layers, keyframes,
//! resources, composition settings, exports) as serde types, value-compatible
//! with the Swift app's `metadata.json` (fixtures harvested from real projects
//! are the parity gate — see `fixtures/projects/`).

pub mod geometry;
pub mod project;

pub use geometry::{Point, Rect, Size};
pub use project::*;

/// The `metadata.json` schema this crate targets. Bumped only when the Swift
/// app changes its persisted format (both sides decode older payloads
/// tolerantly, mirroring the Swift decoders).
pub const METADATA_SCHEMA: u32 = 1;

/// Core library version, surfaced through the FFI for the host gate test.
pub fn core_version() -> &'static str {
    concat!("promo-core ", env!("CARGO_PKG_VERSION"))
}

/// Semantic JSON equality: numbers compare as f64 values (so `3` == `3.0`),
/// everything else compares structurally. This is the round-trip yardstick —
/// Swift's JSONEncoder and serde_json format numbers differently but must
/// mean the same values.
pub fn json_semantically_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    use serde_json::Value::*;
    match (a, b) {
        (Number(x), Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(fx), Some(fy)) => fx == fy || (fx.is_nan() && fy.is_nan()),
            _ => x == y,
        },
        (Array(xs), Array(ys)) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys.iter())
                    .all(|(x, y)| json_semantically_equal(x, y))
        }
        (Object(xs), Object(ys)) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .all(|(k, x)| ys.get(k).is_some_and(|y| json_semantically_equal(x, y)))
        }
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_string_is_stable_prefix() {
        assert!(core_version().starts_with("promo-core "));
    }

    fn fixture(name: &str) -> String {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../fixtures/projects/");
        std::fs::read_to_string(format!("{path}{name}")).expect("fixture readable")
    }

    /// Decode → encode → decode must be a fixed point: the second decode sees
    /// exactly what the first produced (migrations only fire on legacy input).
    /// `legacyEndTime` is in-memory only in Swift too — it never re-encodes.
    #[test]
    fn fixtures_round_trip_idempotently() {
        for name in [
            "project-1.json",
            "project-2.json",
            "project-3.json",
            "project-4.json",
        ] {
            let mut first = ProjectMetadata::from_json(&fixture(name))
                .unwrap_or_else(|e| panic!("{name}: decode failed: {e}"));
            let encoded = first.to_json().expect("encode");
            for sub in &mut first.subtitles {
                sub.legacy_end_time = None;
            }
            let second = ProjectMetadata::from_json(&encoded)
                .unwrap_or_else(|e| panic!("{name}: re-decode failed: {e}"));
            assert_eq!(first, second, "{name}: round trip not a fixed point");

            // And the re-encoded JSON is value-identical to the first encode.
            let v1: serde_json::Value = serde_json::from_str(&encoded).unwrap();
            let v2: serde_json::Value = serde_json::from_str(&second.to_json().unwrap()).unwrap();
            assert!(json_semantically_equal(&v1, &v2), "{name}: encode drift");
        }
    }

    /// `a` is semantically contained in `b`: every key/value in `a` appears in
    /// `b` (numbers as f64). Extra keys in `b` are allowed — they are the
    /// decode-time defaults Swift also materializes on its next save.
    fn json_contained_in(a: &serde_json::Value, b: &serde_json::Value) -> Result<(), String> {
        use serde_json::Value::*;
        match (a, b) {
            (Object(xs), Object(ys)) => {
                for (k, x) in xs {
                    let y = ys.get(k).ok_or_else(|| format!("missing key {k}"))?;
                    json_contained_in(x, y).map_err(|e| format!("{k}.{e}"))?;
                }
                Ok(())
            }
            (Array(xs), Array(ys)) => {
                if xs.len() != ys.len() {
                    return Err(format!("array len {} vs {}", xs.len(), ys.len()));
                }
                for (i, (x, y)) in xs.iter().zip(ys.iter()).enumerate() {
                    json_contained_in(x, y).map_err(|e| format!("[{i}].{e}"))?;
                }
                Ok(())
            }
            _ => {
                if json_semantically_equal(a, b) {
                    Ok(())
                } else {
                    Err(format!("value {a} vs {b}"))
                }
            }
        }
    }

    /// Legacy migrations fire exactly like the Swift decoders.
    #[test]
    fn synthetic_fixture_migrations_match_swift() {
        let p = ProjectMetadata::from_json(&fixture("project-4.json")).unwrap();
        let resources = p.resources.as_ref().unwrap();

        // audioGain 2.5 (no volume) migrates to volume clamped to 1.0.
        let video = &resources[0];
        assert_eq!(video.volume, Some(1.0));
        assert_eq!(video.audio_gain, Some(2.5));
        // disabledAudioTrackIndices [2, 0, 2, -1] sanitizes to [0, 2].
        assert_eq!(video.disabled_audio_track_indices, vec![0, 2]);
        assert!(video.is_looped());

        // Image-cut rect [[0.1,-0.2],[0.5,1.6]] normalizes into the unit square.
        let cut = &resources[1].image_cuts[0];
        assert!((cut.rect.x() - 0.1).abs() < 1e-12);
        assert_eq!(cut.rect.y(), 0.0);
        assert!((cut.rect.width() - 0.5).abs() < 1e-12);
        assert_eq!(cut.rect.height(), 1.0);
        // Legacy frame kind "phone" folds into Device.
        assert_eq!(cut.frame.as_ref().unwrap().kind, ResourceFrameKind::Device);

        // Legacy subtitle startTime/endTime: time adopts startTime; endTime
        // is held only in memory and dropped on encode.
        assert_eq!(p.subtitles[1].time, 4.5);
        assert_eq!(p.subtitles[1].legacy_end_time, Some(6.5));
        let re: serde_json::Value = serde_json::from_str(&p.to_json().unwrap()).unwrap();
        let sub = &re["subtitles"][1];
        assert!(sub.get("endTime").is_none());
        assert!(sub.get("startTime").is_none());
        assert_eq!(sub["time"].as_f64(), Some(4.5));

        // Unknown export kind decodes tolerantly as Images.
        assert_eq!(
            p.exports.as_ref().unwrap()[1].kind,
            ProjectExportKind::Images
        );
    }

    /// Real projects saved by the Swift app decode without loss: re-encoding
    /// preserves every field the app wrote. (Fields added since the save gain
    /// their defaults, exactly as the Swift decoder materializes them — full
    /// Swift-encode vs Rust-encode equality is the parity harness's job.)
    #[test]
    fn real_fixtures_preserve_all_swift_written_fields() {
        for name in ["project-1.json", "project-2.json", "project-3.json"] {
            let raw = fixture(name);
            let original: serde_json::Value = serde_json::from_str(&raw).unwrap();
            let decoded = ProjectMetadata::from_json(&raw).unwrap();
            let reencoded: serde_json::Value =
                serde_json::from_str(&decoded.to_json().unwrap()).unwrap();
            if let Err(e) = json_contained_in(&original, &reencoded) {
                panic!("{name}: dropped/changed Swift-written field: {e}");
            }
        }
    }
}
