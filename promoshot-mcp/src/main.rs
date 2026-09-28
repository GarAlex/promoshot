//! An MCP server for making PromoShot projects with no app attached.
//!
//! Speaks Model Context Protocol over stdio — newline-delimited JSON-RPC on
//! stdin/stdout, logs on stderr — which is the transport agent clients spawn
//! themselves: no port, no token, no daemon. The Mac app's automation
//! server shares the core tool names over HTTP for a running GUI; this
//! binary is the headless half, and the fuller one.
//!
//! The tool surface is the whole agent loop, and each piece keeps to one
//! source of truth. The three schema faces (`promo_schema`, `_full`,
//! `_types`) are compiled in from `promo-model`, the same files and structs
//! the parser runs. The senses (`promo_media_probe` / `_filmstrip` /
//! `_silences`) shell to the ffmpeg/ffprobe the render pipeline already
//! requires. The scaffold (`promo_init` / `promo_upsert_layer`, in
//! `authoring`) writes metadata.json through the format's own parser.
//! Narration (`promo_speak`, in `speak`) spends the person's own provider
//! key from the environment, under the app's exact receipt discipline. And
//! every RENDER goes through the `promo` CLI beside this executable — this
//! server owns no rendering code, so it can never disagree with the one
//! command-line contract.
//!
//! Configuration is three flags, everything else defaulted:
//!   --workspace <dir>   where promo_workspace points (else
//!                       $PROMOSHOT_WORKSPACE, else XDG data dir)
//!   --root <dir>        fence: refuse projects outside this tree
//!   --promo <path>      the CLI binary (else next to this executable,
//!                       else PATH)
//!   --log <file>        append one line per tool call: time, tool,
//!                       milliseconds, ok/error — what a session cost
//!                       on this side of the wire
// The tool descriptors are one large `json!` literal; the macro recurses
// per nesting level, and the default limit is below what 19 tools need.
#![recursion_limit = "256"]

mod media;
mod preview;
mod speak;

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use promo_author::contract::{self, Host};
use serde_json::{json, Value};

const PROTOCOL_FALLBACK: &str = "2025-03-26";

fn main() {
    // `promoshot-mcp key …` is the person's door to the keyring, not a
    // server session: handled and done before any stdio framing.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("key") {
        match speak::key_command(&argv[1..], &mut std::io::stdin()) {
            Ok(answer) => {
                println!("{answer}");
                return;
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        }
    }
    let config = match Config::from_args(argv.into_iter()) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("promoshot-mcp: {message}");
            std::process::exit(2);
        }
    };
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            // A parse failure has no id to answer; say so on stderr and
            // keep serving rather than dying mid-session.
            eprintln!("promoshot-mcp: unparseable request skipped");
            continue;
        };
        if let Some(response) = handle(&request, &config, &run_promo) {
            let mut bytes = response.to_string();
            bytes.push('\n');
            if stdout.write_all(bytes.as_bytes()).is_err() {
                break;
            }
            let _ = stdout.flush();
        }
    }
}

struct Config {
    workspace: PathBuf,
    root: Option<PathBuf>,
    promo: Option<PathBuf>,
    log: Option<PathBuf>,
}

impl Config {
    fn from_args(args: impl Iterator<Item = String>) -> Result<Config, String> {
        let mut workspace = None;
        let mut root = None;
        let mut promo = None;
        let mut log = None;
        let mut args = args.peekable();
        while let Some(flag) = args.next() {
            let mut value = |name: &str| {
                args.next()
                    .ok_or_else(|| format!("{name} expects a directory"))
            };
            match flag.as_str() {
                "--workspace" => workspace = Some(PathBuf::from(value("--workspace")?)),
                "--root" => root = Some(PathBuf::from(value("--root")?)),
                "--promo" => promo = Some(PathBuf::from(value("--promo")?)),
                "--log" => log = Some(PathBuf::from(value("--log")?)),
                other => return Err(format!("unknown flag `{other}`")),
            }
        }
        Ok(Config {
            workspace: workspace.unwrap_or_else(default_workspace),
            root,
            promo,
            log,
        })
    }
}

/// $PROMOSHOT_WORKSPACE, else the XDG data directory. Not created until the
/// workspace tool is actually asked — a server that only validates should
/// leave no footprint.
fn default_workspace() -> PathBuf {
    if let Ok(dir) = std::env::var("PROMOSHOT_WORKSPACE") {
        return PathBuf::from(dir);
    }
    if let Ok(data) = std::env::var("XDG_DATA_HOME") {
        return Path::new(&data).join("promoshot");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".local/share/promoshot")
}

/// The `promo` binary: an explicit --promo wins, then a sibling of this
/// executable (how a built target dir and an installed pair both look), then
/// whatever PATH holds.
fn promo_binary(config: &Config) -> PathBuf {
    if let Some(path) = &config.promo {
        return path.clone();
    }
    if let Ok(me) = std::env::current_exe() {
        let sibling = me.with_file_name("promo");
        if sibling.exists() {
            return sibling;
        }
    }
    PathBuf::from("promo")
}

/// Runs the CLI and hands back stdout, or stderr as the error. The CLI
/// already writes human-usable answers on both streams; nothing here needs
/// to interpret them. A FAILED SPAWN explains itself — "No such file" cost
/// a fresh Linux box a debugging session (issue #2) when all it meant was
/// "the render CLI is not installed yet".
fn run_promo(config: &Config, args: &[String]) -> Result<String, String> {
    let binary = promo_binary(config);
    let output = std::process::Command::new(&binary)
        .args(args)
        .output()
        .map_err(|e| {
            format!(
                "could not run the `promo` CLI at `{}` ({e}). Every render \
                 shells to that binary. Fix one of: put `promo` beside \
                 promoshot-mcp, start the server with --promo /path/to/promo, \
                 or install it — download a release binary from \
                 github.com/GarAlex/promoshot/releases, or build with \
                 `cargo build --release -p promo-cli`.",
                binary.display()
            )
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if output.status.success() {
        Ok(stdout)
    } else {
        Err(if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        })
    }
}

/// One request in, at most one response out. Notifications (no id) answer
/// nothing, per JSON-RPC.
fn handle<R>(request: &Value, config: &Config, run: &R) -> Option<Value>
where
    R: Fn(&Config, &[String]) -> Result<String, String>,
{
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let result = match method {
        "initialize" => Ok(initialize(request)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": contract::tools(Host::Headless) })),
        "tools/call" => Ok(call(request, config, run)),
        _ if id.is_none() => return None, // notifications/initialized and kin
        other => Err(format!("method `{other}` is not supported")),
    };
    let id = id?;
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(message) => json!({
            "jsonrpc": "2.0", "id": id,
            "error": { "code": -32601, "message": message }
        }),
    })
}

