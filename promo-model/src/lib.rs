//! promo-model: the project data model (RecordingProject, layers, keyframes,
//! resources, composition settings, exports) as serde types, value-compatible
//! with the Swift app's `metadata.json` (fixtures harvested from real projects
//! are the parity gate — see `fixtures/projects/`).

pub mod geometry;
pub mod inventory;
pub mod nesting;
pub mod project;

pub use geometry::{Point, Rect, Size};
pub use inventory::{
    derived_id, effective_resources, layers_with_missing_media, ResolvedResource, ResourceOrigin,
};
pub use project::*;

/// The `metadata.json` schema this crate targets. Bumped only when the Swift
/// app changes its persisted format (both sides decode older payloads
/// tolerantly, mirroring the Swift decoders).
/// The project format, described for whoever is writing one by hand — an
/// assistant, a generator script, or a person.
///
/// It lives here rather than in the Mac app because the CLI has to serve it
/// too (`promo schema`), and two hand-maintained descriptions of one format
/// is how they start disagreeing. `MCPTools.schemaText` reads this through
/// the FFI, so the app and the CLI answer with the same bytes.
pub const SCHEMA: &str = include_str!("schema.md");
/// The authoring subset plus four complete recipes — what `promo_schema`
/// serves, sized to be READ rather than searched. Every fenced ```json
/// block in it is a whole project that must decode and validate clean;
/// a test extracts and runs them, so a recipe cannot rot as the format
/// moves. The full document stays the authority behind both.
pub const SCHEMA_QUICK: &str = include_str!("schema-quick.md");

/// The machine half of the format: a JSON Schema GENERATED from the same
/// serde structs the parser runs, so it cannot say anything the parser
/// would not accept. Types only — descriptions are stripped, because the
/// essay is `SCHEMA` and duplicating it here would just be a second copy
/// to rot. Structured-output models fill this; humans read the markdown.
pub fn wire_schema() -> serde_json::Value {
    let schema = schemars::schema_for!(ProjectMetadata);
    let mut value = serde_json::to_value(schema).unwrap_or_default();
    strip_descriptions(&mut value);
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "title".into(),
            serde_json::Value::String(".promo metadata.json".into()),
        );
    }
    value
}

/// The four complete projects embedded in `SCHEMA_QUICK`, in document
/// order — every ```json fence is one. The recipe tests, the machine-schema
/// tests and the examples' own golden all read THIS, so "the recipes" is
/// one list everywhere.
pub fn quick_schema_recipes() -> Vec<&'static str> {
    let mut out = Vec::new();
    let mut rest = SCHEMA_QUICK;
    while let Some(start) = rest.find("```json") {
        let body = &rest[start + 7..];
        let Some(end) = body.find("```") else { break };
        out.push(body[..end].trim());
        rest = &body[end + 3..];
    }
    out
}

fn strip_descriptions(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.remove("description");
            for entry in map.values_mut() {
                strip_descriptions(entry);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                strip_descriptions(item);
            }
        }
        _ => {}
    }
}

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

/// The drift guard for `schema.md`. [`SCHEMA`] is the format description the
/// CLI (`promo schema`) and the app's `promo_schema` tool both serve, and a
/// field the model writes but the doc never names is a feature no assistant
/// can author.
///
/// The kitchen-sink project below is built with EXHAUSTIVE struct literals —
/// no `..Default::default()` — so the moment the model grows a field this
/// module stops compiling, and whoever added the field decides right here:
/// give it a value and a mention in schema.md, or set `None` with a comment
/// saying why the doc deliberately leaves it out.
#[cfg(test)]
mod schema_doc_tests {
    use super::*;

    fn full_reveal() -> TextReveal {
        TextReveal {
            by: RevealUnit::Word,
            mode: RevealMode::Highlight,
            seconds_per: Some(0.12),
            seconds: Some(2.0),
            unit_seconds: Some(0.2),
            rise: Some(0.5),
            highlight_color_hex: Some("5B8CFF".into()),
            easing: Some(Easing::EaseInOut),
        }
    }

