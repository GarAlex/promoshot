//! Beat generators, slice 6 (docs/BEAT-SYNC.md §4): `pulse`, `shake` and
//! `flash` expand into owned, tagged keyframes and one generated drawing
//! layer during resolution; unedited they never reach a file; a person's
//! edit makes them theirs.

use promo_model::musical::{self, BeatSection};
use promo_model::{ProjectLayerKeyframe, ProjectMetadata};
use promo_timeline::{resolve_timing, BeatGrids, TimingProblem};
use serde_json::{json, Value};

/// 120 BPM 4/4 from 0 s: a beat is 0.5 s, a bar 2 s; 30 fps.
fn project(layer_extra: Value, settings_extra: Value) -> Value {
    let mut layer = json!({"id": "a", "name": "A", "sortIndex": 1, "kind": "image",
                           "isEnabled": true, "startTime": 0, "duration": 4,
                           "resourceID": "pic", "keyframes": []});
    for (k, v) in layer_extra.as_object().unwrap() {
        layer[k] = v.clone();
    }
    let mut settings = json!({"canvasWidth": 160, "canvasHeight": 90, "fps": 30,
                              "beatClock": {"resourceID": "song", "startTime": 0}});
    for (k, v) in settings_extra.as_object().unwrap() {
        settings[k] = v.clone();
    }
    json!({
        "id": "P", "name": "P", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 4, "videoDuration": 4, "subtitles": [],
        "compositionSettings": settings,
        "layers": [
            {"id": "bg", "name": "Ground", "sortIndex": 0, "kind": "background",
             "isEnabled": true, "startTime": 0, "duration": 4, "keyframes": []},
            layer
        ],
        "resources": [
            {"id": "song", "kind": "audio", "filename": "s.wav", "displayName": "S",
             "addedAt": 0, "beatGrid": {"bpm": 120}},
            {"id": "pic", "kind": "image", "filename": "p.png", "displayName": "P",
             "addedAt": 0, "pixelWidth": 160, "pixelHeight": 90}
        ]
    })
}

fn resolved(file: &Value) -> ProjectMetadata {
    resolved_with(file, &BeatGrids::default()).0
}

fn resolved_with(file: &Value, grids: &BeatGrids) -> (ProjectMetadata, Vec<TimingProblem>) {
    let mut meta = ProjectMetadata::from_json(&file.to_string()).unwrap();
    let report = resolve_timing(&mut meta, grids);
    (meta, report.problems)
}

fn layer<'a>(meta: &'a ProjectMetadata, id: &str) -> &'a promo_model::ProjectLayer {
    meta.layers
        .as_ref()
        .unwrap()
        .iter()
        .find(|l| l.id == id)
        .unwrap()
}

fn key<'a>(meta: &'a ProjectMetadata, id: &str) -> &'a ProjectLayerKeyframe {
    layer(meta, "a")
        .keyframes
        .iter()
        .find(|k| k.id == id)
        .unwrap_or_else(|| panic!("no keyframe {id}"))
}

fn close(a: [f64; 4], b: [f64; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9)
}

fn generated(k: &ProjectLayerKeyframe) -> bool {
    k.auto
        .as_deref()
        .is_some_and(|a| a.starts_with(musical::BEAT_TAG))
}

/// A viewport pulse on every downbeat: a punched window ON the beat, the
/// base window a decay later, eased out — every key tagged as the
/// generator's, its tag the signature of its values.
#[test]
fn pulse_expands_to_tagged_viewport_keys() {
    let meta = resolved(&project(
        json!({"pulse": {"every": "downbeat", "amount": 0.25}}),
        json!({}),
    ));
    let a = layer(&meta, "a");
    assert_eq!(a.keyframes.len(), 4, "{:?}", a.keyframes);
    for (bar, at) in [(0, 0.0), (1, 2.0)] {
        let peak = key(&meta, &format!("beat-a-pulse0-{bar}-peak"));
        assert_eq!(peak.time, at);
        assert!(
            (peak.transition_duration - 1.0 / 30.0).abs() < 1e-12,
            "a frame's attack"
        );
        assert!(close(peak.viewport.unwrap(), [0.1, 0.1, 0.8, 0.8]));
        let rest = key(&meta, &format!("beat-a-pulse0-{bar}-rest"));
        assert_eq!(rest.time, at + 0.25, "half a beat of decay");
        assert!(close(rest.viewport.unwrap(), [0.0, 0.0, 1.0, 1.0]));
        assert_eq!(rest.easing, Some(promo_model::Easing::EaseOut));
    }
    for k in &a.keyframes {
        let value = serde_json::to_value(k).unwrap();
        assert_eq!(
            musical::is_unedited_beat_keyframe(&value),
            Some(true),
            "{value}"
        );
    }
}

