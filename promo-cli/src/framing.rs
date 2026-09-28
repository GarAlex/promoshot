//! Whether every body stays in frame through a camera move (3D plan §6½,
//! R1). A stage whose camera aims at something or flies a route frames the
//! shot itself — its picture is the camera's, the canvas's shape — so a
//! dolly that comes too close cuts the subject off, and until now only a
//! render showed it. The engine's framing probe measures each member's box
//! through the very view the stage draws with; this turns the samples into
//! findings a person or an agent can act on before rendering.

use crate::project::Project;
use crate::render::Renderer;
use promo_engine::FramingSample;

/// A member this little past the frame edge is its anti-aliased rim, not
/// a cut worth a finding.
const TOLERANCE: f32 = 0.02;
/// Moments per second the probe measures, besides every keyframe.
const RATE: f64 = 10.0;

/// Whether any camera in the project frames its own shot — the only
/// stages the probe measures, so a project without one costs nothing.
pub fn frames_its_own_shots(project: &Project) -> bool {
    promo_model::nesting::all_layers(&project.meta)
        .iter()
        .any(|layer| {
            layer.keyframes.iter().any(|k| {
                k.camera
                    .as_ref()
                    .is_some_and(|c| c.target.is_some() || c.motion_path.is_some())
            })
        })
}

/// The moments to measure: an even sample over the film and every
/// keyframe time, so a move's end is always looked at.
fn moments(project: &Project) -> Vec<f64> {
    let duration = project.duration().max(0.0);
    let mut times: Vec<f64> = (0..=((duration * RATE).floor() as usize))
        .map(|i| i as f64 / RATE)
        .filter(|t| *t < duration)
        .collect();
    for layer in promo_model::nesting::all_layers(&project.meta) {
        for k in &layer.keyframes {
            let t = layer.start_time + k.time;
            if t >= 0.0 && t < duration {
                times.push(t);
            }
        }
    }
    times.sort_by(f64::total_cmp);
    times.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    times
}

/// How much of a member's box is outside the frame (0 = all in, 1 = all
/// out), and the edges it crosses.
fn cut(sample: &FramingSample) -> (f32, Vec<&'static str>) {
    if sample.behind {
        return (1.0, vec!["behind the camera"]);
    }
    let (lo, hi) = (sample.lo, sample.hi);
    let (w, h) = ((hi[0] - lo[0]).max(1e-6), (hi[1] - lo[1]).max(1e-6));
    let visible_w = (hi[0].min(1.0) - lo[0].max(0.0)).max(0.0);
    let visible_h = (hi[1].min(1.0) - lo[1].max(0.0)).max(0.0);
    let outside = 1.0 - (visible_w * visible_h) / (w * h);
    let mut edges = Vec::new();
    if lo[0] < -TOLERANCE * w {
        edges.push("left");
    }
    if hi[0] > 1.0 + TOLERANCE * w {
        edges.push("right");
    }
    if lo[1] < -TOLERANCE * h {
        edges.push("top");
    }
    if hi[1] > 1.0 + TOLERANCE * h {
        edges.push("bottom");
    }
    (outside, edges)
}

/// Where a member stands in the frame at one moment.
#[derive(Clone, Copy, PartialEq, Debug)]
enum State {
    /// Inside, or past an edge by no more than its rim.
    In,
    /// Part of it cut while the frame still shows round it on some side:
    /// the subject leaving the frame.
    Partial,
    /// Past every edge: the body covers the frame. A flight INTO a body
    /// (a push to a device's screen) ends here on purpose.
    Covers,
}

fn state(sample: &FramingSample) -> State {
    let (outside, edges) = cut(sample);
    if sample.behind || edges.len() == 4 {
        State::Covers
    } else if outside > TOLERANCE && !edges.is_empty() {
        State::Partial
    } else {
        State::In
    }
}