    fn full_caption_style() -> SubtitleStyle {
        SubtitleStyle {
            placement: Some(Placement {
                height: None,
                width: None,
                mode: None,
                anchor: Some(Anchor::Bottom),
                offset: Some([0.0, -40.0]),
            }),
            left_margin: Some(90.0),
            right_margin: Some(90.0),
            vertical_margin: Some(880.0),
            font_size: Some(96.0),
            alignment: Some(SubtitleTextAlignment::Trailing),
            padding: Some(20.0),
            corner_radius: Some(8.0),
            font_family: Some(SubtitleFontFamily::Rounded),
            is_bold: Some(true),
            is_italic: Some(false),
            text_color_hex: Some("FFFFFF".into()),
            background_color_hex: Some("000000".into()),
            background_opacity: Some(0.0),
            stroke_color_hex: Some("0A0A0A".into()),
            stroke_width: Some(6.0),
            shadow_color_hex: Some("000000".into()),
            shadow_opacity: Some(0.55),
            shadow_radius: Some(10.0),
            shadow_offset: Some([0.0, 4.0]),
            reveal: Some(full_reveal()),
        }
    }

    fn full_transition(kind: TransitionKind, from: TransitionEdge) -> LayerTransition {
        LayerTransition {
            kind,
            from: Some(from),
            duration: 0.5,
            easing: Some(Easing::EaseOut),
        }
    }

    fn full_frame() -> ResourceFrame {
        ResourceFrame {
            kind: ResourceFrameKind::Device,
            border_color_hex: "@edge".into(),
            border_width: 12.0,
            corner_radius: 24.0,
            material: FrameMaterial::NaturalTitanium,
            tilt_y: 12.0,
            tilt_x: -4.0,
            bezel_fraction: 0.03,
            depth_fraction: 0.06,
            shadow_color_hex: Some("@media-shadow".into()),
            shadow_opacity: Some(0.4),
            shadow_radius: Some(18.0),
            shadow_offset: Some([0.0, 8.0]),
        }
    }

    fn full_gradient() -> BackgroundGradient {
        BackgroundGradient {
            kind: GradientKind::Linear,
            stops: vec![
                GradientStop {
                    color_hex: "0B1026".into(),
                    at: 0.0,
                },
                GradientStop {
                    color_hex: "1B4A8B".into(),
                    at: 1.0,
                },
            ],
            start: Some(Point(0.0, 0.0)),
            end: Some(Point(1.0, 1.0)),
            repeat: Some(GradientRepeat::Mirror),
        }
    }

    /// One keyframe carrying every track the format can animate.
    fn full_keyframe() -> ProjectLayerKeyframe {
        ProjectLayerKeyframe {
            id: "K".into(),
            time: 1.0,
            wait: true,
            zoom: Some(0.8),
            font_size: Some(64.0),
            vertical_shift: Some(150.0),
            horizontal_shift: Some(240.0),
            color_hex: Some("@accent".into()),
            resource_id: Some("R2".into()),
            transition: Some(full_transition(TransitionKind::Push, TransitionEdge::Right)),
            gradient: Some(full_gradient()),
            gain: Some(0.8),
            rotation: Some(3.0),
            tilt_x: Some(6.0),
            tilt_y: Some(-4.0),
            opacity: Some(1.0),
            shutter: Some(0.5),
            saturation: Some(0.0),
            contrast: Some(1.1),
            brightness: Some(0.05),
            tint_amount: Some(0.4),
            mask_offset_x: Some(-320.0),
            mask_offset_y: Some(90.0),
            mask_zoom: Some(1.6),
            mask_zoom_y: Some(0.8),
            mask_rotation: Some(45.0),
            transition_duration: 0.5,
            transition_percent: Some(80.0),
            motion_path: Some(MotionPath {
                path_resource_id: "P".into(),
                flipped: Some(true),
                start_at: Some(0.0),
                end_at: Some(1.0),
            }),
            easing: Some(Easing::EaseInOut),
            viewport: Some([0.25, 0.25, 0.5, 0.5]),
            placement: Some(Placement {
                height: Some(620.0),
                width: Some(900.0),
                mode: Some(PlacementMode::Fit),
                anchor: Some(Anchor::BottomRight),
                offset: Some([0.0, -40.0]),
            }),
        }
    }