/// Over a keyed track the pulse rides the person's own motion: the base
/// is sampled from their keyframes, and a base key holds the line just
/// before each punch.
#[test]
fn pulse_over_a_keyed_track_follows_the_base() {
    let meta = resolved(&project(
        json!({"pulse": {"every": "bar", "amount": 0.25, "range": {"bar": 2, "bars": 1}},
        "keyframes": [
            {"id": "v0", "time": 0, "transitionDuration": 0, "viewport": [0, 0, 1, 1]},
            {"id": "v1", "time": 4, "transitionDuration": 4, "viewport": [0.5, 0.5, 0.5, 0.5]}
        ]}),
        json!({}),
    ));
    let at = |t: f64| {
        let f = t / 4.0;
        [0.5 * f, 0.5 * f, 1.0 - 0.5 * f, 1.0 - 0.5 * f]
    };
    let base = key(&meta, "beat-a-pulse0-0-base");
    assert!((base.time - (2.0 - 1.0 / 30.0)).abs() < 1e-12);
    assert!(close(base.viewport.unwrap(), at(2.0 - 1.0 / 30.0)));
    let [x, y, w, h] = at(2.0);
    let peak = key(&meta, "beat-a-pulse0-0-peak");
    let (nw, nh) = (w * 0.8, h * 0.8);
    assert!(close(
        peak.viewport.unwrap(),
        [x + (w - nw) / 2.0, y + (h - nh) / 2.0, nw, nh]
    ));
    assert!(close(
        key(&meta, "beat-a-pulse0-0-rest").viewport.unwrap(),
        at(2.25)
    ));
    // The person's keys are still theirs, untouched.
    assert!(close(
        key(&meta, "v1").viewport.unwrap(),
        [0.5, 0.5, 0.5, 0.5]
    ));
    assert!(key(&meta, "v1").auto.is_none());
}

/// An opacity pulse adds to the opacity; a placement pulse grows the box a
/// rule places, or the zoom of a layer placed by raw zoom.
#[test]
fn opacity_and_placement_pulses() {
    let meta = resolved(&project(
        json!({"pulse": {"every": "downbeat", "property": "opacity", "amount": -0.5}}),
        json!({}),
    ));
    assert_eq!(key(&meta, "beat-a-pulse0-0-peak").opacity, Some(0.5));
    assert_eq!(key(&meta, "beat-a-pulse0-0-rest").opacity, Some(1.0));
    let meta = resolved(&project(
        json!({"pulse": {"every": "downbeat", "property": "placement", "amount": 0.25},
               "keyframes": [{"id": "p", "time": 0, "transitionDuration": 0,
                              "placement": {"height": 40, "anchor": "center"}}]}),
        json!({}),
    ));
    let peak = key(&meta, "beat-a-pulse0-1-peak");
    assert_eq!(peak.placement.as_ref().unwrap().height, Some(50.0));
    let meta = resolved(&project(
        json!({"pulse": {"every": "downbeat", "property": "placement", "amount": 0.25},
               "keyframes": [{"id": "z", "time": 0, "transitionDuration": 0, "zoom": 0.8}]}),
        json!({}),
    ));
    assert!((key(&meta, "beat-a-pulse0-1-peak").zoom.unwrap() - 1.0).abs() < 1e-12);
    // A property a pulse cannot key is refused, by name.
    let (_, problems) = resolved_with(
        &project(json!({"pulse": {"property": "colour"}}), json!({})),
        &BeatGrids::default(),
    );
    assert!(problems
        .iter()
        .any(|p| p.is_break() && p.to_string().contains("pulse property \"colour\"")));
}