fn initialize(request: &Value) -> Value {
    // Answer in the client's protocol dialect when it names one; this server
    // uses nothing that has changed across revisions.
    let version = request
        .pointer("/params/protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(PROTOCOL_FALLBACK);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": "promoshot-mcp",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "instructions": contract::instructions(Host::Headless),
    })
}

fn call<R>(request: &Value, config: &Config, run: &R) -> Value
where
    R: Fn(&Config, &[String]) -> Result<String, String>,
{
    let name = request
        .pointer("/params/name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let empty = json!({});
    let args = request.pointer("/params/arguments").unwrap_or(&empty);
    let started = std::time::Instant::now();
    // The contract first: a tool this server does not serve, or an
    // argument the tool does not take, is refused by name — both servers
    // used to ignore an unknown argument and do something else.
    let outcome = contract::check_arguments(Host::Headless, name, args)
        .and_then(|()| dispatch_tool(name, args, config, run));
    if let Some(path) = &config.log {
        // One line per call, appended: when, which tool, how long, how it
        // went. The timing a caller's own log cannot see — the render, the
        // probe, the validate — measured on this side of the wire.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let line = format!(
            "{now}\t{name}\t{}\t{}\n",
            started.elapsed().as_millis(),
            if outcome.is_ok() { "ok" } else { "error" }
        );
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            use std::io::Write;
            let _ = file.write_all(line.as_bytes());
        }
    }
    match outcome {
        Ok(text) => {
            // The authoring pair and validate answer with a glance attached
            // — and a failed glance never fails the call it rides on.
            let mut content = vec![json!({ "type": "text", "text": text })];
            if preview::wanted(name, args) {
                match preview::thumbnail(name, args, config, run) {
                    Ok(image) => content.push(image),
                    Err(note) => {
                        content[0]["text"] =
                            json!(format!("{text}\n(preview unavailable: {note})"));
                    }
                }
            }
            json!({ "content": content })
        }
        Err(message) => json!({
            "content": [{ "type": "text", "text": message }],
            "isError": true
        }),
    }
}

fn dispatch_tool<R>(name: &str, args: &Value, config: &Config, run: &R) -> Result<String, String>
where
    R: Fn(&Config, &[String]) -> Result<String, String>,
{
    match name {
        "promo_schema" => Ok(promo_model::SCHEMA_QUICK.to_string()),
        "promo_media_probe" => {
            media_fence(args, config)?;
            let is_model = args
                .get("file")
                .and_then(Value::as_str)
                .is_some_and(|f| f.to_ascii_lowercase().ends_with(".glb"));
            if is_model {
                let file = args["file"].as_str().unwrap_or_default();
                let file = std::fs::canonicalize(file)
                    .map(|p| p.display().to_string())
                    .map_err(|_| format!("file {file} does not exist"))?;
                run(config, &["model".to_string(), file, "--json".into()])
            } else {
                media::probe_many(args)
            }
        }
        "promo_media_turntable" => {
            media_fence(args, config)?;
            let file = args
                .get("file")
                .and_then(Value::as_str)
                .ok_or("promo_media_turntable: `file` is required")?
                .to_string();
            // Absolute and real, so the CLI never reads it as a flag.
            let file = std::fs::canonicalize(&file)
                .map(|p| p.display().to_string())
                .map_err(|_| format!("file {file} does not exist"))?;
            let stem = Path::new(&file)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("model")
                .to_string();
            let out = match args.get("out").and_then(Value::as_str) {
                Some(out) => out.to_string(),
                None => config
                    .workspace
                    .join(format!("turntable-{stem}.png"))
                    .display()
                    .to_string(),
            };
            let mut argv = vec![
                "turntable".to_string(),
                file,
                "--out".into(),
                out,
                "--json".into(),
            ];
            if let Some(n) = args.get("count").and_then(Value::as_u64) {
                argv.extend(["--count".into(), n.to_string()]);
            }
            if let Some(px) = args.get("size").and_then(Value::as_u64) {
                argv.extend(["--size".into(), format!("{px}x{px}")]);
            }
            run(config, &argv)
        }
        "promo_media_filmstrip" => {
            media_fence(args, config)?;
            media::filmstrip(args, &config.workspace)
        }
        "promo_media_silences" => {
            media_fence(args, config)?;
            media::silences(args)
        }
        "promo_media_scenes" => {
            media_fence(args, config)?;
            media::scenes(args)
        }
        "promo_transcribe" => {
            media_fence(args, config)?;
            media::transcribe(args)
        }
        "promo_explain" => {
            let answer = promo_author::explain(args, config.root.as_deref())?;
            // A model showing something on a slot: where it lands is the
            // renderer's to measure (review 2026-09-27, P2-36), so the CLI —
            // which renders — answers instead, and a failed measurement
            // still leaves the project-only answer.
            if !answer.contains("\"shows\"") {
                return Ok(answer);
            }
            let project = fenced_project(args, config)?;
            let mut asked = args.clone();
            if let Some(map) = asked.as_object_mut() {
                map.remove("project");
            }
            match run(
                config,
                &[
                    "explain".to_string(),
                    project,
                    "--args".into(),
                    asked.to_string(),
                ],
            ) {
                Ok(measured) => Ok(measured),
                Err(why) => Ok(format!("{answer}\n(slot placement not measured: {why})")),
            }
        }
        "promo_diff" => promo_author::diff(args, config.root.as_deref()),
        "promo_init" => promo_author::init(args, config.root.as_deref()),
        "promo_upsert_layer" => {
            promo_author::upsert_layer(args, config.root.as_deref(), &media::host_probe)
        }
        "promo_upsert_keyframe" => promo_author::upsert_keyframe(args, config.root.as_deref()),
        "promo_apply" => promo_author::apply(args, config.root.as_deref()),
        "promo_slideshow" => {
            promo_author::slideshow(args, config.root.as_deref(), &media::host_probe)
        }
        "promo_schema_full" => {
            let topics: Vec<String> = args
                .get("topics")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            Ok(contract::schema_text(&topics))
        }
        "promo_schema_types" => {
            let types: Vec<String> = args
                .get("types")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            contract::schema_types_text(&types)
        }
        "promo_workspace" => {
            std::fs::create_dir_all(&config.workspace)
                .map_err(|e| format!("could not create workspace: {e}"))?;
            Ok(config.workspace.display().to_string())
        }
        "promo_validate" | "promo_inspect" => {
            let project = fenced_project(args, config)?;
            let command = name.trim_start_matches("promo_");
            run(config, &[command.into(), project])
        }
        "promo_render_still" => {
            let project = fenced_project(args, config)?;
            let time = args.get("time").and_then(Value::as_f64).unwrap_or(0.0);
            let out = default_out(args, "out", &project, &format!("still-{time}s.png"))?;
            let mut argv = vec!["still".to_string(), project, "--out".into(), out];
            if let Some(policy) = args.get("proxy").and_then(Value::as_str) {
                argv.extend(["--proxy".into(), policy.to_string()]);
            }
            argv.extend(["--time".into(), time.to_string()]);
            push_size(&mut argv, args);
            run(config, &argv)
        }
        "promo_render_frames" => {
            let project = fenced_project(args, config)?;
            let out = default_out(args, "outDir", &project, "frames")?;
            let mut argv = vec!["frames".to_string(), project.clone(), "--out".into(), out];
            if let Some(policy) = args.get("proxy").and_then(Value::as_str) {
                argv.extend(["--proxy".into(), policy.to_string()]);
            }
            // The ask, the tool's defaults applied (bare: a sample of twelve
            // moments), is the contract's — the app's server reads the same
            // request, and the CLI picks the moments by the same rule.
            let ask = contract::look_request(args);
            if !ask.times.is_empty() {
                let times: Vec<String> = ask.times.iter().map(f64::to_string).collect();
                argv.extend(["--times".into(), times.join(",")]);
            }
            for (value, flag) in [(ask.from, "--from"), (ask.to, "--to"), (ask.fps, "--fps")] {
                if let Some(v) = value {
                    argv.extend([flag.to_string(), v.to_string()]);
                }
            }
            if let Some(n) = ask.sample {
                argv.extend(["--sample".into(), n.to_string()]);
            }
            // The tool's own ceiling, which the CLI does not have: a person
            // may fairly ask for three thousand frames, a tool call may not.
            argv.extend(["--cap".into(), contract::FRAME_CAP.to_string()]);
            argv.extend(["--sheet".into(), sheet_path(&project)]);
            push_size(&mut argv, args);
            run(config, &argv)
        }
        "promo_render_video" => {
            let project = fenced_project(args, config)?;
            // ProRes (and alpha, which is ProRes 4444) is a QuickTime movie:
            // the default name says so, and an .mp4 path is refused as the
            // app's server refuses it.
            let prores = args.get("alpha").and_then(Value::as_bool) == Some(true)
                || args
                    .get("codec")
                    .and_then(Value::as_str)
                    .is_some_and(|c| c.starts_with("prores"));
            if prores
                && args
                    .get("out")
                    .and_then(Value::as_str)
                    .is_some_and(|o| !o.to_ascii_lowercase().ends_with(".mov"))
            {
                return Err(
                    "ProRes writes a QuickTime movie — name an `out` ending in .mov".into(),
                );
            }
            let default_name = if prores { "export.mov" } else { "export.mp4" };
            let out = default_out(args, "out", &project, default_name)?;
            let mut argv = vec!["video".to_string(), project, "--out".into(), out];
            if let Some(policy) = args.get("proxy").and_then(Value::as_str) {
                argv.extend(["--proxy".into(), policy.to_string()]);
            }
            if let Some(codec) = args.get("codec").and_then(Value::as_str) {
                argv.extend(["--codec".into(), codec.to_string()]);
            }
            if args.get("alpha").and_then(Value::as_bool) == Some(true) {
                argv.push("--alpha".into());
            }
            if let Some(fps) = args.get("fps").and_then(Value::as_f64) {
                argv.extend(["--fps".into(), fps.to_string()]);
            }
            push_size(&mut argv, args);
            run(config, &argv)
        }
        "promo_proxy" => {
            let project = fenced_project(args, config)?;
            run(config, &["proxy".to_string(), project, "--json".into()])
        }
        "promo_render_gif" => {
            let project = fenced_project(args, config)?;
            let out = default_out(args, "out", &project, "export.gif")?;
            let mut argv = vec!["gif".to_string(), project, "--out".into(), out];
            if let Some(policy) = args.get("proxy").and_then(Value::as_str) {
                argv.extend(["--proxy".into(), policy.to_string()]);
            }
            if let Some(fps) = args.get("fps").and_then(Value::as_f64) {
                argv.extend(["--fps".into(), fps.to_string()]);
            }
            push_size(&mut argv, args);
            run(config, &argv)
        }
        "promo_voices" => speak::voices(args, &promo_speech::SystemKeys),
        "promo_speak" => speak::speak(
            args,
            config.root.as_deref(),
            &speak::live(),
            &promo_speech::SystemKeys,
            &|path| {
                media::host_probe(path, true)
                    .duration
                    .ok_or_else(|| format!("could not measure {}", path.display()))
            },
        ),
        other => Err(format!("unknown tool `{other}`")),
    }
}