    fn full_layer() -> ProjectLayer {
        ProjectLayer {
            id: "L".into(),
            name: "Clip".into(),
            releases: vec![LayerRelease {
                layer_id: "VO".into(),
                on: None,
            }],
            sort_index: 1,
            kind: ProjectLayerKind::Video,
            is_enabled: true,
            start_time: 0.0,
            duration: Some(4.5),
            fade_in: Some(0.3),
            fade_out: Some(0.4),
            transition_in: Some(full_transition(TransitionKind::Wipe, TransitionEdge::Left)),
            transition_out: Some(full_transition(
                TransitionKind::Slide,
                TransitionEdge::Bottom,
            )),
            motion_blur: Some(MotionBlur { shutter: 0.5 }),
            adjustments: Some(LayerAdjustments {
                saturation: Some(0.0),
                contrast: Some(1.15),
                brightness: Some(0.06),
                tint_hex: Some("E8B380".into()),
                tint_amount: Some(0.4),
            }),
            blend_mode: Some(BlendMode::Screen),
            mask_resource_id: Some("M".into()),
            mask_inverted: Some(true),
            resource_id: Some("R".into()),
            // Legacy Swift bookkeeping, superseded by resourceID.
            image_filename: None,
            // Image cuts are app-made crops; the doc keeps them as opaque
            // editing state, so the layer's pointer into them stays out too.
            image_cut_id: None,
            media_cut_id: Some("C".into()),
            beyond_end: Some(BeyondEnd::Loop),
            // App-managed rotate applied at import, not an authoring field.
            image_orientation: None,
            image_border_color_hex: Some("26364F".into()),
            image_border_width: Some(1.0),
            caption_text: Some("A fast spreadsheet for Mac".into()),
            caption_style: Some(full_caption_style()),
            // Narration the app records against a caption — its own workflow.
            caption_voice_clip: None,
            // App-side audio ducking toggle, undocumented on purpose.
            audio_focus: None,
            duration_rule: Some(DurationRule {
                kind: DurationRuleKind::FitDependents,
                tail: Some(2.5),
            }),
            timing: Some(LayerTiming {
                start: Some(TimingAnchor {
                    from: TimingReference::PreviousStart,
                    offset: 0.45,
                }),
                end: Some(TimingAnchor {
                    from: TimingReference::PreviousEnd,
                    offset: 0.0,
                }),
            }),
            keyframes: vec![full_keyframe()],
            extra: serde_json::Map::new(),
        }
    }

    fn full_resource() -> ProjectResource {
        ProjectResource {
            id: "R".into(),
            kind: ProjectResourceKind::Image,
            filename: "walk.png".into(),
            display_name: "Walk".into(),
            added_at: 0.0,
            duration: Some(6.5),
            trim_start: Some(0.15),
            trim_end: Some(6.35),
            // The key is documented (a cut can skip its dull middle); the
            // entries' own fields are editor state the doc leaves opaque.
            trim_keyframes: Some(vec![]),
            caption_text: Some("Two lines, then two more".into()),
            // The full style is exercised on the layer above.
            caption_style: None,
            caption_voice_clip: None,
            drawing: Some(DrawingDocument {
                shapes: vec![DrawingShape {
                    id: "S".into(),
                    kind: DrawingShapeKind::Oval,
                    points: vec![Point(0.0, 0.0), Point(100.0, 100.0)],
                    stroke_color_hex: "FFFFFF".into(),
                    stroke_width: 1.0,
                    stroke_opacity: Some(1.0),
                    fill_color_hex: Some("FFFFFF".into()),
                    fill_opacity: Some(0.5),
                    arrow_start: false,
                    arrow_end: false,
                    even_odd_fill: Some(true),
                    // Editor-side shape grouping, not an authoring field.
                    group_id: None,
                }],
                // The drawing canvas backdrop the app's editor paints.
                background_color_hex: None,
            }),
            path: Some(PathDocument {
                start: Point(0.0, 0.0),
                end: Point(100.0, 0.0),
                controls: vec![Point(50.0, -60.0)],
            }),
            sprite: Some(SpriteSheet {
                columns: 4,
                rows: 2,
                frame_count: Some(7),
                fps: Some(12.0),
                frame_durations: Some(vec![0.1; 7]),
            }),
            sampling: Some(ResourceSampling::Nearest),
            image_cuts: vec![],
            speed: Some(1.1),
            media_cuts: vec![MediaCut {
                id: "C".into(),
                name: "The formula bit".into(),
                trim_start: Some(12.0),
                trim_end: Some(18.0),
                trim_keyframes: Some(vec![]),
                speed: Some(1.5),
                extra: serde_json::Map::new(),
            }],
            // Legacy 0…4 gain, migrated into volume on decode.
            audio_gain: None,
            volume: Some(0.9),
            disabled_audio_track_indices: vec![],
            video_natural_width: Some(1920.0),
            video_natural_height: Some(1080.0),
            pixel_width: Some(1170.0),
            pixel_height: Some(2532.0),
            frame: Some(full_frame()),
            background: None,
            composition: Some(Composition {
                canvas_width: 1920.0,
                canvas_height: 1080.0,
                background_color_hex: Some("0E1726".into()),
                layers: vec![],
            }),
            palette: None,
            looped: Some(true),
            extra: serde_json::Map::new(),
        }
    }