/// A shake is four keys a beat — out, back half, out a quarter, home — in
/// a direction seeded by the layer and the beat: the same every time, not
/// the same every beat.
#[test]
fn shake_is_seeded_and_deterministic() {
    let file = project(json!({"shake": {"on": "beat", "amplitude": 10}}), json!({}));
    let one = resolved(&file);
    let two = resolved(&file);
    assert_eq!(one, two);
    let a = layer(&one, "a");
    // Four a beat; the first hit falls on the layer's start, so no
    // "base" key holds the layer before it.
    assert_eq!(a.keyframes.len(), 8 * 4);
    let mut directions = std::collections::BTreeSet::new();
    for beat in 0..8 {
        let hit = key(&one, &format!("beat-a-shake0-{beat}-hit"));
        let (x, y) = (hit.horizontal_shift.unwrap(), hit.vertical_shift.unwrap());
        assert!(((x * x + y * y).sqrt() - 10.0).abs() < 1e-9);
        directions.insert(((x * 100.0).round() as i64, (y * 100.0).round() as i64));
        let back = key(&one, &format!("beat-a-shake0-{beat}-back"));
        assert!((back.horizontal_shift.unwrap() + x / 2.0).abs() < 1e-9);
        let home = key(&one, &format!("beat-a-shake0-{beat}-rest"));
        assert_eq!(home.horizontal_shift, Some(0.0));
        assert!(
            (home.time - (beat as f64 * 0.5 + 0.125)).abs() < 1e-9,
            "a quarter beat"
        );
    }
    assert!(directions.len() > 1, "every beat shook the same way");
}

/// The shake's keys move each kind of layer by its amplitude in canvas
/// pixels, in the terms that kind is placed by: a picture's top-left as its
/// own track resolves it (the resource's real aspect, not a square), a
/// caption's placement rule (its style's, or the one its margins amount
/// to) — never a caption's shifts, which are its margins.
#[test]
fn shake_keys_move_each_kind_by_the_amplitude() {
    let delta =
        |meta: &ProjectMetadata, id: &str, f: &dyn Fn(&ProjectLayerKeyframe) -> (f64, f64)| {
            let hit = f(key(meta, &format!("beat-{id}-shake0-1-hit")));
            let base = f(key(meta, &format!("beat-{id}-shake0-0-rest")));
            let rest = f(key(meta, &format!("beat-{id}-shake0-1-rest")));
            assert_eq!(base, rest, "{id}: home before and after the hit");
            ((hit.0 - base.0).powi(2) + (hit.1 - base.1).powi(2)).sqrt()
        };
    let shifts =
        |k: &ProjectLayerKeyframe| (k.horizontal_shift.unwrap(), k.vertical_shift.unwrap());
    let offset = |k: &ProjectLayerKeyframe| {
        assert!(
            k.horizontal_shift.is_none() && k.vertical_shift.is_none(),
            "a caption's shifts are its margins"
        );
        let rule = k
            .placement
            .as_ref()
            .expect("a caption moves by its placement");
        assert!(!rule.sizes(), "only where it hangs");
        let [x, y] = rule.offset.unwrap();
        (x, y)
    };
    let shake = json!({"on": "downbeat", "amplitude": 12});

    // A placed 16:9 picture: an 80×45 box centred on 160×90 — its top-left
    // is (40, 22.5), not where a square stand-in would put it.
    let placed = resolved(&project(
        json!({"shake": shake, "keyframes": [{"id": "p", "time": 0,
                                               "placement": {"height": 45, "anchor": "center"}}]}),
        json!({}),
    ));
    let home = key(&placed, "beat-a-shake0-1-rest");
    assert!(
        (home.horizontal_shift.unwrap() - 40.0).abs() < 1e-9,
        "{home:?}"
    );
    assert!(
        (home.vertical_shift.unwrap() - 22.5).abs() < 1e-9,
        "{home:?}"
    );
    assert!((delta(&placed, "a", &shifts) - 12.0).abs() < 1e-9);

    // A caption by its margins: anchored top, centred between them.
    let caption = |extra: Value| {
        let mut layer = json!({"kind": "caption", "resourceID": null, "captionText": "Hi",
                               "shake": shake});
        for (k, v) in extra.as_object().unwrap() {
            layer[k] = v.clone();
        }
        resolved(&project(
            layer,
            json!({"subtitleLeftMargin": 30, "subtitleRightMargin": 10,
                   "subtitleVerticalMargin": 50}),
        ))
    };
    let margins = caption(json!({}));
    let home = key(&margins, "beat-a-shake0-1-rest")
        .placement
        .clone()
        .unwrap();
    assert_eq!(home.anchor, Some(promo_model::Anchor::Top));
    assert_eq!(
        home.offset,
        Some([10.0, 50.0]),
        "(30 − 10) / 2 across, the margin down"
    );
    assert!((delta(&margins, "a", &offset) - 12.0).abs() < 1e-9);

    // A placed caption: its style's rule, nudged.
    let styled = caption(json!({"captionStyle": {"placement": {"anchor": "bottom",
                                                                "offset": [0, -20]}}}));
    let home = key(&styled, "beat-a-shake0-1-rest")
        .placement
        .clone()
        .unwrap();
    assert_eq!(home.anchor, Some(promo_model::Anchor::Bottom));
    assert_eq!(home.offset, Some([0.0, -20.0]));
    assert!((delta(&styled, "a", &offset) - 12.0).abs() < 1e-9);
    let blamed: Vec<String> = promo_timeline::validate::warnings(&styled)
        .into_iter()
        .filter(|w| !w.contains("minReaderVersion"))
        .collect();
    assert!(blamed.is_empty(), "nothing to blame: {blamed:?}");
}

