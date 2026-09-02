//! `promo` — render a project folder to images or video, without an app.
//!
//! A project is a folder: `metadata.json` describing the composition, plus
//! `Resources/` and `Images/` holding the assets. That format is the product's
//! interface (see `../AUTOMATION-PLAN.md`), and this is the first tool to
//! treat it that way.
//!
//! Renders every layer kind the format has: backgrounds, video, images,
//! drawings and captions, with their keyframes — zoom, shift, rotation, tilt,
//! opacity, corner radius, borders, letterboxing. Codec I/O goes through
//! `promo-media`, so what this tool can read and write, any front end can.
//! `promo inspect` reports anything a given project would lose.

use project::{Project, Unsupported};
use promo_cli::{project, render};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
promo — render a PromoShot project folder

USAGE:
    promo validate <project-dir>
    promo schema [--full|--types]
    promo inspect <project-dir>
    promo still   <project-dir> --out <file.png> [--time <s>] [--size <WxH>]
    promo frames  <project-dir> --out <dir> [--fps <n>] [--from <s>] [--to <s>] [--size <WxH>]
    promo video   <project-dir> --out <file.mp4> [--fps <n>] [--size <WxH>]
    promo gif     <project-dir> --out <file.gif> [--fps <n>] [--size <WxH>]