    fn full_settings() -> CompositionSettings {
        CompositionSettings {
            canvas_width: 1920.0,
            canvas_height: 1080.0,
            fps: Some(60.0),
            subtitle_left_margin: 90.0,
            subtitle_right_margin: 90.0,
            subtitle_vertical_margin: 880.0,
            subtitle_font_size: 54.0,
            subtitle_font_family: SubtitleFontFamily::System,
            subtitle_bold: true,
            subtitle_italic: false,
            subtitle_color_hex: "FFFFFF".into(),
            background_color_hex: "@ink".into(),
            background_gradient: Some(full_gradient()),
            palette: Some(vec![PaletteColor {
                name: "accent".into(),
                color_hex: "5B8CFF".into(),
            }]),
            palette_resource_id: None,
            subtitle_background_color_hex: "000000".into(),
            subtitle_background_opacity: 0.7,
            subtitle_background_padding: 16.0,
            subtitle_stroke_color_hex: "000000".into(),
            subtitle_stroke_width: 0.0,
            subtitle_shadow_color_hex: "000000".into(),
            subtitle_shadow_opacity: 0.0,
            subtitle_shadow_radius: 0.0,
            subtitle_shadow_offset: Some([0.0, 4.0]),
            subtitle_reveal: Some(full_reveal()),
            subtitle_background_corner_radius: 8.0,
            subtitle_alignment: SubtitleTextAlignment::Center,
            video_border_color_hex: "26364F".into(),
            video_border_width: 1.0,
            video_corner_radius: 14.0,
            video_shadow_color_hex: "@accent".into(),
            video_shadow_opacity: 0.35,
            video_shadow_radius: 24.0,
            video_shadow_offset: Some([0.0, 12.0]),
            video_keyframes: vec![],
            background_keyframes: vec![],
            image_export_scale_percent: 100.0,
            gif_export_scale_percent: 33.0,
            gif_export_fps: 10.0,
            video_export_width: None,
            video_export_height: None,
        }
    }

    pub(crate) fn kitchen_sink() -> ProjectMetadata {
        ProjectMetadata {
            id: "T".into(),
            name: "Everything".into(),
            created_at: 0.0,
            subtitles: vec![],
            trim_start: 0.0,
            trim_end: 0.0,
            // The pre-layer whole-video trim, app bookkeeping.
            trim_keyframes: None,
            video_duration: 0.0,
            composition_settings: full_settings(),
            layers: Some(vec![full_layer()]),
            resources: Some(vec![full_resource()]),
            // Export history the app appends; never authored.
            exports: None,
            markers: Some(vec![Marker {
                id: "M1".into(),
                time: 2.5,
                name: "Chapter one".into(),
                kind: MarkerKind::Chapter,
                color_hex: Some("@accent".into()),
            }]),
            updated_at: None,
            state: "recorded".into(),
            // Writer bookkeeping; the doc teaches minReaderVersion only.
            format_version: None,
            min_reader_version: Some(13),
            extra: serde_json::Map::new(),
        }
    }

    /// Always-encoded keys the doc deliberately leaves out: the pre-layer
    /// legacy timeline and export bookkeeping the app manages.
    const UNDOCUMENTED: &[&str] = &[
        "videoKeyframes",
        "backgroundKeyframes",
        "imageExportScalePercent",
        "gifExportScalePercent",
        "gifExportFPS",
    ];