/// A pulse on a caption's size punches its fontSize — its zoom is the
/// legacy spelling of the size in points, and 1.04 of it is 1-point text.
#[test]
fn a_caption_pulse_punches_its_font_size() {
    let meta = resolved(&project(
        json!({"kind": "caption", "resourceID": null, "captionText": "Hi",
               "pulse": {"every": "downbeat", "property": "placement", "amount": 0.25}}),
        json!({"subtitleFontSize": 40}),
    ));
    let peak = key(&meta, "beat-a-pulse0-1-peak");
    assert_eq!(peak.font_size, Some(50.0));
    assert_eq!(peak.zoom, None);
    assert_eq!(key(&meta, "beat-a-pulse0-1-rest").font_size, Some(40.0));
}

/// A range limits the grid points: bars, beats, a named section of the
/// song's analysis, or a span from one time to another.
#[test]
fn range_limits_by_bar_section_and_span() {
    let peaks = |meta: &ProjectMetadata| -> Vec<f64> {
        layer(meta, "a")
            .keyframes
            .iter()
            .filter(|k| k.id.ends_with("-peak"))
            .map(|k| k.time)
            .collect()
    };
    let with = |range: Value, grids: &BeatGrids| {
        resolved_with(
            &project(
                json!({"pulse": {"every": "beat", "range": range}}),
                json!({}),
            ),
            grids,
        )
    };
    let none = BeatGrids::default();
    assert_eq!(
        peaks(&with(json!({"bar": 2, "bars": 1}), &none).0),
        [2.0, 2.5, 3.0, 3.5]
    );
    assert_eq!(
        peaks(&with(json!({"beat": 3, "beats": 2}), &none).0),
        [1.0, 1.5]
    );
    assert_eq!(
        peaks(&with(json!({"from": {"beat": 3}, "to": 1.6}), &none).0),
        [1.0, 1.5]
    );
    let mut grids = BeatGrids::default();
    grids.sections.insert(
        "song".into(),
        vec![
            BeatSection {
                index: 0,
                label: "intro".into(),
                start: 0.0,
                end: 2.0,
                start_bar: 1,
                bars: 1,
                energy: 0.0,
            },
            BeatSection {
                index: 1,
                label: "drop".into(),
                start: 2.0,
                end: 4.0,
                start_bar: 2,
                bars: 1,
                energy: 1.0,
            },
        ],
    );
    assert_eq!(
        peaks(&with(json!({"section": "drop"}), &grids).0),
        [2.0, 2.5, 3.0, 3.5]
    );
    assert_eq!(
        peaks(&with(json!({"section": 0}), &grids).0),
        [0.0, 0.5, 1.0, 1.5]
    );
    let (_, problems) = with(json!({"section": "chorus"}), &grids);
    assert!(problems
        .iter()
        .any(|p| p.is_break() && p.to_string().contains("section \"chorus\"")));
}