/// One finding per member per stretch of time it spends partly cut. A
/// stretch that runs into, or out of, the body covering the frame is a
/// flight into it — the camera meant to fill the frame with it — and is
/// no finding.
pub fn summarize(samples: &[FramingSample]) -> Vec<String> {
    let mut keys: Vec<(&str, &str)> = samples
        .iter()
        .map(|s| (s.stage.as_str(), s.member.as_str()))
        .collect();
    keys.sort();
    keys.dedup();
    let mut out = Vec::new();
    for (stage, member) in keys {
        let mut own: Vec<&FramingSample> = samples
            .iter()
            .filter(|s| s.stage == stage && s.member == member)
            .collect();
        own.sort_by(|a, b| a.time.total_cmp(&b.time));
        let states: Vec<State> = own.iter().map(|s| state(s)).collect();
        let mut i = 0;
        while i < own.len() {
            if states[i] != State::Partial {
                i += 1;
                continue;
            }
            let start = i;
            while i < own.len() && states[i] == State::Partial {
                i += 1;
            }
            let run = &own[start..i];
            let into_cover = (start > 0 && states[start - 1] == State::Covers)
                || (i < own.len() && states[i] == State::Covers);
            if into_cover {
                continue;
            }
            let worst = run.iter().map(|s| cut(s).0).fold(0.0f32, f32::max);
            let mut edges: Vec<&str> = run.iter().flat_map(|s| cut(s).1).collect();
            edges.sort();
            edges.dedup();
            let (from, to) = (run[0].time, run[run.len() - 1].time);
            let when = if (to - from).abs() < 1e-6 {
                format!("at {from:.1} s")
            } else {
                format!("from {from:.1} to {to:.1} s")
            };
            out.push(format!(
                "stage \"{stage}\": \"{}\" leaves the frame {when} — up to {:.0}% of it is cut \
                 ({}); its camera comes too close for this field of view: keep a larger \
                 `distance`, a wider `fov`, or aim the camera so it stays in frame",
                run[0].member_name,
                (worst * 100.0).clamp(1.0, 100.0),
                edges.join(", ")
            ));
        }
    }
    out
}

/// The engine's framing samples for `project` at `times`.
fn probe(project: &Project, times: &[f64]) -> Result<Vec<FramingSample>, String> {
    let canvas = &project.meta.composition_settings;
    let (cw, ch) = (canvas.canvas_width.max(1.0), canvas.canvas_height.max(1.0));
    // Small: the probe draws no stage, and the view does not depend on the
    // output size — only on the canvas's shape.
    let h = 180u32;
    let w = ((h as f64 * cw / ch).round() as u32).clamp(16, 4096);
    let mut renderer = Renderer::new(project, w, h)?;
    renderer.framing_samples(times)
}

/// Where each member of a camera-framed stage stands at `time`, for
/// `promo_explain`: member id → `{state, cut, edges, box}`. `state` is
/// `whole`, `cut` (part of it past an edge — the finding `validate`
/// makes), `fillsFrame` (past every edge: a flight into it) or
/// `behindCamera`; `box` is its bounds in fractions of the stage's own
/// frame, 0,0 top left. Empty when no camera frames its own shot.
pub fn in_frame(
    project: &Project,
    time: f64,
) -> Result<std::collections::BTreeMap<String, serde_json::Value>, String> {
    let mut out = std::collections::BTreeMap::new();
    if !frames_its_own_shots(project) {
        return Ok(out);
    }
    for sample in probe(project, &[time])? {
        let (outside, edges) = cut(&sample);
        let state = match state(&sample) {
            _ if sample.behind => "behindCamera",
            State::Covers => "fillsFrame",
            State::Partial => "cut",
            State::In => "whole",
        };
        let round = |v: f32| ((v as f64) * 1000.0).round() / 1000.0;
        let mut doc = serde_json::json!({
            "state": state,
            "cut": round(outside.clamp(0.0, 1.0)),
            "edges": edges,
        });
        if !sample.behind {
            doc["box"] = serde_json::json!({
                "left": round(sample.lo[0]), "top": round(sample.lo[1]),
                "right": round(sample.hi[0]), "bottom": round(sample.hi[1]),
            });
        }
        if state == "cut" {
            doc["why"] = serde_json::json!(
                "the camera is too close for this field of view: a larger `distance`, a \
                 wider `fov`, a wider `framing` word, or aim so it stays in frame"
            );
        }
        out.insert(sample.member, doc);
    }
    Ok(out)
}

/// The framing findings added to a validation report as warnings — the
/// one call `promo validate` and the app's `promo_validate` (through the
/// FFI's report) both make, so the two doors cannot disagree.
pub fn report_into(report: &mut promo_timeline::report::Report, project: &Project) {
    for finding in findings(project) {
        report.warn(finding);
    }
}