fn push_size(argv: &mut Vec<String>, args: &Value) {
    if let Some(size) = args.get("size").and_then(Value::as_str) {
        argv.extend(["--size".into(), size.into()]);
    }
}

/// The project path, canonicalized, and inside --root when a root is set.
/// The fence is on the PROJECT, which every file the CLI reads or writes
/// lives under — output defaults included.
fn fenced_project(args: &Value, config: &Config) -> Result<String, String> {
    let raw = args
        .get("project")
        .and_then(Value::as_str)
        .ok_or("`project` is required")?;
    let path = std::fs::canonicalize(raw).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!(
                "no project folder at `{raw}` — create one with promo_init (or promo_slideshow), \
                 in the folder promo_workspace names"
            )
        } else {
            format!("project `{raw}`: {e}")
        }
    })?;
    if let Some(root) = &config.root {
        let root = std::fs::canonicalize(root).map_err(|e| format!("--root: {e}"))?;
        if !path.starts_with(&root) {
            return Err(format!(
                "project `{}` is outside the served root `{}`",
                path.display(),
                root.display()
            ));
        }
    }
    Ok(path.display().to_string())
}

/// Where a frames call leaves its contact sheet: one fixed place per
/// project, beside the other exports rather than inside the frames folder,
/// so `frame-*.png` stays a clean glob for ffmpeg and the preview knows
/// where to look without re-deriving `outDir`.
fn sheet_path(project: &str) -> String {
    exports_dir(project)
        .join("frames-sheet.png")
        .display()
        .to_string()
}

/// Where a project's outputs go: BESIDE it, never inside. `<parent>/<Name>
/// Exports/` for `<parent>/<Name>.promo` (a project folder without the
/// extension keeps its whole name). An export is an output, not part of
/// the work — a project that swallowed its own renders grew to gigabytes
/// and travelled that way — and the apps keep the same rule.
pub(crate) fn exports_dir(project: &str) -> std::path::PathBuf {
    let path = Path::new(project);
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.strip_suffix(".promo").unwrap_or(n))
        .unwrap_or("project");
    path.parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
        .join(format!("{name} Exports"))
}