/// A generated key a person edits is theirs: it reaches the file with no
/// tag, and the next expansion leaves its grid point alone.
#[test]
fn edited_generated_key_becomes_the_persons() {
    let file = project(
        json!({"pulse": {"every": "downbeat", "amount": 0.25}}),
        json!({}),
    );
    let mut meta = resolved(&file);
    let layers = meta.layers.as_mut().unwrap();
    let a = layers.iter_mut().find(|l| l.id == "a").unwrap();
    let peak = a
        .keyframes
        .iter_mut()
        .find(|k| k.id == "beat-a-pulse0-1-peak")
        .unwrap();
    peak.viewport = Some([0.2, 0.2, 0.6, 0.6]);
    let written: Value = serde_json::from_str(&meta.to_json().unwrap()).unwrap();
    let keys = written["layers"][1]["keyframes"].as_array().unwrap();
    assert_eq!(keys.len(), 1, "{keys:?}");
    assert_eq!(keys[0]["id"], "beat-a-pulse0-1-peak");
    assert!(keys[0].get("auto").is_none());
    // Opened again: the person's key stands, the other bar is generated.
    let reopened = resolved(&written);
    let a = layer(&reopened, "a");
    assert!(a
        .keyframes
        .iter()
        .any(|k| k.id == "beat-a-pulse0-1-peak" && !generated(k)));
    assert!(!a.keyframes.iter().any(|k| k.id == "beat-a-pulse0-1-rest"));
    assert!(a
        .keyframes
        .iter()
        .any(|k| k.id == "beat-a-pulse0-0-peak" && generated(k)));
}

fn flash_file() -> Value {
    project(
        json!({}),
        json!({"flash": {"on": "downbeat", "colorHex": "FFFFFF", "opacity": 0.6,
                         "duration": {"beats": 0.25}, "blendMode": "screen"}}),
    )
}

/// A flash is one generated drawing layer above everything over one
/// generated full-canvas drawing, its opacity up on every downbeat.
#[test]
fn flash_is_one_generated_drawing_layer() {
    let meta = resolved(&flash_file());
    let flash = layer(&meta, "beat-flash-0");
    assert!(flash
        .generated_by
        .as_deref()
        .unwrap()
        .starts_with("flash:0:"));
    assert_eq!(flash.kind, promo_model::ProjectLayerKind::Drawing);
    assert!(flash.sort_index > layer(&meta, "a").sort_index);
    assert_eq!(flash.blend_mode, Some(promo_model::BlendMode::Screen));
    let on: Vec<f64> = flash
        .keyframes
        .iter()
        .filter(|k| k.opacity == Some(0.6))
        .map(|k| k.time)
        .collect();
    assert_eq!(on, [0.0, 2.0]);
    let resource = meta
        .resources
        .as_ref()
        .unwrap()
        .iter()
        .find(|r| r.id == "beat-flash-0-drawing")
        .unwrap();
    assert_eq!(flash.resource_id.as_deref(), Some("beat-flash-0-drawing"));
    assert!(resource.generated_by.is_some());
    let drawing = resource.drawing.as_ref().unwrap();
    assert_eq!(drawing.shapes[0].fill_color_hex.as_deref(), Some("FFFFFF"));
}

/// The flash's layer and drawing do not share an id: validate finds no
/// break in a project whose only content is a flash.
#[test]
fn a_flash_validates_clean() {
    let report = promo_timeline::report::file_report_with(
        &flash_file().to_string(),
        None,
        &BeatGrids::default(),
    )
    .unwrap();
    let said = report.messages();
    assert!(
        !said.iter().any(|m| m.contains("used by 2 records")),
        "{said:?}"
    );
}