/// The findings for `project`: none when no camera frames its own shot,
/// and none when no GPU is available to run the engine (the check is the
/// engine's own view, not a guess at it).
pub fn findings(project: &Project) -> Vec<String> {
    if !frames_its_own_shots(project) {
        return Vec::new();
    }
    let Ok(mut samples) = probe(project, &moments(project)) else {
        return Vec::new();
    };
    // A stage is named as the person named it, not by its id.
    for sample in &mut samples {
        if let Some(layer) = promo_model::nesting::all_layers(&project.meta)
            .into_iter()
            .find(|l| l.id == sample.stage)
        {
            sample.stage = layer.name.clone();
        }
    }
    summarize(&samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(time: f64, lo: [f32; 2], hi: [f32; 2]) -> FramingSample {
        FramingSample {
            time,
            stage: "S".into(),
            member: "L".into(),
            member_name: "Phone".into(),
            lo,
            hi,
            behind: false,
        }
    }

    /// In frame is no finding; a stretch past the edges is one, with its
    /// times, how much is cut and which edges.
    #[test]
    fn a_stretch_out_of_frame_is_one_finding() {
        let samples = vec![
            sample(0.0, [0.3, 0.2], [0.6, 0.8]),
            sample(2.0, [0.2, -0.3], [0.8, 1.3]),
            sample(2.1, [0.1, -0.5], [0.9, 1.5]),
            sample(3.0, [0.3, 0.2], [0.6, 0.8]),
        ];
        let found = summarize(&samples);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("from 2.0 to 2.1 s"), "{}", found[0]);
        assert!(found[0].contains("bottom, top"), "{}", found[0]);
        assert!(found[0].contains("50% of it is cut"), "{}", found[0]);
    }

    /// A push INTO a body — partly cut on the way, then covering the
    /// frame — is the camera filling the frame on purpose: no finding.
    #[test]
    fn a_flight_into_a_body_is_not_leaving_the_frame() {
        let samples = vec![
            sample(0.0, [0.3, 0.2], [0.6, 0.8]),
            sample(1.0, [0.1, -0.2], [0.9, 1.2]),
            sample(2.0, [-0.5, -0.9], [1.5, 1.9]),
            sample(3.0, [-0.9, -1.4], [1.9, 2.4]),
        ];
        assert!(summarize(&samples).is_empty(), "{:?}", summarize(&samples));
    }

    /// The engine's own view, end to end: a stage whose camera, aimed at
    /// its centre, dollies a phone from 6 radii to 1.2 cuts it off, and
    /// validate says so with the stage's name and the seconds; the same
    /// move stopping at 5 radii keeps it in frame and says nothing. (At 3
    /// it IS cut at the top — the render shows it — which is the point:
    /// nobody should have to guess that from the numbers.)
    #[test]
    fn a_dolly_too_close_is_found_and_a_safe_one_is_not() {
        if promo_gpu::GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let project = |end: f64| {
            let dir =
                std::env::temp_dir().join(format!("promo-framing-{}-{}", std::process::id(), end));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("Resources")).unwrap();
            std::fs::write(
                dir.join("Resources/phone.glb"),
                promo_engine::model::device_glb(promo_engine::model::DeviceKind::Phone),
            )
            .unwrap();
            std::fs::write(
                dir.join("metadata.json"),
                format!(
                    r#"{{"id":"P","name":"Dolly","createdAt":0,"state":"recorded","trimStart":0,
                    "trimEnd":4,"videoDuration":4,"subtitles":[],"minReaderVersion":48,
                    "compositionSettings":{{"canvasWidth":960,"canvasHeight":540}},
                    "resources":[{{"id":"ph","kind":"model","filename":"phone.glb",
                                  "displayName":"Phone","addedAt":0}}],
                    "layers":[{{"id":"S","name":"Desk","sortIndex":0,"kind":"stage",
                      "isEnabled":true,"startTime":0,"duration":4,
                      "members":[{{"id":"L","name":"Phone","sortIndex":0,"kind":"model",
                        "isEnabled":true,"startTime":0,"duration":4,"resourceID":"ph",
                        "keyframes":[]}}],
                      "keyframes":[
                        {{"id":"a","time":0,"transitionDuration":0,
                          "camera":{{"yaw":-25,"distance":6,"target":"center"}}}},
                        {{"id":"b","time":3,"transitionDuration":3,
                          "camera":{{"yaw":-25,"distance":{end},"target":"center"}}}}]}}]}}"#
                ),
            )
            .unwrap();
            dir
        };
        let close = project(1.2);
        let found = findings(&Project::open(&close).expect("project"));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].contains("stage \"Desk\": \"Phone\" leaves the frame"),
            "{}",
            found[0]
        );
        assert!(
            found[0].contains("top") && found[0].contains("bottom"),
            "{}",
            found[0]
        );
        // explain says the same of one moment: cut at the end, whole at
        // the start, the edges and the reason on the member itself.
        let explain = |dir: &std::path::Path, time: f64| -> serde_json::Value {
            let args = serde_json::json!({ "project": dir.display().to_string(), "time": time });
            serde_json::from_str(&crate::placement::explain(&args, None).unwrap()).unwrap()
        };
        let end = explain(&close, 3.5);
        let member = &end["layers"][0]["members"][0]["inFrame"];
        assert_eq!(member["state"], "cut", "{end}");
        assert!(member["edges"].to_string().contains("top"), "{member}");
        assert!(member["why"].is_string(), "{member}");
        let start = explain(&close, 0.0);
        assert_eq!(
            start["layers"][0]["members"][0]["inFrame"]["state"], "whole",
            "{start}"
        );
        let safe = project(5.0);
        let quiet = findings(&Project::open(&safe).expect("project"));
        assert!(quiet.is_empty(), "{quiet:?}");
        let _ = std::fs::remove_dir_all(&close);
        let _ = std::fs::remove_dir_all(&safe);
    }

    /// Framing words (R2) through the engine: aimed at the centre, `wide`
    /// shows the phone small and `closeUp` large, both whole — the move
    /// between them validates clean; aimed at one of two phones, a
    /// close-up centres THAT phone and fills the frame with it.
    #[test]
    fn framing_words_size_the_subject_and_keep_it_whole() {
        if promo_gpu::GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let write = |name: &str, members: &str, keyframes: &str| {
            let dir =
                std::env::temp_dir().join(format!("promo-words-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("Resources")).unwrap();
            std::fs::write(
                dir.join("Resources/phone.glb"),
                promo_engine::model::device_glb(promo_engine::model::DeviceKind::Phone),
            )
            .unwrap();
            std::fs::write(
                dir.join("metadata.json"),
                format!(
                    r#"{{"id":"P","name":"Words","createdAt":0,"state":"recorded","trimStart":0,
                    "trimEnd":4,"videoDuration":4,"subtitles":[],"minReaderVersion":48,
                    "compositionSettings":{{"canvasWidth":960,"canvasHeight":540}},
                    "resources":[{{"id":"ph","kind":"model","filename":"phone.glb",
                                  "displayName":"Phone","addedAt":0}}],
                    "layers":[{{"id":"S","name":"Desk","sortIndex":0,"kind":"stage",
                      "isEnabled":true,"startTime":0,"duration":4,"members":[{members}],
                      "keyframes":[{keyframes}]}}]}}"#
                ),
            )
            .unwrap();
            dir
        };
        let phone = |id: &str, across: f64| {
            format!(
                r#"{{"id":"{id}","name":"Phone {id}","sortIndex":0,"kind":"model","isEnabled":true,
                   "startTime":0,"duration":4,"resourceID":"ph",
                   "keyframes":[{{"id":"{id}0","time":0,"transitionDuration":0,
                                 "stageOffset":[{across},0]}}]}}"#
            )
        };
        let centre = write(
            "centre",
            &phone("A", 0.0),
            r#"{"id":"a","time":0,"transitionDuration":0,"camera":{"framing":"wide","target":"center"}},
               {"id":"b","time":3,"transitionDuration":3,"camera":{"framing":"closeUp","target":"center"}}"#,
        );
        let project = Project::open(&centre).expect("project");
        assert!(findings(&project).is_empty(), "{:?}", findings(&project));
        let mut renderer = Renderer::new(&project, 320, 180).expect("renderer");
        let samples = renderer.framing_samples(&[0.0, 3.5]).expect("samples");
        let height = |s: &FramingSample| s.hi[1] - s.lo[1];
        assert_eq!(samples.len(), 2, "{samples:?}");
        let (wide, close) = (&samples[0], &samples[1]);
        assert!(height(wide) < 0.55, "wide {wide:?}");
        assert!(
            height(close) > 0.7 && close.lo[1] >= 0.0 && close.hi[1] <= 1.0,
            "close {close:?}"
        );

        let pair = write(
            "pair",
            &format!("{},{}", phone("A", -1.5), phone("B", 1.5)),
            r#"{"id":"a","time":0,"transitionDuration":0,
                "camera":{"yaw":0,"pitch":0,"framing":"closeUp","target":{"member":"B"}}}"#,
        );
        let project = Project::open(&pair).expect("project");
        let mut renderer = Renderer::new(&project, 320, 180).expect("renderer");
        let samples = renderer.framing_samples(&[1.0]).expect("samples");
        let b = samples
            .iter()
            .find(|s| s.member == "B")
            .expect("B measured");
        let centre_x = (b.lo[0] + b.hi[0]) / 2.0;
        assert!((centre_x - 0.5).abs() < 0.08, "B is centred: {b:?}");
        assert!(
            height(b) > 0.7 && b.lo[1] >= 0.0 && b.hi[1] <= 1.0,
            "B fills, whole: {b:?}"
        );
        let _ = std::fs::remove_dir_all(&centre);
        let _ = std::fs::remove_dir_all(&pair);
    }

    /// R3 end to end: a stage with no camera keyframes, moved only by
    /// `cameraMove` through promo_apply — a push-in, an orbit, a pull-out
    /// and a reveal — validates clean, and the push-in's close-up fills
    /// the frame with the phone whole.
    #[test]
    fn camera_moves_through_apply_keep_the_subject_whole() {
        if promo_gpu::GpuContext::shared().is_none() {
            eprintln!("no GPU adapter; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("promo-moves-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        std::fs::write(
            dir.join("Resources/phone.glb"),
            promo_engine::model::device_glb(promo_engine::model::DeviceKind::Phone),
        )
        .unwrap();
        std::fs::write(
            dir.join("metadata.json"),
            r#"{"id":"P","name":"Moves","createdAt":0,"state":"recorded","trimStart":0,
            "trimEnd":10,"videoDuration":10,"subtitles":[],"minReaderVersion":33,
            "compositionSettings":{"canvasWidth":960,"canvasHeight":540},
            "resources":[{"id":"ph","kind":"model","filename":"phone.glb",
                          "displayName":"Phone","addedAt":0}],
            "layers":[{"id":"S","name":"Desk","sortIndex":0,"kind":"stage",
              "isEnabled":true,"startTime":0,"duration":10,"keyframes":[],
              "members":[{"id":"A","name":"Phone","sortIndex":0,"kind":"model",
                "isEnabled":true,"startTime":0,"duration":10,"resourceID":"ph",
                "keyframes":[]}]}]}"#,
        )
        .unwrap();
        let answer = promo_author::apply(
            &serde_json::json!({ "project": dir.display().to_string(), "commands": [
                {"kind": "cameraMove", "layerID": "S", "move": "pushIn", "at": 0, "duration": 2},
                {"kind": "cameraMove", "layerID": "S", "move": "orbit", "at": 2, "duration": 3},
                {"kind": "cameraMove", "layerID": "S", "move": "pullOut", "at": 5, "duration": 2},
                {"kind": "cameraMove", "layerID": "S", "move": "reveal", "at": 7, "duration": 2}
            ]}),
            None,
        )
        .expect("the moves apply");
        let project = Project::open(&dir).expect("project");
        assert!(frames_its_own_shots(&project), "{answer}");
        assert!(findings(&project).is_empty(), "{:?}", findings(&project));
        let explained: serde_json::Value = serde_json::from_str(
            &crate::placement::explain(
                &serde_json::json!({ "project": dir.display().to_string(), "time": 2.0 }),
                None,
            )
            .unwrap(),
        )
        .unwrap();
        let stage = &explained["layers"][0];
        assert_eq!(stage["camera"]["framing"], "closeUp", "{stage}");
        assert_eq!(stage["members"][0]["inFrame"]["state"], "whole", "{stage}");
        let mut renderer = Renderer::new(&project, 320, 180).expect("renderer");
        let close = &renderer.framing_samples(&[2.0]).expect("samples")[0];
        let height = close.hi[1] - close.lo[1];
        assert!(
            height > 0.7 && close.lo[1] >= 0.0 && close.hi[1] <= 1.0,
            "the push-in ends close and whole: {close:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A box that exactly fills the frame, or brushes an edge by its rim,
    /// is not cut.
    #[test]
    fn filling_the_frame_is_not_leaving_it() {
        let samples = vec![
            sample(0.0, [0.0, 0.0], [1.0, 1.0]),
            sample(0.1, [-0.01, 0.1], [0.5, 0.9]),
        ];
        assert!(summarize(&samples).is_empty());
    }
}
