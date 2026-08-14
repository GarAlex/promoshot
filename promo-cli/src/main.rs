//! `promo` — render a project folder to images or video, without an app.
//!
//! A project is a folder: `metadata.json` describing the composition, plus
//! `Resources/` and `Images/` holding the assets. That format is the product's
//! interface (see `../AUTOMATION-PLAN.md`), and this is the first tool to
//! treat it that way.
//!
//! What renders today: backgrounds, image layers, drawings and captions, with
//! their keyframes — zoom, shift, rotation, tilt, opacity, corner radius,
//! borders, letterboxing. What does not: video layers (no decoder yet, R2 in
//! LINUX-READY-PLAN). `promo inspect` says so per project rather than leaving
//! you to infer it from a blank frame.

mod project;
mod render;

use project::{Project, Unsupported};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
promo — render a PromoShot project folder

USAGE:
    promo inspect <project-dir>
    promo still   <project-dir> --out <file.png> [--time <s>] [--size <WxH>]
    promo frames  <project-dir> --out <dir> [--fps <n>] [--from <s>] [--to <s>] [--size <WxH>]
    promo video   <project-dir> --out <file.mp4> [--fps <n>] [--size <WxH>]

OPTIONS:
    --time <s>     Timestamp for a still (default 0)
    --fps <n>      Frames per second (default 30)
    --from/--to    Time range (default: the whole composition)
    --size <WxH>   Output size (default: the project's canvas size)

NOTES:
    `video` pipes raw frames to `ffmpeg`, which must be on PATH. Frames are
    rendered on the GPU; ffmpeg only encodes them.

    Video layers are not rendered yet — `inspect` reports what a given project
    would lose.
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
            eprintln!("promo: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let command = args[0].as_str();
    let rest = &args[1..];
    let dir = rest
        .first()
        .filter(|a| !a.starts_with("--"))
        .ok_or_else(|| format!("{command}: expected a project directory\n\n{USAGE}"))?;
    let opts = Options::parse(&rest[1..])?;
    let project = Project::open(Path::new(dir))?;

    match command {
        "inspect" => inspect(&project),
        "still" => still(&project, &opts),
        "frames" => frames(&project, &opts),
        "video" => video(&project, &opts),
        other => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    }
}

#[derive(Default)]
struct Options {
    out: Option<PathBuf>,
    time: Option<f64>,
    fps: Option<f64>,
    from: Option<f64>,
    to: Option<f64>,
    size: Option<(u32, u32)>,
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
                "--out" => opts.out = Some(PathBuf::from(value()?)),
                "--time" => opts.time = Some(parse_f64(&value()?, flag)?),
                "--fps" => opts.fps = Some(parse_f64(&value()?, flag)?),
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

    fn fps(&self) -> f64 {
        self.fps.unwrap_or(30.0).max(1.0)
    }
}

fn parse_f64(raw: &str, flag: &str) -> Result<f64, String> {
    raw.parse()
        .map_err(|_| format!("{flag}: expected a number, got `{raw}`"))
}

/// What is in this project, and what this tool would drop.
fn inspect(project: &Project) -> Result<(), String> {
    let layers = project.meta.layers.as_deref().unwrap_or(&[]);
    let (w, h) = (
        project.meta.composition_settings.canvas_width,
        project.meta.composition_settings.canvas_height,
    );
    println!("project:   {}", project.meta.name);
    println!("canvas:    {w:.0}x{h:.0}");
    println!("duration:  {:.2}s", project.duration());
    println!("layers:    {}", layers.len());
    println!("resources: {}", project.resources().len());

    let mut renderable = 0;
    let mut skipped: Vec<(&str, Unsupported)> = Vec::new();
    for layer in layers {
        match project.unsupported(layer) {
            None => renderable += 1,
            Some(why) => skipped.push((layer.name.as_str(), why)),
        }
    }
    println!("\nrenderable: {renderable} of {}", layers.len());
    if !skipped.is_empty() {
        println!("skipped:");
        for (name, why) in &skipped {
            println!("  - {name}: {why}");
        }
    }
    if renderable == 0 && !layers.is_empty() {
        println!(
            "\nNothing in this project renders yet. Video decoding (LINUX-READY-PLAN R2)\n\
             and text rasterization are the two gaps."
        );
    }
    Ok(())
}

fn still(project: &Project, opts: &Options) -> Result<(), String> {
    let out = opts.out()?;
    let (w, h) = opts.size(project);
    let time = opts.time.unwrap_or(0.0);
    let mut renderer = render::Renderer::new(project, w, h)?;
    let rgba = renderer.frame_rgba(time)?;
    render::write_png(out, &rgba, w, h)?;
    println!("wrote {} ({w}x{h} at {time:.2}s)", out.display());
    Ok(())
}

fn frames(project: &Project, opts: &Options) -> Result<(), String> {
    let out = opts.out()?;
    let (w, h) = opts.size(project);
    let (start, end, fps) = range(project, opts);
    let count = frame_count(start, end, fps);
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;

    let mut renderer = render::Renderer::new(project, w, h)?;
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
    println!(
        "wrote {count} frames to {} ({w}x{h} @ {fps}fps)",
        out.display()
    );
    Ok(())
}

/// Renders straight into ffmpeg's stdin as raw BGRA — no intermediate PNGs,
/// no temp directory the size of the uncompressed video.
///
/// ffmpeg is invoked as a separate program, not linked, so this borrows
/// nothing from its licence.
fn video(project: &Project, opts: &Options) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let out = opts.out()?;
    let (w, h) = opts.size(project);
    let (start, end, fps) = range(project, opts);
    let count = frame_count(start, end, fps);
    if count == 0 {
        return Err("nothing to render: the time range is empty".into());
    }

    let mut renderer = render::Renderer::new(project, w, h)?;
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "bgra",
            "-s",
            &format!("{w}x{h}"),
            "-r",
            &fps.to_string(),
            "-i",
            "-",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-crf",
            "18",
        ])
        .arg(out)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                "ffmpeg not found on PATH. Install it, or use `promo frames` and encode \
                 the PNG sequence yourself."
                    .to_string()
            }
            _ => format!("ffmpeg: {e}"),
        })?;

    let mut stdin = child.stdin.take().ok_or("ffmpeg: no stdin")?;
    for i in 0..count {
        let time = start + i as f64 / fps;
        let bgra = renderer.frame_bgra(time)?;
        stdin
            .write_all(&bgra)
            .map_err(|e| format!("ffmpeg stdin (frame {i}): {e}"))?;
        if i % 30 == 0 || i + 1 == count {
            eprint!("\r  {}/{count} frames", i + 1);
        }
    }
    eprintln!();
    drop(stdin);

    let status = child.wait().map_err(|e| format!("ffmpeg: {e}"))?;
    if !status.success() {
        return Err(format!("ffmpeg exited with {status}"));
    }
    println!(
        "wrote {} ({w}x{h}, {count} frames @ {fps}fps)",
        out.display()
    );
    Ok(())
}

fn range(project: &Project, opts: &Options) -> (f64, f64, f64) {
    let start = opts.from.unwrap_or(0.0).max(0.0);
    let end = opts.to.unwrap_or_else(|| project.duration()).max(start);
    (start, end, opts.fps())
}

fn frame_count(start: f64, end: f64, fps: f64) -> usize {
    // A zero-length composition still yields one frame — asking for a poster
    // of a single-image project should not produce nothing.
    (((end - start) * fps).round() as usize).max(1)
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
}