/// A flash layer someone deleted takes its generator with it, so the
/// delete sticks; one they edited becomes their layer and its generator
/// goes too.
#[test]
fn deleted_flash_layer_removes_its_generator() {
    let mut meta = resolved(&flash_file());
    meta.layers
        .as_mut()
        .unwrap()
        .retain(|l| l.id != "beat-flash-0");
    let written: Value = serde_json::from_str(&meta.to_json().unwrap()).unwrap();
    assert!(
        written["compositionSettings"].get("flash").is_none(),
        "{written}"
    );
    assert!(
        !written.to_string().contains("beat-flash"),
        "the resource went too"
    );

    let mut meta = resolved(&flash_file());
    let flash = meta
        .layers
        .as_mut()
        .unwrap()
        .iter_mut()
        .find(|l| l.id == "beat-flash-0")
        .unwrap();
    flash.name = "My flash".into();
    let written: Value = serde_json::from_str(&meta.to_json().unwrap()).unwrap();
    assert!(written["compositionSettings"].get("flash").is_none());
    let kept = written["layers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"] == "beat-flash-0")
        .expect("the edited layer is the person's");
    assert!(kept.get("generatedBy").is_none());
    assert_eq!(kept["name"], "My flash");
}

/// Nothing a generator made reaches a file: the written project is the
/// one before resolution, generators and all.
#[test]
fn generated_content_is_not_saved() {
    let file = project(
        json!({"pulse": {"every": "beat"}, "shake": {"on": "downbeat"}}),
        json!({"flash": {"on": "bar"}}),
    );
    let unresolved = ProjectMetadata::from_json(&file.to_string()).unwrap();
    let meta = resolved(&file);
    assert!(layer(&meta, "a").keyframes.len() > 8);
    let written = meta.to_json().unwrap();
    assert!(
        !written.contains(musical::BEAT_TAG),
        "a generated key was saved"
    );
    assert!(!written.contains("generatedBy"));
    assert!(!written.contains("beat-flash"));
    let written: Value = serde_json::from_str(&written).unwrap();
    let before: Value = serde_json::from_str(&unresolved.to_json().unwrap()).unwrap();
    assert!(
        promo_model::json_semantically_equal(&written, &before),
        "{written}\n{before}"
    );
    // And resolving again, from the written file, makes the same — and so
    // does resolving the resolved project: nothing doubles.
    let again = resolved(&written);
    assert_eq!(again, meta);
    let mut twice = meta.clone();
    resolve_timing(&mut twice, &BeatGrids::default());
    assert_eq!(twice, meta, "a second pass doubled the generated content");
    assert_eq!(meta.minimum_reader_version(), 49);
}

/// A new grid re-times every generated key: only the song's tempo changed.
#[test]
fn generators_retime_with_the_grid() {
    let mut file = project(json!({"pulse": {"every": "beat"}}), json!({}));
    let at_120: Vec<f64> = layer(&resolved(&file), "a")
        .keyframes
        .iter()
        .filter(|k| k.id.ends_with("-peak"))
        .map(|k| k.time)
        .collect();
    assert_eq!(at_120, [0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5]);
    file["resources"][0]["beatGrid"]["bpm"] = json!(60);
    let at_60: Vec<f64> = layer(&resolved(&file), "a")
        .keyframes
        .iter()
        .filter(|k| k.id.ends_with("-peak"))
        .map(|k| k.time)
        .collect();
    assert_eq!(at_60, [0.0, 1.0, 2.0, 3.0]);
    // In place, too: a project already resolved at 120 BPM, its grid
    // changed, resolves its generated keys anew — the old ones go.
    file["resources"][0]["beatGrid"]["bpm"] = json!(120);
    let mut meta = resolved(&file);
    meta.resources.as_mut().unwrap()[0]
        .beat_grid
        .as_mut()
        .unwrap()
        .bpm = 60.0;
    resolve_timing(&mut meta, &BeatGrids::default());
    let peaks: Vec<f64> = layer(&meta, "a")
        .keyframes
        .iter()
        .filter(|k| k.id.ends_with("-peak"))
        .map(|k| k.time)
        .collect();
    assert_eq!(peaks, [0.0, 1.0, 2.0, 3.0]);
}