    /// Enum wire values an author has to spell exactly. Extend alongside the
    /// enums; every entry must appear in schema.md verbatim.
    const VALUES: &[&str] = &[
        "chapter",
        "composition",
        // Layer and resource kinds.
        "background",
        "video",
        "image",
        "drawing",
        "caption",
        "audio",
        "path",
        // Transition kinds and edges.
        "fade",
        "wipe",
        "slide",
        "push",
        "scale",
        "left",
        "right",
        "top",
        "bottom",
        // Easing.
        "linear",
        "easeIn",
        "easeOut",
        "easeInOut",
        // Reveal units and modes.
        "character",
        "word",
        "line",
        "rise",
        "highlight",
        // Blend modes.
        "normal",
        "multiply",
        "screen",
        "add",
        // beyondEnd.
        "hold",
        "loop",
        "hide",
        // Timing anchor references.
        "previousStart",
        "previousEnd",
        "nextStart",
        "nextEnd",
        "previousPeerStart",
        "previousPeerEnd",
        "nextPeerStart",
        "nextPeerEnd",
        // Duration rules.
        "fitContent",
        "fitDependents",
        // Sampling.
        "smooth",
        "nearest",
        // Placement anchors and modes.
        "topLeft",
        "topRight",
        "center",
        "bottomLeft",
        "bottomRight",
        "fit",
        "fill",
        // Resource frames: the kind, plus every body material.
        "border",
        "device",
        "phone",
        "spaceBlack",
        "naturalTitanium",
        "silver",
        "gold",
        "deepBlue",
        "plasticWhite",
        "plasticBlack",
        "plasticBlue",
        "plasticRed",
        "plasticGreen",
        "plasticYellow",
        "plasticPink",
        // Gradients.
        "radial",
        "clamp",
        "repeat",
        "mirror",
        // Caption alignment and font families.
        "leading",
        "trailing",
        "system",
        "rounded",
        "serif",
        "monospaced",
        "helveticaNeue",
        "avenirNext",
        "gillSans",
        "futura",
        "trebuchetMS",
        "georgia",
        "palatino",
        "timesNewRoman",
        "americanTypewriter",
        "courierNew",
        "chalkboard",
        "markerFelt",
        "snellRoundhand",
        // Drawing shapes.
        "pen",
        "oval",
    ];