/// A media tool reads the files it is named inside `--root` when one is
/// served, as a project tool does (review 2026-09-27, P2-38) — the media
/// tools read whatever they were told, from anywhere. A file that is not
/// there is left to the tool, which says so.
fn media_fence(args: &Value, config: &Config) -> Result<(), String> {
    let Some(root) = &config.root else {
        return Ok(());
    };
    let root = std::fs::canonicalize(root).map_err(|e| format!("--root: {e}"))?;
    let mut named: Vec<&str> = Vec::new();
    if let Some(file) = args.get("file").and_then(Value::as_str) {
        named.push(file);
    }
    if let Some(list) = args.get("files").and_then(Value::as_array) {
        named.extend(list.iter().filter_map(Value::as_str));
    }
    for raw in named {
        if let Ok(real) = std::fs::canonicalize(raw) {
            if !real.starts_with(&root) {
                return Err(format!(
                    "file `{raw}` is outside the served root `{}`",
                    root.display()
                ));
            }
        }
    }
    Ok(())
}

/// An explicit output path wins; otherwise the project's exports folder,
/// created on the way — the same default the app's own tools use.
fn default_out(args: &Value, key: &str, project: &str, filename: &str) -> Result<String, String> {
    if let Some(out) = args.get(key).and_then(Value::as_str) {
        return Ok(out.to_string());
    }
    let exports = exports_dir(project);
    std::fs::create_dir_all(&exports)
        .map_err(|e| format!("could not create {}: {e}", exports.display()))?;
    Ok(exports.join(filename).display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config {
            workspace: std::env::temp_dir().join("promoshot-mcp-test-ws"),
            root: None,
            log: None,
            promo: None,
        }
    }

    /// A runner that records the argv it was handed and answers canned text.
    fn recording(
        seen: &std::cell::RefCell<Vec<Vec<String>>>,
    ) -> impl Fn(&Config, &[String]) -> Result<String, String> + '_ {
        move |_, args| {
            seen.borrow_mut().push(args.to_vec());
            Ok("ran".into())
        }
    }

    fn never(_: &Config, _: &[String]) -> Result<String, String> {
        panic!("this tool must not shell out")
    }

    /// The contract is enforced on the wire (review 2026-09-27, P2-32): an
    /// argument the tool does not take, or a tool this server does not
    /// serve, is refused by name and nothing runs. Both servers used to
    /// ignore an unknown argument and do something else.
    #[test]
    fn a_call_outside_the_contract_is_refused_and_nothing_runs() {
        let call = |name: &str, arguments: Value| {
            let seen = std::cell::RefCell::new(Vec::new());
            let req = serde_json::json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/call",
                "params": { "name": name, "arguments": arguments } });
            let answer = handle(&req, &config(), &recording(&seen)).unwrap();
            let ran = !seen.borrow().is_empty();
            (answer, ran)
        };
        let (answer, ran) = call(
            "promo_render_still",
            serde_json::json!({ "project": "/tmp/x.promo", "scale": 50 }),
        );
        assert_eq!(answer["result"]["isError"], true);
        let text = answer["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("does not take `scale`"), "{text}");
        assert!(text.contains("`size`"), "it names what it takes: {text}");
        assert!(!ran, "nothing ran");
        let (answer, ran) = call(
            "promo_open",
            serde_json::json!({ "project": "/tmp/x.promo" }),
        );
        let text = answer["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("unknown tool `promo_open`"), "{text}");
        assert!(!ran);
    }

    /// Explain on a project whose model shows something on a slot asks the
    /// CLI, which renders — where a screen lands is measured, not guessed
    /// (review 2026-09-27, P2-36); a project without one never leaves the
    /// process.
    #[test]
    fn explain_asks_the_renderer_where_a_screen_lands() {
        let dir = std::env::temp_dir().join(format!("mcp-explain3d-{}.promo", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |materials: Value| {
            std::fs::write(
                dir.join("metadata.json"),
                serde_json::json!({
                    "id": "P", "name": "E", "createdAt": 0, "state": "recorded",
                    "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
                    "compositionSettings": { "canvasWidth": 64, "canvasHeight": 64 },
                    "resources": [
                        { "id": "pic", "kind": "image", "filename": "p.png", "displayName": "p", "addedAt": 0 },
                        { "id": "phone", "kind": "model", "filename": "", "displayName": "Phone",
                          "addedAt": 0, "recipe": { "device": { "kind": "phone" } },
                          "materials": materials }],
                    "layers": [{ "id": "m", "name": "m", "sortIndex": 0, "kind": "model",
                                 "isEnabled": true, "startTime": 0, "duration": 2,
                                 "resourceID": "phone", "keyframes": [] }]
                })
                .to_string(),
            )
            .unwrap();
        };
        let call = || {
            let seen = std::cell::RefCell::new(Vec::new());
            let req = serde_json::json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call",
                "params": { "name": "promo_explain",
                            "arguments": { "project": dir.display().to_string(), "time": 1 } } });
            let answer = handle(&req, &config(), &recording(&seen)).unwrap();
            let argv = seen.borrow().first().cloned();
            (answer, argv)
        };
        write(serde_json::json!({ "Screen": { "resourceID": "pic" } }));
        let (_, argv) = call();
        let argv = argv.expect("the CLI was asked");
        assert_eq!(argv[0], "explain");
        assert_eq!(argv[2], "--args");
        assert!(argv[3].contains("\"time\":1"), "{argv:?}");
        write(serde_json::json!({ "Body": "@accent" }));
        let (answer, argv) = call();
        assert!(
            argv.is_none(),
            "no slot shows anything: answered in process"
        );
        assert!(answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("\"camera\""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A project that is not there says how to make one — the refusal
    /// used to be the OS's "No such file or directory" (review 2026-09-27,
    /// P2-33).
    #[test]
    fn a_missing_project_says_how_to_make_one() {
        let req = serde_json::json!({ "jsonrpc": "2.0", "id": 8, "method": "tools/call",
            "params": { "name": "promo_inspect", "arguments": { "project": "/nowhere/Gone.promo" } } });
        let answer = handle(&req, &config(), &never).unwrap();
        let text = answer["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("no project folder at `/nowhere/Gone.promo`"),
            "{text}"
        );
        assert!(
            text.contains("promo_init") && text.contains("promo_workspace"),
            "{text}"
        );
    }

    /// ProRes is a QuickTime movie on both servers: the default name is
    /// export.mov, and an .mp4 path is refused before anything renders.
    #[test]
    fn a_prores_render_is_a_movie() {
        let project = std::env::temp_dir().join(format!("mcp-prores-{}.promo", std::process::id()));
        std::fs::create_dir_all(&project).unwrap();
        let path = project.display().to_string();
        let seen = std::cell::RefCell::new(Vec::new());
        let req = |arguments: Value| {
            serde_json::json!({ "jsonrpc": "2.0", "id": 6, "method": "tools/call",
                "params": { "name": "promo_render_video", "arguments": arguments } })
        };
        handle(
            &req(serde_json::json!({ "project": path, "codec": "prores4444" })),
            &config(),
            &recording(&seen),
        )
        .unwrap();
        let argv = seen.borrow()[0].clone();
        let out = &argv[argv.iter().position(|a| a == "--out").unwrap() + 1];
        assert!(out.ends_with("export.mov"), "{out}");
        let refused = handle(
            &req(serde_json::json!({ "project": path, "alpha": true, "out": "/tmp/a.mp4" })),
            &config(),
            &recording(&seen),
        )
        .unwrap();
        assert_eq!(refused["result"]["isError"], true);
        assert_eq!(seen.borrow().len(), 1, "the refused call ran nothing");
        let _ = std::fs::remove_dir_all(&project);
        let _ = std::fs::remove_dir_all(project.with_file_name(format!(
            "{} Exports",
            project.file_stem().unwrap().to_string_lossy()
        )));
    }

    #[test]
    fn the_handshake_names_the_server_and_offers_tools() {
        let req = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18" } });
        let answer = handle(&req, &config(), &never).expect("initialize answers");
        assert_eq!(
            answer.pointer("/result/protocolVersion").unwrap(),
            "2025-06-18",
            "the client's dialect is echoed"
        );
        assert_eq!(
            answer.pointer("/result/serverInfo/name").unwrap(),
            "promoshot-mcp"
        );
        assert!(answer.pointer("/result/capabilities/tools").is_some());
    }

    #[test]
    fn a_notification_answers_nothing() {
        let req = serde_json::json!({ "jsonrpc": "2.0",
            "method": "notifications/initialized" });
        assert!(handle(&req, &config(), &never).is_none());
    }

    #[test]
    fn the_tool_list_is_the_offered_surface() {
        let req = serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
        let answer = handle(&req, &config(), &never).unwrap();
        let names: Vec<&str> = answer
            .pointer("/result/tools")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "promo_validate",
                "promo_inspect",
                "promo_schema",
                "promo_schema_types",
                "promo_schema_full",
                "promo_render_still",
                "promo_render_frames",
                "promo_render_video",
                "promo_render_gif",
                "promo_proxy",
                "promo_workspace",
                "promo_media_probe",
                "promo_media_turntable",
                "promo_media_filmstrip",
                "promo_media_silences",
                "promo_media_scenes",
                "promo_transcribe",
                "promo_init",
                "promo_upsert_layer",
                "promo_upsert_keyframe",
                "promo_apply",
                "promo_slideshow",
                "promo_explain",
                "promo_diff",
                "promo_voices",
                "promo_speak"
            ],
            "everything the app offers except promo_open, which needs a window"
        );
    }

    /// Every tool says what it does to the world. A host that gates on
    /// `readOnlyHint` has to assume the worst without them — asking for the
    /// format's schema looked like asking for a video render — and the
    /// answers have to be TRUE: nothing that writes into a project may
    /// claim to be read-only.
    #[test]
    fn every_tool_says_what_it_does_to_the_world() {
        let tools = Value::Array(contract::tools(Host::Headless));
        let tools = tools.as_array().unwrap();
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            let a = &tool["annotations"];
            assert!(a.is_object(), "{name} carries no annotations");
            for hint in [
                "readOnlyHint",
                "destructiveHint",
                "idempotentHint",
                "openWorldHint",
            ] {
                assert!(a[hint].is_boolean(), "{name}.{hint}");
            }
            assert!(
                a["title"]
                    .as_str()
                    .is_some_and(|t| !t.is_empty() && t != name),
                "{name} has a human title"
            );
        }
        let read_only = |name: &str| {
            tools
                .iter()
                .find(|t| t["name"] == name)
                .map(|t| t["annotations"]["readOnlyHint"] == true)
                .unwrap()
        };
        assert!(read_only("promo_schema") && read_only("promo_inspect"));
        // These write: a render into Exports, a scaffold into metadata.json,
        // and validate's own glance at Exports/preview.png.
        for writer in [
            "promo_validate",
            "promo_render_still",
            "promo_render_frames",
            "promo_render_video",
            "promo_init",
            "promo_upsert_layer",
            "promo_apply",
            "promo_speak",
        ] {
            assert!(
                !read_only(writer),
                "{writer} writes and must not claim otherwise"
            );
        }
        let apply = tools.iter().find(|t| t["name"] == "promo_apply").unwrap();
        assert_eq!(
            apply["annotations"]["destructiveHint"], true,
            "the one door that deletes says so"
        );
        let voices = tools.iter().find(|t| t["name"] == "promo_voices").unwrap();
        assert_eq!(
            voices["annotations"]["openWorldHint"], true,
            "it calls a provider"
        );
    }

    /// The handshake says how to use the server. A registry or Docker
    /// install has no skill beside it — 27 tools and no loop — and
    /// `instructions` is the one place the protocol lets a server say it.
    #[test]
    fn the_handshake_teaches_the_loop() {
        let req = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18" } });
        let answer = handle(&req, &config(), &never).expect("initialize answers");
        let text = answer
            .pointer("/result/instructions")
            .and_then(Value::as_str)
            .expect("instructions are offered");
        for step in [
            "promo_schema",
            "promo_workspace",
            "promo_validate",
            "promo_render_frames",
            "metadata.json",
        ] {
            assert!(text.contains(step), "the loop names {step}");
        }
        assert!(text.len() < 1_400, "and stays short: {} bytes", text.len());
    }

    #[test]
    fn schema_is_answered_in_process() {
        let req = serde_json::json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "promo_schema" } });
        let answer = handle(&req, &config(), &never).unwrap();
        let text = answer
            .pointer("/result/content/0/text")
            .and_then(Value::as_str)
            .unwrap();
        assert!(
            text.contains("minReaderVersion"),
            "the compiled-in format text, not a stub"
        );
        assert!(
            text.contains("promo_schema_full"),
            "the subset names the full door"
        );
    }

    #[test]
    fn a_still_defaults_its_output_into_exports() {
        let project = std::env::temp_dir().join(format!("mcp-still-{}", std::process::id()));
        std::fs::create_dir_all(&project).unwrap();
        let seen = std::cell::RefCell::new(Vec::new());
        let req = serde_json::json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "promo_render_still",
                "arguments": { "project": project.display().to_string(), "time": 2.5 } } });
        handle(&req, &config(), &recording(&seen)).unwrap();
        let argv = seen.borrow()[0].clone();
        assert_eq!(argv[0], "still");
        let out = argv[argv.iter().position(|a| a == "--out").unwrap() + 1].clone();
        // Compared by component: the separator is the platform's, and on
        // Windows the path also carries canonicalize's \\?\ prefix — a
        // substring match with '/' tests a Unix spelling, not the rule.
        let out_path = Path::new(&out);
        assert_eq!(
            out_path.file_name().and_then(|n| n.to_str()),
            Some("still-2.5s.png"),
            "defaulted still name: {out}"
        );
        let beside = format!("{} Exports", project.file_name().unwrap().to_str().unwrap());
        assert_eq!(
            out_path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str()),
            Some(beside.as_str()),
            "defaulted BESIDE the project, into its exports folder: {out}"
        );
        assert!(
            !out_path.starts_with(&project),
            "never inside the project: {out}"
        );
        assert_eq!(
            exports_dir("/tmp/Show.promo"),
            Path::new("/tmp/Show Exports"),
            "a package's extension is not part of the folder's name"
        );
        assert!(
            Path::new(&out).parent().unwrap().is_dir(),
            "and Exports exists"
        );
        std::fs::remove_dir_all(&project).unwrap();
    }

    /// A bare frames call is a LOOK: twelve sampled moments, a ceiling, and
    /// a contact sheet — not every frame of the composition. Asked for a
    /// range or exact times it renders those instead, and the sheet and the
    /// cap ride along either way.
    /// The whole tool surface has a CEILING, because every client loads it
    /// on connect and carries it in every request afterwards. It was 118 KB
    /// — 29k tokens, 83% of it promo_apply's hoisted type graph — before
    /// the references became placeholders.
    #[test]
    fn the_tool_surface_stays_small() {
        let bytes = Value::Array(contract::tools(Host::Headless))
            .to_string()
            .len();
        assert!(
            bytes < 45_000,
            "tools/list is {bytes} bytes; it was 118,533 before the type graph came out, \
             and every request pays for it"
        );
        // And the ceiling is not met by dropping tools or their prose.
        let tools = Value::Array(contract::tools(Host::Headless));
        let tools = tools.as_array().unwrap();
        assert_eq!(tools.len(), 26);
        assert!(
            tools
                .iter()
                .all(|t| t["description"].as_str().is_some_and(|d| d.len() > 40)),
            "every tool still says what it is for"
        );
    }

    #[test]
    fn frames_samples_and_caps_and_asks_for_a_sheet() {
        let project = std::env::temp_dir().join(format!("mcp-frames-{}", std::process::id()));
        std::fs::create_dir_all(&project).unwrap();
        let call = |arguments: Value| {
            let seen = std::cell::RefCell::new(Vec::new());
            let req = serde_json::json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call",
                "params": { "name": "promo_render_frames", "arguments": arguments } });
            handle(&req, &config(), &recording(&seen)).unwrap();
            let argv = seen.borrow()[0].clone();
            argv
        };
        let after = |argv: &[String], flag: &str| -> Option<String> {
            argv.iter()
                .position(|a| a == flag)
                .map(|i| argv[i + 1].clone())
        };

        let bare = call(serde_json::json!({ "project": project.display().to_string() }));
        assert_eq!(bare[0], "frames");
        assert_eq!(
            after(&bare, "--sample"),
            Some(contract::SAMPLE_FRAMES.to_string()),
            "{bare:?}"
        );
        assert_eq!(after(&bare, "--cap"), Some(contract::FRAME_CAP.to_string()));
        let sheet = after(&bare, "--sheet").expect("a sheet is always asked for");
        assert_eq!(
            Path::new(&sheet).file_name().and_then(|n| n.to_str()),
            Some("frames-sheet.png"),
            "{sheet}"
        );

        // A range means "every frame in it" — the sampling default steps
        // aside, and the ceiling does not.
        let ranged = call(serde_json::json!({
            "project": project.display().to_string(), "from": 0, "to": 2, "fps": 12 }));
        assert_eq!(after(&ranged, "--sample"), None, "{ranged:?}");
        assert_eq!(after(&ranged, "--fps"), Some("12".into()));
        assert_eq!(
            after(&ranged, "--cap"),
            Some(contract::FRAME_CAP.to_string())
        );

        let listed = call(serde_json::json!({
            "project": project.display().to_string(), "times": [0.5, 2.0] }));
        assert_eq!(
            after(&listed, "--times"),
            Some("0.5,2".into()),
            "{listed:?}"
        );
        assert_eq!(after(&listed, "--sample"), None, "exact moments win");

        // And an explicit sample beats the default.
        let counted = call(serde_json::json!({
            "project": project.display().to_string(), "sample": 4 }));
        assert_eq!(after(&counted, "--sample"), Some("4".into()));
        std::fs::remove_dir_all(&project).unwrap();
    }

    #[test]
    fn the_root_fence_refuses_a_project_outside_it() {
        let inside = std::env::temp_dir().join(format!("mcp-root-{}", std::process::id()));
        std::fs::create_dir_all(&inside).unwrap();
        let outside = std::env::temp_dir().join(format!("mcp-out-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        let fenced = Config {
            root: Some(inside.clone()),
            ..config()
        };
        let req = serde_json::json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": { "name": "promo_validate",
                "arguments": { "project": outside.display().to_string() } } });
        let answer = handle(&req, &fenced, &never).unwrap();
        assert_eq!(
            answer.pointer("/result/isError"),
            Some(&Value::Bool(true)),
            "refused, as a tool error the client can read"
        );
        std::fs::remove_dir_all(&inside).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
    }

    /// The registry manifest names the version being released and its
    /// image: server.json is published from the tagged commit, so a bump
    /// that forgot it would announce the previous image as the new one.
    #[test]
    fn the_registry_manifest_names_this_version() {
        let manifest: Value = serde_json::from_str(include_str!("../../server.json")).unwrap();
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(manifest["version"], version);
        let image = manifest
            .pointer("/packages/0/identifier")
            .and_then(Value::as_str)
            .unwrap_or("");
        assert!(image.ends_with(&format!(":v{version}")), "{image}");
    }

    /// The media tools read inside the served root too, and hand ffprobe
    /// a real absolute path, never a bare name that could be an option
    /// (review 2026-09-27, P2-38).
    #[test]
    fn media_tools_read_inside_the_root() {
        let inside = std::env::temp_dir().join(format!("mcp-media-in-{}", std::process::id()));
        let outside = std::env::temp_dir().join(format!("mcp-media-out-{}", std::process::id()));
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("clip.mp4"), b"not really").unwrap();
        let fenced = Config {
            root: Some(inside.clone()),
            ..config()
        };
        for (name, arguments) in [
            (
                "promo_media_probe",
                serde_json::json!({ "file": outside.join("clip.mp4") }),
            ),
            (
                "promo_media_probe",
                serde_json::json!({ "files": [outside.join("clip.mp4")] }),
            ),
            (
                "promo_media_scenes",
                serde_json::json!({ "file": outside.join("clip.mp4") }),
            ),
            (
                "promo_media_turntable",
                serde_json::json!({ "file": outside.join("clip.mp4") }),
            ),
        ] {
            let req = serde_json::json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/call",
                "params": { "name": name, "arguments": arguments } });
            let answer = handle(&req, &fenced, &never).unwrap();
            let text = answer["result"]["content"][0]["text"].as_str().unwrap();
            assert!(text.contains("outside the served root"), "{name}: {text}");
        }
        let _ = std::fs::remove_dir_all(&inside);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// The shipped skill (skill/SKILL.md) is the workflow layer over this
    /// server, and a skill that names tools the server does not offer — or
    /// misses ones it does — teaches wrongly. Held here, where the tool
    /// list lives, the same discipline as the app's SkillDriftTests.
    /// Phrases that were true once and taught agents wrong after the format
    /// moved on (review 2026-09-27). None may come back in anything an agent
    /// reads: the skill, both schemas, the handshake, the tool descriptions.
    /// The app's twin of this test reads its own descriptions.
    #[test]
    fn no_stale_phrase_reaches_an_agent() {
        const STALE: &[&str] = &[
            "Every id is a UUID",
            "ordinary JSON edit",
            "Stamp `\"minReaderVersion\"",
            "stamp \"minReaderVersion\"",
            "think no more about it",
            "never video",
            "video layers cannot swap",
            "codec: \"prores\"",
            // `ok` came back beside missing media and fields nothing read;
            // the verdict is NOT OK / ok now, and "ok" no longer promises.
            "means it will render",
        ];
        let tools = Value::Array(contract::tools(Host::Headless)).to_string();
        let handshake = contract::instructions(Host::Headless);
        let sources = [
            ("SKILL.md", include_str!("../../skill/SKILL.md")),
            ("schema-quick.md", promo_model::SCHEMA_QUICK),
            ("schema.md", promo_model::SCHEMA),
            ("the handshake", handshake.as_str()),
            ("the tool descriptions", tools.as_str()),
        ];
        for (name, text) in sources {
            for phrase in STALE {
                assert!(!text.contains(phrase), "{name} still says \"{phrase}\"");
            }
        }
    }

    #[test]
    fn the_skill_teaches_exactly_the_tools_the_server_offers() {
        let skill = include_str!("../../skill/SKILL.md");
        assert!(skill.starts_with("---\n"), "front matter, so it installs");
        let tools = Value::Array(contract::tools(Host::Headless));
        for tool in tools.as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            assert!(skill.contains(name), "the skill never mentions `{name}`");
        }
        // The stamp is COMPUTED — by `write_metadata` on the way out, and
        // named by `promo_validate` when a hand-written file declares one
        // too low. This assertion used to pin the skill to teaching a
        // literal 19, and went stale while the ladder climbed to 42: a
        // test can pin the wrong thing as easily as the right one.
        assert!(
            skill.contains("Never guess `minReaderVersion`"),
            "the skill must teach that the stamp is computed, not a number"
        );
        for stale in [
            "minReaderVersion\": 19",
            "minReaderVersion: 19",
            "minReaderVersion: 34",
        ] {
            assert!(
                !skill.contains(stale),
                "the skill states a literal stamp: {stale}"
            );
        }
        assert!(
            !skill.contains("owns the file"),
            "one-way ownership was repealed by SPECS D5 stages 1-3 — \
             the file is shared and every writer merges"
        );
    }

    /// The glance: an authoring call answers text PLUS an image block, the
    /// still is sampled at the touched layer's midpoint (never a fade-in's
    /// empty t=0), sized to the canvas aspect, and written to the stable
    /// Exports/preview.png a person can keep open.
    #[test]
    fn authoring_answers_with_a_thumbnail_of_the_touched_layer() {
        let project = std::env::temp_dir().join(format!("mcp-thumb-{}", std::process::id()));
        let seen = std::cell::RefCell::new(Vec::<Vec<String>>::new());
        let drawing = |_: &Config, args: &[String]| {
            seen.borrow_mut().push(args.to_vec());
            let out = &args[args.iter().position(|a| a == "--out").unwrap() + 1];
            std::fs::write(out, b"foobar").unwrap();
            Ok("wrote a still".into())
        };
        let init = serde_json::json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": { "name": "promo_init", "arguments": {
                "project": project.display().to_string(), "canvas": "1920x1080" } } });
        handle(&init, &config(), &drawing).unwrap();
        let upsert = serde_json::json!({ "jsonrpc": "2.0", "id": 8, "method": "tools/call",
            "params": { "name": "promo_upsert_layer", "arguments": {
                "project": project.display().to_string(), "kind": "caption",
                "captionText": "Hi", "startTime": 1.0, "duration": 4.0 } } });
        let answer = handle(&upsert, &config(), &drawing).unwrap();

        let content = answer
            .pointer("/result/content")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(content.len(), 2, "text plus the glance");
        assert_eq!(content[1]["mimeType"], "image/png");
        assert_eq!(
            content[1]["data"], "Zm9vYmFy",
            "the image block carries preview.png, base64"
        );
        let argv = seen.borrow().last().unwrap().clone();
        assert_eq!(argv[0], "still");
        let flag = |name: &str| argv[argv.iter().position(|a| a == name).unwrap() + 1].clone();
        assert_eq!(flag("--time"), "3", "the caption's midpoint, not t=0");
        assert_eq!(
            flag("--size"),
            "480x270",
            "canvas aspect at thumbnail scale"
        );
        assert!(
            flag("--out").ends_with("preview.png"),
            "the stable path a person can watch"
        );
        std::fs::remove_dir_all(&project).unwrap();
    }

    /// Issue #7: the wizard answered with text alone while every other
    /// authoring tool attached its glance, and `preview` on it was a no-op.
    /// It rides the same thumbnail now — the composition's midpoint, since
    /// the wizard touches every layer — and honours `preview: false`.
    #[test]
    fn the_wizard_answers_with_a_glance_too() {
        let root = std::env::temp_dir().join(format!("mcp-showglance-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let png: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x62, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        let slide = root.join("a.png");
        std::fs::write(&slide, png).unwrap();
        let seen = std::cell::RefCell::new(Vec::<Vec<String>>::new());
        let drawing = |_: &Config, args: &[String]| {
            seen.borrow_mut().push(args.to_vec());
            let out = &args[args.iter().position(|a| a == "--out").unwrap() + 1];
            std::fs::write(out, b"foobar").unwrap();
            Ok("wrote a still".into())
        };
        let project = root.join("Show.promo");
        let show = serde_json::json!({ "jsonrpc": "2.0", "id": 11, "method": "tools/call",
            "params": { "name": "promo_slideshow", "arguments": {
                "project": project.display().to_string(),
                "slides": [{ "file": slide.display().to_string(), "caption": "One" },
                           { "file": slide.display().to_string() }] } } });
        let answer = handle(&show, &config(), &drawing).unwrap();
        let content = answer
            .pointer("/result/content")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(content.len(), 2, "text plus the glance: {answer}");
        assert_eq!(content[1]["mimeType"], "image/png");
        let argv = seen.borrow().last().unwrap().clone();
        assert_eq!(argv[0], "still");
        assert!(
            root.join("Show Exports/preview.png").is_file(),
            "the glance beside the project"
        );
        assert!(!project.join("Exports").exists(), "and nothing inside it");
        // And the classic show carries its caption as a layer.
        let meta: Value =
            serde_json::from_str(&std::fs::read_to_string(project.join("metadata.json")).unwrap())
                .unwrap();
        let captions = meta["layers"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|l| l["kind"] == "caption")
            .count();
        assert_eq!(captions, 1, "the slide with words has a caption layer");

        let quiet = root.join("Quiet.promo");
        let off = serde_json::json!({ "jsonrpc": "2.0", "id": 12, "method": "tools/call",
            "params": { "name": "promo_slideshow", "arguments": {
                "project": quiet.display().to_string(), "preview": false,
                "slides": [{ "file": slide.display().to_string() }] } } });
        let answer = handle(&off, &config(), &never).unwrap();
        let content = answer
            .pointer("/result/content")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(content.len(), 1, "preview: false attaches nothing");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A scaffold that succeeded reports success: the preview failing —
    /// no CLI beside the server, a render error — degrades to a note,
    /// never to isError.
    #[test]
    fn a_failed_preview_never_fails_the_call_it_rides_on() {
        let project = std::env::temp_dir().join(format!("mcp-noglance-{}", std::process::id()));
        let broken = |_: &Config, _: &[String]| Err("no `promo` on PATH".to_string());
        let init = serde_json::json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call",
            "params": { "name": "promo_init", "arguments": {
                "project": project.display().to_string(), "canvas": "1920x1080" } } });
        let answer = handle(&init, &config(), &broken).unwrap();
        assert_eq!(
            answer.pointer("/result/isError"),
            None,
            "the init still succeeded"
        );
        let text = answer
            .pointer("/result/content/0/text")
            .and_then(Value::as_str)
            .unwrap();
        assert!(text.contains("initialized"), "{text}");
        assert!(text.contains("preview unavailable"), "{text}");

        let off = serde_json::json!({ "jsonrpc": "2.0", "id": 10, "method": "tools/call",
            "params": { "name": "promo_upsert_layer", "arguments": {
                "project": project.display().to_string(), "kind": "caption",
                "captionText": "Hi", "preview": false } } });
        let answer = handle(&off, &config(), &never).unwrap();
        let text = answer
            .pointer("/result/content/0/text")
            .and_then(Value::as_str)
            .unwrap();
        assert!(
            text.contains("upserted") && !text.contains("preview"),
            "preview:false never shells out at all: {text}"
        );
        std::fs::remove_dir_all(&project).unwrap();
    }

    /// promo_apply's contract is the editor's Command VOCABULARY: which
    /// commands exist and what each names, without the model's type graph
    /// riding along. A batch through the tool reaches what the scaffold
    /// cannot — here, a deletion on a caption-only project (no probing).
    #[test]
    fn apply_carries_the_command_schema_and_reaches_the_long_tail() {
        let tools = Value::Array(contract::tools(Host::Headless));
        let apply = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "promo_apply")
            .expect("promo_apply offered");
        let schema = &apply["inputSchema"];
        assert!(
            schema.get("$defs").is_none(),
            "the type graph does not ride along"
        );
        let text = schema.to_string();
        assert!(
            !text.contains("\"$ref\""),
            "and no reference is left dangling for a client to resolve"
        );
        for kind in [
            "deleteLayer",
            "moveLayer",
            "updateLayer",
            "patchResource",
            "upsertKeyframe",
            "cameraMove",
        ] {
            assert!(text.contains(kind), "descriptor schema lacks `{kind}`");
        }

        let project = std::env::temp_dir().join(format!("mcp-apply-{}", std::process::id()));
        let broken = |_: &Config, _: &[String]| Err("no CLI in this test".to_string());
        let call = |name: &str, args: Value| {
            handle(
                &serde_json::json!({ "jsonrpc": "2.0", "id": 11, "method": "tools/call",
                    "params": { "name": name, "arguments": args } }),
                &config(),
                &broken,
            )
            .unwrap()
        };
        call(
            "promo_init",
            serde_json::json!({
            "project": project.display().to_string(), "canvas": "1280x720", "preview": false }),
        );
        call(
            "promo_upsert_layer",
            serde_json::json!({
            "project": project.display().to_string(), "kind": "caption", "id": "gone",
            "captionText": "bye", "preview": false }),
        );
        let answer = call(
            "promo_apply",
            serde_json::json!({
            "project": project.display().to_string(), "preview": false,
            "commands": [{ "kind": "deleteLayer", "layerID": "gone" }] }),
        );
        assert_eq!(answer.pointer("/result/isError"), None, "{answer}");
        let text = std::fs::read_to_string(project.join("metadata.json")).unwrap();
        assert!(
            !text.contains("\"gone\""),
            "the layer is gone from the file"
        );
        std::fs::remove_dir_all(&project).unwrap();
    }

    /// REVIEW A2: "make a show from these pictures" is one tool call, and
    /// what it writes is a project the other tools can keep working on.
    #[test]
    fn the_wizard_builds_a_show_through_the_tool() {
        let base = std::env::temp_dir().join(format!("mcp-wizard-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        const PNG_1X1: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x62, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        let (a, b) = (base.join("a.png"), base.join("b.png"));
        std::fs::write(&a, PNG_1X1).unwrap();
        std::fs::write(&b, PNG_1X1).unwrap();
        let project = base.join("Show.promo");
        let broken = |_: &Config, _: &[String]| Err("no CLI in this test".to_string());
        let answer = handle(
            &serde_json::json!({ "jsonrpc": "2.0", "id": 12, "method": "tools/call",
                "params": { "name": "promo_slideshow", "arguments": {
                    "project": project.display().to_string(), "preview": false,
                    "slides": [
                        { "file": a.display().to_string(), "caption": "One" },
                        { "file": b.display().to_string() }
                    ] } } }),
            &config(),
            &broken,
        )
        .unwrap();
        assert_eq!(answer.pointer("/result/isError"), None, "{answer}");
        let text = std::fs::read_to_string(project.join("metadata.json")).unwrap();
        assert!(
            text.contains("\"minReaderVersion\":18"),
            "stamped like every tool-built file"
        );
        assert!(project.join("Resources/a.png").exists(), "media copied in");
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// Issue #2's sharpest paper cut: a missing CLI died with "No such
    /// file". The refusal must hand the operator the fix.
    #[test]
    fn a_missing_promo_cli_explains_how_to_get_one() {
        let broken = Config {
            log: None,
            promo: Some(PathBuf::from("/nonexistent/promo-cli-binary")),
            ..config()
        };
        let err = run_promo(&broken, &["schema".into()]).unwrap_err();
        for hint in ["--promo", "beside promoshot-mcp", "releases", "cargo build"] {
            assert!(err.contains(hint), "the error omits `{hint}`: {err}");
        }
    }

    #[test]
    fn an_unknown_method_with_an_id_is_a_jsonrpc_error() {
        let req = serde_json::json!({ "jsonrpc": "2.0", "id": 6, "method": "resources/list" });
        let answer = handle(&req, &config(), &never).unwrap();
        assert!(answer.get("error").is_some());
    }
}