OPTIONS:
    --time <s>     Timestamp for a still (default 0)
    --fps <n>      Frames per second, overriding the project's own
                   (default: the project's `fps`, else 30). Fractional rates
                   are allowed — 59.94 matches a typical screen recording
                   exactly, where 30 resamples it.
    --from/--to    Time range (default: the whole composition)
    --size <WxH>   Output size (default: the project's canvas size)
    --json         Machine output: one JSON object on stdout (errors too)

    `validate` decodes the project, resolves its attachments and reports what
    the renderer would silently correct. Exit 0 when it is clean, 2 when the
    project cannot be decoded at all.

    `schema` prints the authoring subset plus four complete recipes — the
    same text the app's `promo_schema` tool serves. `schema --full` is the
    whole format (`promo_schema_full`); one file in the core backs each, so
    no reader can disagree.

NOTES:
    Reading and writing video needs `ffmpeg` (and `ffprobe`) on PATH. Frames
    are composited on the GPU; ffmpeg only decodes and encodes them.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // With --json anywhere on the line, the failure is machine
            // output too: one object on stdout, the prose on stderr, the
            // exit code unchanged.
            if args.iter().any(|a| a == "--json") {
                println!("{}", serde_json::json!({ "error": e }));
            }
            eprintln!("promo: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let command = args[0].as_str();
    let rest = &args[1..];
    // Describes the FORMAT, not a project, so it takes no directory — and
    // must be answered before the prelude below demands one.
    if command == "schema" {
        if rest.iter().any(|a| a == "--types") {
            let schema = promo_model::wire_schema();
            println!(
                "{}",
                serde_json::to_string_pretty(&schema).unwrap_or_default()
            );
        } else if rest.iter().any(|a| a == "--full") {
            print!("{}", promo_model::SCHEMA);
        } else {
            print!("{}", promo_model::SCHEMA_QUICK);
        }
        return Ok(());
    }
    let dir = rest
        .first()
        .filter(|a| !a.starts_with("--"))
        .ok_or_else(|| format!("{command}: expected a project directory\n\n{USAGE}"))?;
    let opts = Options::parse(&rest[1..])?;
    let project = Project::open(Path::new(dir))?;

    let answer = match command {
        "inspect" => inspect(&project, &opts),
        "validate" => validate(&project, &opts),
        "proxy" => build_proxies(&project, &opts),
        "still" => still(&project, &opts),
        "frames" => frames(&project, &opts),
        "video" => video(&project, &opts),
        "gif" => gif(&project, &opts),
        other => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    }?;
    println!("{answer}");
    Ok(())
}

#[derive(Default)]
struct Options {
    json: bool,
    out: Option<PathBuf>,
    time: Option<f64>,
    fps: Option<f64>,
    from: Option<f64>,
    to: Option<f64>,
    size: Option<(u32, u32)>,
    proxy: render::ProxyPolicy,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut opts = Options::default();
        let mut i = 0;
        while i < args.len() {
            let flag = args[i].as_str();
            let value = || {
                args.get(i + 1)
                    .cloned()
                    .ok_or_else(|| format!("{flag}: expected a value"))
            };
            match flag {
                "--json" => {
                    opts.json = true;
                    i += 1;
                    continue;
                }
                "--out" => opts.out = Some(PathBuf::from(value()?)),
                "--time" => opts.time = Some(parse_f64(&value()?, flag)?),
                "--fps" => opts.fps = Some(parse_f64(&value()?, flag)?),
                "--proxy" => opts.proxy = render::ProxyPolicy::parse(&value()?)?,
                "--from" => opts.from = Some(parse_f64(&value()?, flag)?),
                "--to" => opts.to = Some(parse_f64(&value()?, flag)?),
                "--size" => {
                    let raw = value()?;
                    let (w, h) = raw
                        .split_once(['x', 'X'])
                        .ok_or_else(|| format!("--size: expected WxH, got `{raw}`"))?;
                    opts.size = Some((
                        w.trim().parse().map_err(|_| "--size: bad width")?,
                        h.trim().parse().map_err(|_| "--size: bad height")?,
                    ));
                }
                other => return Err(format!("unknown option `{other}`\n\n{USAGE}")),
            }
            i += 2;
        }
        Ok(opts)
    }

    fn out(&self) -> Result<&Path, String> {
        self.out
            .as_deref()
            .ok_or_else(|| "--out is required".to_string())
    }

    fn size(&self, project: &Project) -> (u32, u32) {
        self.size.unwrap_or_else(|| {
            let s = &project.meta.composition_settings;
            (
                (s.canvas_width.max(1.0)) as u32,
                (s.canvas_height.max(1.0)) as u32,
            )
        })
    }
}

fn parse_f64(raw: &str, flag: &str) -> Result<f64, String> {
    raw.parse()
        .map_err(|_| format!("{flag}: expected a number, got `{raw}`"))
}

/// What is in this project, and what this tool would drop.
/// Everything wrong with a project that still renders.
///
/// A decode failure already came back from `Project::open`; what is left is
/// the quiet stuff — a window the renderer slides back inside the source, a
/// viewport on a layer kind that ignores it, an attachment that could not
/// resolve, a file that uses features it does not declare a reader version
/// for. Each of those changes what you see and none of them said so.
/// `promo proxy <project>`: a tier-1 proxy for every video resource the
/// project has (B3.1) — idempotent, built into the proxy cache, never
/// into the package.
fn build_proxies(project: &Project, opts: &Options) -> Result<String, String> {
    use promo_media::proxy;
    let cache = proxy::cache_dir();
    let mut built = Vec::new();
    for resource in project
        .resources()
        .iter()
        .filter(|r| r.kind == promo_model::ProjectResourceKind::Video)
    {
        let Some(source) = project.resource_path(resource) else {
            continue;
        };
        let fresh = proxy::available(&cache, &source, 1).is_none();
        let path =
            proxy::ensure(&cache, &source, proxy::TIER1_LONG_EDGE).map_err(|e| e.to_string())?;
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        built.push((resource.display_name.clone(), path, bytes, fresh));
    }
    if opts.json {
        return Ok(serde_json::json!({
            "cacheDir": cache.display().to_string(),
            "proxies": built.iter().map(|(name, path, bytes, fresh)| serde_json::json!({
                "resource": name, "path": path.display().to_string(), "bytes": bytes, "built": fresh,
            })).collect::<Vec<_>>(),
        })
        .to_string());
    }
    if built.is_empty() {
        return Ok("no video resources — nothing to proxy".into());
    }
    let mut out = format!("proxies in {}:", cache.display());
    for (name, path, bytes, fresh) in &built {
        out.push_str(&format!(
            "\n  {}  {}  {:.1} MB{}",
            name,
            path.file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default(),
            *bytes as f64 / 1_048_576.0,
            if *fresh { "  (built)" } else { "" }
        ));
    }
    Ok(out)
}

fn validate(project: &Project, opts: &Options) -> Result<String, String> {
    let mut warnings = project.attachment_problems.clone();
    warnings.extend(promo_timeline::validate::warnings(&project.meta));

    if opts.json {
        return Ok(serde_json::json!({ "ok": true, "warnings": warnings }).to_string());
    }
    if warnings.is_empty() {
        return Ok("ok — nothing the renderer would quietly correct".into());
    }
    let mut out = format!(
        "ok — the project decodes, with {} warning(s):",
        warnings.len()
    );
    for warning in &warnings {
        out.push_str(&format!("\n  - {warning}"));
    }
    Ok(out)
}

fn inspect(project: &Project, opts: &Options) -> Result<String, String> {
    let layers = project.meta.layers.as_deref().unwrap_or(&[]);
    let (w, h) = (
        project.meta.composition_settings.canvas_width,
        project.meta.composition_settings.canvas_height,
    );
    let mut renderable = 0;
    let mut skipped: Vec<(&str, Unsupported)> = Vec::new();
    for layer in layers {
        match project.unsupported(layer) {
            None => renderable += 1,
            Some(why) => skipped.push((layer.name.as_str(), why)),
        }
    }
    // Nested compositions: what each holds, and who places it.
    let compositions: Vec<serde_json::Value> = project
        .resources()
        .iter()
        .filter(|r| r.kind == promo_model::ProjectResourceKind::Composition)
        .map(|r| {
            let nested = r.composition.as_ref().map(|c| c.layers.len()).unwrap_or(0);
            let placed_by: Vec<String> = promo_model::nesting::all_layers(&project.meta)
                .into_iter()
                .filter(|l| l.resource_id.as_deref() == Some(r.id.as_str()))
                .map(|l| l.id.clone())
                .collect();
            serde_json::json!({
                "id": r.id, "name": r.display_name, "duration": r.duration,
                "layers": nested, "placedBy": placed_by,
            })
        })
        .collect();
    if opts.json {
        return Ok(serde_json::json!({
            "name": project.meta.name,
            "canvas": { "width": w, "height": h },
            "duration": project.duration(),
            "updated": project.meta.updated_at,
            "compositions": compositions,
            "layers": layers
                .iter()
                .map(|l| serde_json::json!({
                    "id": l.id, "name": l.name, "kind": l.kind,
                    "startTime": l.start_time, "duration": l.duration,
                }))
                .collect::<Vec<_>>(),
            "resources": project.resources().len(),
            "renderable": renderable,
            "markers": project.meta.markers.as_deref().unwrap_or(&[]).iter().map(|m| serde_json::json!({
                "id": m.id, "time": m.time, "name": m.name, "kind": m.kind,
            })).collect::<Vec<_>>(),
            "skipped": skipped
                .iter()
                .map(|(name, why)| serde_json::json!({
                    "layer": name, "reason": why.to_string()
                }))
                .collect::<Vec<_>>(),
        })
        .to_string());
    }
    let mut out = String::new();
    out.push_str(&format!("project:   {}\n", project.meta.name));
    out.push_str(&format!("canvas:    {w:.0}x{h:.0}\n"));
    out.push_str(&format!("duration:  {:.2}s\n", project.duration()));
    // The turn signal (SPECS D5): a raw change marker, not a calendar date —
    // compare it with the last inspect to learn whether someone else edited.
    if let Some(stamp) = project.meta.updated_at {
        out.push_str(&format!("updated:   {stamp}\n"));
    }
    out.push_str(&format!("layers:    {}\n", layers.len()));
    // Each layer with its ID — the handle promo_upsert_keyframe takes.
    // The trial that validated the keyframe tool (issue #1) had to read
    // metadata.json to learn a layer's id; this listing is the fix.
    for layer in layers {
        let end = layer
            .duration
            .map(|d| format!("{:.2}", layer.start_time + d))
            .unwrap_or_else(|| "…".into());
        let kind = serde_json::to_value(layer.kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| format!("{:?}", layer.kind));
        out.push_str(&format!(
            "  {}  {kind}  {:.2}–{end}s  \"{}\"\n",
            layer.id, layer.start_time, layer.name
        ));
    }
    out.push_str(&format!("resources: {}\n", project.resources().len()));
    if !compositions.is_empty() {
        out.push_str(&format!("compositions: {}\n", compositions.len()));
        for c in &compositions {
            out.push_str(&format!(
                "  {}  \"{}\"  {:.2}s  {} layers  placed by {}\n",
                c["id"].as_str().unwrap_or(""),
                c["name"].as_str().unwrap_or(""),
                c["duration"].as_f64().unwrap_or(0.0),
                c["layers"],
                c["placedBy"].as_array().map(|p| p.len()).unwrap_or(0)
            ));
        }
    }
    if let Some(markers) = project.meta.markers.as_deref().filter(|m| !m.is_empty()) {
        out.push_str(&format!("markers:   {}\n", markers.len()));
        for m in markers {
            let kind = serde_json::to_value(m.kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            out.push_str(&format!("  {:.2}s  {kind}  \"{}\"\n", m.time, m.name));
        }
    }
    out.push_str(&format!("\nrenderable: {renderable} of {}", layers.len()));
    if !skipped.is_empty() {
        out.push_str("\nskipped:");
        for (name, why) in &skipped {
            out.push_str(&format!("\n  - {name}: {why}"));
        }
    }
    if renderable == 0 && !layers.is_empty() {
        out.push_str(
            "\n\nNothing in this project renders yet. Video decoding on this platform\n\
             and text rasterization are the two gaps.",
        );
    }
    Ok(out)
}

fn still(project: &Project, opts: &Options) -> Result<String, String> {
    let out = opts.out()?;
    let (w, h) = opts.size(project);
    let time = opts.time.unwrap_or(0.0);
    let mut renderer = render::Renderer::with_proxy(project, w, h, opts.proxy)?;
    let rgba = renderer.frame_rgba(time)?;
    render::write_png(out, &rgba, w, h)?;
    if opts.json {
        return Ok(serde_json::json!({
            "wrote": out.display().to_string(),
            "width": w, "height": h, "time": time,
        })
        .to_string());
    }
    Ok(format!("wrote {} ({w}x{h} at {time:.2}s)", out.display()))
}

fn frames(project: &Project, opts: &Options) -> Result<String, String> {
    let out = opts.out()?;
    let (w, h) = opts.size(project);
    let (start, end, fps) = range(project, opts);
    let count = frame_count(start, end, fps);
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;

    let mut renderer = render::Renderer::with_proxy(project, w, h, opts.proxy)?;
    for i in 0..count {
        let time = start + i as f64 / fps;
        let rgba = renderer.frame_rgba(time)?;
        let path = out.join(format!("frame-{i:05}.png"));
        render::write_png(&path, &rgba, w, h)?;
        if i % 30 == 0 || i + 1 == count {
            eprint!("\r  {}/{count} frames", i + 1);
        }
    }
    eprintln!();
    if opts.json {
        return Ok(serde_json::json!({
            "wroteDir": out.display().to_string(),
            "frames": count, "width": w, "height": h,
            "fps": fps, "from": start, "to": end,
        })
        .to_string());
    }
    Ok(format!(
        "wrote {count} frames to {} ({w}x{h} @ {fps}fps)",
        out.display()
    ))
}

/// Renders straight into ffmpeg's stdin as raw BGRA — no intermediate PNGs,
/// no temp directory the size of the uncompressed video.
///
/// ffmpeg is invoked as a separate program, not linked, so this borrows
/// nothing from its licence.
fn video(project: &Project, opts: &Options) -> Result<String, String> {
    let out = opts.out()?;
    let (w, h) = opts.size(project);
    let (start, end, fps) = range(project, opts);
    let count = frame_count(start, end, fps);

    // The export itself lives in the lib (render::export_video), because
    // the app exports through the same code over the FFI — the CLI only
    // narrates it.
    let settings = render::ExportSettings {
        width: w,
        height: h,
        start,
        end,
        fps,
        // The CLI is the oracle and renders clean; watermark policy is the
        // apps' concern.
        overlay: None,
    };
    render::export_video(
        project,
        out,
        &settings,
        &mut |done, total| {
            if done % 30 == 0 || done == total {
                eprint!("\r  {done}/{total} frames");
            }
            true
        },
        opts.proxy,
    )?;
    eprintln!();

    if opts.json {
        return Ok(serde_json::json!({
            "wrote": out.display().to_string(),
            "frames": count, "width": w, "height": h, "fps": fps,
        })
        .to_string());
    }
    Ok(format!(
        "wrote {} ({w}x{h}, {count} frames @ {fps}fps)",
        out.display()
    ))
}

/// A looping GIF, the same frames `video` renders — encoded with the
/// image crate rather than ffmpeg, because a GIF needs no codec licence
/// and no external tool. Default 12fps: a GIF is a preview, not a master.
fn gif(project: &Project, opts: &Options) -> Result<String, String> {
    let out = opts.out()?;
    let (w, h) = opts.size(project);
    let start = opts.from.unwrap_or(0.0).max(0.0);
    let end = opts.to.unwrap_or_else(|| project.duration()).max(start);
    let fps = opts.fps.unwrap_or(12.0).max(1.0);
    let count = frame_count(start, end, fps);

    let file = std::fs::File::create(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut encoder = image::codecs::gif::GifEncoder::new(std::io::BufWriter::new(file));
    encoder
        .set_repeat(image::codecs::gif::Repeat::Infinite)
        .map_err(|e| e.to_string())?;
    let delay = image::Delay::from_numer_denom_ms((1000.0 / fps).round() as u32, 1);
    let mut renderer = render::Renderer::with_proxy(project, w, h, opts.proxy)?;
    for i in 0..count {
        let time = start + i as f64 / fps;
        let rgba = renderer.frame_rgba(time)?;
        let buffer = image::RgbaImage::from_raw(w, h, rgba).ok_or("frame buffer size mismatch")?;
        let frame = image::Frame::from_parts(buffer, 0, 0, delay);
        encoder.encode_frame(frame).map_err(|e| e.to_string())?;
        if i % 12 == 0 || i + 1 == count {
            eprint!("\r  {}/{count} frames", i + 1);
        }
    }
    eprintln!();
    if opts.json {
        return Ok(serde_json::json!({
            "wrote": out.display().to_string(),
            "frames": count, "width": w, "height": h, "fps": fps,
        })
        .to_string());
    }
    Ok(format!(
        "wrote {} ({w}x{h}, {count} frames @ {fps}fps, looping)",
        out.display()
    ))
}

/// The export clock — `promo_timeline::export_plan`, the one rule the FFI
/// export job and the apps' loop also run. The project decides its own
/// frame rate; --fps is an override for a one-off render.
fn range(project: &Project, opts: &Options) -> (f64, f64, f64) {
    let plan = promo_timeline::export_plan(&project.meta, opts.fps, opts.from, opts.to);
    (plan.start, plan.end, plan.fps)
}

fn frame_count(start: f64, end: f64, fps: f64) -> usize {
    promo_timeline::frame_count(start, end, fps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_parses_and_defaults_to_the_canvas() {
        let opts = Options::parse(&["--size".into(), "1280x800".into()]).unwrap();
        assert_eq!(opts.size, Some((1280, 800)));
        assert!(Options::parse(&["--size".into(), "1280".into()]).is_err());
    }

    #[test]
    fn frame_count_never_renders_nothing() {
        assert_eq!(
            frame_count(0.0, 0.0, 30.0),
            1,
            "a still project is one frame"
        );
        assert_eq!(frame_count(0.0, 1.0, 30.0), 30);
        assert_eq!(frame_count(2.0, 4.0, 24.0), 48);
    }

    #[test]
    fn unknown_options_are_rejected_rather_than_ignored() {
        assert!(Options::parse(&["--nope".into(), "1".into()]).is_err());
        assert!(Options::parse(&["--time".into()]).is_err());
    }

    /// --json is a lone flag among value-taking options, and it must not
    /// eat its neighbour.
    #[test]
    fn json_is_a_boolean_flag() {
        let opts = Options::parse(&["--json".into(), "--time".into(), "2".into()]).unwrap();
        assert!(opts.json);
        assert_eq!(opts.time, Some(2.0));
    }

    /// A placed composition is renderable — it needs no file — and inspect
    /// says what it holds and who places it.
    #[test]
    fn inspect_lists_a_composition_and_counts_its_placement_renderable() {
        let dir = std::env::temp_dir().join(format!("promo-nest-inspect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("metadata.json"),
            r#"{"id":"P","name":"Nest","createdAt":0,"state":"recorded","minReaderVersion":19,
            "trimStart":0,"trimEnd":4,"videoDuration":4,"subtitles":[],
            "compositionSettings":{"canvasWidth":1280,"canvasHeight":720},
            "resources":[{"id":"cap","kind":"caption","filename":"","displayName":"Words","addedAt":0,
               "captionText":"hi","imageCuts":[]},
              {"id":"A","kind":"composition","filename":"","displayName":"Title","addedAt":0,
               "duration":4,"pixelWidth":1280,"pixelHeight":720,"imageCuts":[],
               "composition":{"canvasWidth":1280,"canvasHeight":720,"layers":[
                 {"id":"L","name":"inner","sortIndex":0,"kind":"caption","isEnabled":true,
                  "startTime":0,"duration":4,"resourceID":"cap","keyframes":[]}]}}],
            "layers":[{"id":"P1","name":"Title","sortIndex":0,"kind":"video","isEnabled":true,
              "startTime":0,"duration":4,"resourceID":"A","keyframes":[]}]}"#,
        )
        .unwrap();
        let project = Project::open(&dir).expect("opens");
        assert!(project
            .unsupported(&project.meta.layers.as_deref().unwrap()[0])
            .is_none());
        let out = inspect(&project, &Options::parse(&["--json".into()]).unwrap()).unwrap();
        let json: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(json["renderable"], 1);
        assert_eq!(json["compositions"][0]["layers"], 1);
        assert_eq!(json["compositions"][0]["placedBy"][0], "P1");
        let text = inspect(&project, &Options::parse(&[]).unwrap()).unwrap();
        assert!(
            text.contains("compositions: 1") && text.contains("placed by 1"),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Nested compositions: a composition that contains itself cannot be
    /// rendered by recursion, so the file refuses to OPEN — as a decode
    /// failure would; a nested layer naming an unknown resource is a
    /// warning `validate` lists, like any other quiet correction.
    #[test]
    fn a_composition_containing_itself_refuses_to_open_and_an_unknown_nested_reference_warns() {
        let dir = std::env::temp_dir().join(format!("promo-nest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let doc = |inner_resource: &str| {
            format!(
                r#"{{"id":"P","name":"Nest","createdAt":0,"state":"recorded",
                "minReaderVersion":19,"trimStart":0,"trimEnd":4,"videoDuration":4,
                "subtitles":[],
                "compositionSettings":{{"canvasWidth":1280,"canvasHeight":720}},
                "resources":[{{"id":"A","kind":"composition","filename":"","displayName":"Title",
                  "addedAt":0,"duration":4,"pixelWidth":1280,"pixelHeight":720,"imageCuts":[],
                  "composition":{{"canvasWidth":1280,"canvasHeight":720,"layers":[
                    {{"id":"L","name":"inner","sortIndex":0,"kind":"video","isEnabled":true,
                     "startTime":0,"duration":4,"resourceID":"{inner_resource}","keyframes":[]}}]}}}}],
                "layers":[{{"id":"P1","name":"Title","sortIndex":0,"kind":"video","isEnabled":true,
                  "startTime":0,"duration":4,"resourceID":"A","keyframes":[]}}]}}"#
            )
        };
        std::fs::write(dir.join("metadata.json"), doc("A")).unwrap();
        let err = Project::open(&dir)
            .err()
            .expect("a self-containing composition is refused");
        assert!(err.contains("contains itself"), "{err}");

        std::fs::write(dir.join("metadata.json"), doc("ghost")).unwrap();
        let project = Project::open(&dir).expect("opens with a warning");
        let out = validate(&project, &Options::parse(&["--json".into()]).unwrap()).unwrap();
        let json: serde_json::Value = serde_json::from_str(&out).unwrap();
        let warnings = json["warnings"].as_array().unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.as_str().unwrap().contains("unknown resource ghost")),
            "{warnings:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// Machine output is one parseable object per command — validate's
    /// warnings as an array, inspect's facts as fields — because "output
    /// defaults to prose" is how a tool stays unscriptable.
    #[test]
    fn validate_and_inspect_speak_json_when_asked() {
        let dir = std::env::temp_dir().join(format!("promo-json-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("metadata.json"),
            r#"{"id":"P","name":"Machine","createdAt":0,"state":"recorded",
                "minReaderVersion":18,"trimStart":0,"trimEnd":3,"videoDuration":3,
                "subtitles":[],
                "compositionSettings":{"canvasWidth":1280,"canvasHeight":720},
                "layers":[{"id":"bg","name":"Ground","sortIndex":0,
                  "kind":"background","isEnabled":true,"startTime":0,
                  "duration":3,"keyframes":[
                    {"id":"k","time":0,"colorHex":"101014","transitionDuration":0}]}]}"#,
        )
        .unwrap();
        let project = Project::open(&dir).expect("opens");
        let json_opts = Options {
            json: true,
            ..Options::default()
        };

        let answer = validate(&project, &json_opts).unwrap();
        let value: serde_json::Value = serde_json::from_str(&answer).expect("valid JSON");
        assert_eq!(value["ok"], true);
        assert!(value["warnings"].is_array());

        let answer = inspect(&project, &json_opts).unwrap();
        let value: serde_json::Value = serde_json::from_str(&answer).expect("valid JSON");
        assert_eq!(value["name"], "Machine");
        assert_eq!(value["canvas"]["width"], 1280.0);
        // Layers answer as an ARRAY of handles (issue #1's validation:
        // ids are what promo_upsert_keyframe takes), not a bare count.
        let layers = value["layers"].as_array().expect("layers array");
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0]["id"], "bg");
        assert_eq!(layers[0]["kind"], "background");

        // And the prose stays prose without the flag.
        let prose = validate(&project, &Options::default()).unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&prose).is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