    fn collect_keys(value: &serde_json::Value, keys: &mut std::collections::BTreeSet<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, inner) in map {
                    keys.insert(key.clone());
                    collect_keys(inner, keys);
                }
            }
            serde_json::Value::Array(items) => {
                for inner in items {
                    collect_keys(inner, keys);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn schema_md_mentions_every_authorable_wire_field() {
        let meta = kitchen_sink();
        let value = serde_json::to_value(&meta).expect("kitchen sink encodes");
        let mut keys = std::collections::BTreeSet::new();
        collect_keys(&value, &mut keys);
        // Guard the guard: if the walk ever sees a near-empty document, the
        // test would pass by looking at nothing.
        assert!(keys.len() > 120, "only {} keys collected", keys.len());

        let mut missing: Vec<String> = keys
            .iter()
            .filter(|k| !UNDOCUMENTED.contains(&k.as_str()) && !SCHEMA.contains(k.as_str()))
            .map(|k| format!("field \"{k}\""))
            .collect();
        missing.extend(
            VALUES
                .iter()
                .filter(|v| !SCHEMA.contains(**v))
                .map(|v| format!("value \"{v}\"")),
        );
        assert!(
            missing.is_empty(),
            "schema.md never mentions: {} — either document it there or mark \
             it deliberately undocumented in this test",
            missing.join(", ")
        );
    }

    /// The richest project this crate can express round-trips exactly — the
    /// same fixed-point contract the fixtures prove for real saves.
    #[test]
    fn the_kitchen_sink_round_trips() {
        let meta = kitchen_sink();
        let back = ProjectMetadata::from_json(&meta.to_json().expect("encodes")).expect("decodes");
        assert_eq!(back, meta);
    }
}

#[cfg(test)]
mod doctrine_tests {
    /// The full schema is the doc nobody re-reads after a rule reverses —
    /// the quick subset gets rewritten, the old paragraph deep in the
    /// essay survives, and anyone who reads promo_schema_full believes
    /// last month's format. Each phrase here is the tombstone of a
    /// REPEALED doctrine: it must never reappear in either document.
    /// When a rule reverses, bury its old wording here.
    #[test]
    fn no_document_preaches_a_repealed_doctrine() {
        let tombstones = [
            // Short ids became first-class when the apps grew the minting
            // door; the app refusing them is last month's truth.
            "the project will not open",
            "the app cannot decode",
            "EVERY id in the file is a UUID",
            "Generate UUIDs",
            // Repealed by SPECS D5 stages 1-3: the file is shared and every
            // writer merges — one-way ownership is last week's truth.
            "owns the file once it opens it",
            "owns the file once it has opened it",
        ];
        for (name, doc) in [
            ("SCHEMA", super::SCHEMA),
            ("SCHEMA_QUICK", super::SCHEMA_QUICK),
        ] {
            for phrase in tombstones {
                assert!(
                    !doc.contains(phrase),
                    "{name} still preaches a repealed doctrine: \"{phrase}\""
                );
            }
        }
    }
}

#[cfg(test)]
mod wire_schema_tests {
    use super::*;

    /// The generated schema must accept everything the format calls a
    /// project: the fully populated fixture (every wire key), and all four
    /// quick-schema recipes. Held with a real validator, so "generated
    /// from the structs" is proven against documents, not assumed.
    #[test]
    fn the_recipes_and_the_full_fixture_satisfy_the_machine_schema() {
        let schema = wire_schema();
        let validator = jsonschema::validator_for(&schema).expect("schema compiles");

        let recipes = quick_schema_recipes();
        assert_eq!(recipes.len(), 4);
        for (index, text) in recipes.iter().enumerate() {
            let recipe: serde_json::Value = serde_json::from_str(text).expect("recipe parses");
            let errors: Vec<String> = validator
                .iter_errors(&recipe)
                .map(|e| format!("{} at {}", e, e.instance_path))
                .collect();
            assert!(errors.is_empty(), "recipe {index}: {errors:?}");
        }

        let full =
            serde_json::to_value(super::schema_doc_tests::kitchen_sink()).expect("fixture encodes");
        let errors: Vec<String> = validator
            .iter_errors(&full)
            .map(|e| format!("{} at {}", e, e.instance_path))
            .collect();
        assert!(errors.is_empty(), "full fixture: {errors:?}");
    }

    /// Each recipe ships as a runnable example — synthetic media included —
    /// and the example's metadata.json IS the recipe, byte for byte. Edit
    /// either and this test walks you to the other; the doc can never teach
    /// one thing while examples/ demonstrates another.
    #[test]
    fn the_examples_are_the_recipes() {
        let examples = [
            (
                "ProductCard",
                include_str!("../../examples/ProductCard.promo/metadata.json"),
            ),
            (
                "TwoClips",
                include_str!("../../examples/TwoClips.promo/metadata.json"),
            ),
            (
                "FocusPush",
                include_str!("../../examples/FocusPush.promo/metadata.json"),
            ),
            (
                "Story",
                include_str!("../../examples/Story.promo/metadata.json"),
            ),
        ];
        let recipes = quick_schema_recipes();
        assert_eq!(recipes.len(), examples.len());
        for ((name, staged), recipe) in examples.iter().zip(recipes) {
            assert_eq!(
                staged.trim(),
                recipe,
                "examples/{name}.promo drifted from its recipe in schema-quick.md"
            );
        }
    }

    /// The checked-in copy (docs/promo.schema.json — what a metadata.json
    /// can reference as $schema for editor autocomplete) must BE the
    /// generator's output. Regenerate with:
    ///   cargo run -p promo-cli -- schema --types > docs/promo.schema.json
    #[test]
    fn the_checked_in_schema_is_the_generated_one() {
        let committed = include_str!("../../docs/promo.schema.json");
        let generated = serde_json::to_string_pretty(&wire_schema()).expect("serializes") + "\n";
        assert_eq!(
            committed, generated,
            "docs/promo.schema.json is stale — regenerate with \
             `cargo run -p promo-cli -- schema --types > docs/promo.schema.json`"
        );
    }

    /// Types only: the essay stays in the markdown. A description leaking
    /// in means the doc-comment stripper broke, and two copies of the
    /// prose begin to drift.
    #[test]
    fn the_machine_schema_carries_no_prose() {
        let text = wire_schema().to_string();
        assert!(!text.contains("\"description\""), "types only, as promised");
    }
}
