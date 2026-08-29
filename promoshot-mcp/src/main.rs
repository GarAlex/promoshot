//! An MCP server for rendering PromoShot projects with no app attached.
//!
//! Speaks Model Context Protocol over stdio — newline-delimited JSON-RPC on
//! stdin/stdout, logs on stderr — which is the transport agent clients spawn
//! themselves: no port, no token, no daemon. The Mac app's automation server
//! is the same seven-tool idea over HTTP for a running GUI; this binary is
//! the headless half, and it deliberately owns no rendering code. Every
//! render goes through the `promo` CLI, so there is exactly one command-line
//! contract to keep honest and the server can never disagree with it.
//!
//! The one answer served in-process is `promo_schema`: the format text lives
//! in `promo-model` and is compiled in, the same single source the CLI
//! prints.
//!
//! Configuration is three flags, everything else defaulted:
//!   --workspace <dir>   where promo_workspace points (else
//!                       $PROMOSHOT_WORKSPACE, else XDG data dir)
//!   --root <dir>        fence: refuse projects outside this tree
//!   --promo <path>      the CLI binary (else next to this executable,
//!                       else PATH)

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

const PROTOCOL_FALLBACK: &str = "2025-03-26";

fn main() {
    let config = match Config::from_args(std::env::args().skip(1)) {
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
}

impl Config {
    fn from_args(args: impl Iterator<Item = String>) -> Result<Config, String> {
        let mut workspace = None;
        let mut root = None;
        let mut promo = None;
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
                other => return Err(format!("unknown flag `{other}`")),
            }
        }
        Ok(Config {
            workspace: workspace.unwrap_or_else(default_workspace),
            root,
            promo,
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
/// to interpret them.
fn run_promo(config: &Config, args: &[String]) -> Result<String, String> {
    let output = std::process::Command::new(promo_binary(config))
        .args(args)
        .output()
        .map_err(|e| format!("could not run `promo`: {e}"))?;
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
        "tools/list" => Ok(json!({ "tools": tool_descriptors() })),
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
        }
    })
}

/// The tool surface, mirroring the Mac app's names so one skill drives both.
/// `promo_open` has no meaning headless and `promo_render_gif` waits on the
/// CLI growing a gif command; neither is offered rather than offered broken.
fn tool_descriptors() -> Value {
    let project = json!({ "type": "string", "description":
        "Path to the .promo project folder (metadata.json + Resources/)" });
    json!([
        {
            "name": "promo_validate",
            "description": "Decode a project with the renderer's own parser and report \
                everything it would silently correct. 'ok' means it will render.",
            "inputSchema": { "type": "object",
                "properties": { "project": project },
                "required": ["project"] }
        },
        {
            "name": "promo_inspect",
            "description": "Canvas, duration, layers by kind, undefined colours, and any \
                layer whose media is missing.",
            "inputSchema": { "type": "object",
                "properties": { "project": project },
                "required": ["project"] }
        },
        {
            "name": "promo_schema",
            "description": "The .promo format, from the same single file the engine \
                compiles in. Read it once before authoring.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "promo_render_still",
            "description": "Render one PNG at a moment. Returns the path written, never \
                the bytes.",
            "inputSchema": { "type": "object",
                "properties": {
                    "project": project,
                    "time": { "type": "number", "description": "Seconds (default 0)" },
                    "size": { "type": "string", "description": "WxH (default: canvas)" },
                    "out": { "type": "string", "description":
                        "Output file (default: <project>/Exports/still-<time>s.png)" }
                },
                "required": ["project"] }
        },
        {
            "name": "promo_render_frames",
            "description": "A PNG per frame over a range — the contact sheet that catches \
                a mis-aimed viewport before a full render.",
            "inputSchema": { "type": "object",
                "properties": {
                    "project": project,
                    "from": { "type": "number" },
                    "to": { "type": "number" },
                    "fps": { "type": "number" },
                    "size": { "type": "string", "description": "WxH (default: canvas)" },
                    "outDir": { "type": "string", "description":
                        "Output directory (default: <project>/Exports/frames)" }
                },
                "required": ["project"] }
        },
        {
            "name": "promo_render_video",
            "description": "Render the mp4, audio mixed — needs ffmpeg on PATH. Returns \
                the path written.",
            "inputSchema": { "type": "object",
                "properties": {
                    "project": project,
                    "fps": { "type": "number", "description":
                        "Default: the project's own, else 30" },
                    "size": { "type": "string", "description": "WxH (default: canvas)" },
                    "out": { "type": "string", "description":
                        "Output file (default: <project>/Exports/export.mp4)" }
                },
                "required": ["project"] }
        },
        {
            "name": "promo_workspace",
            "description": "The folder this machine keeps assistant-authored projects in. \
                Create new .promo folders here.",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ])
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
    match dispatch_tool(name, args, config, run) {
        Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
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
        "promo_schema" => Ok(promo_model::SCHEMA.to_string()),
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
            argv.extend(["--time".into(), time.to_string()]);
            push_size(&mut argv, args);
            run(config, &argv)
        }
        "promo_render_frames" => {
            let project = fenced_project(args, config)?;
            let out = default_out(args, "outDir", &project, "frames")?;
            let mut argv = vec!["frames".to_string(), project, "--out".into(), out];
            for (key, flag) in [("from", "--from"), ("to", "--to"), ("fps", "--fps")] {
                if let Some(v) = args.get(key).and_then(Value::as_f64) {
                    argv.extend([flag.to_string(), v.to_string()]);
                }
            }
            push_size(&mut argv, args);
            run(config, &argv)
        }
        "promo_render_video" => {
            let project = fenced_project(args, config)?;
            let out = default_out(args, "out", &project, "export.mp4")?;
            let mut argv = vec!["video".to_string(), project, "--out".into(), out];
            if let Some(fps) = args.get("fps").and_then(Value::as_f64) {
                argv.extend(["--fps".into(), fps.to_string()]);
            }
            push_size(&mut argv, args);
            run(config, &argv)
        }
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
    let path = std::fs::canonicalize(raw).map_err(|e| format!("project `{raw}`: {e}"))?;
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

/// An explicit output path wins; otherwise the project's Exports folder,
/// created on the way — the same default the app's own tools use.
fn default_out(args: &Value, key: &str, project: &str, filename: &str) -> Result<String, String> {
    if let Some(out) = args.get(key).and_then(Value::as_str) {
        return Ok(out.to_string());
    }
    let exports = Path::new(project).join("Exports");
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
                "promo_render_still",
                "promo_render_frames",
                "promo_render_video",
                "promo_workspace"
            ],
            "no promo_open headless, no gif until the CLI has one"
        );
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
        assert_eq!(
            out_path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str()),
            Some("Exports"),
            "defaulted into the project's Exports: {out}"
        );
        assert!(
            Path::new(&out).parent().unwrap().is_dir(),
            "and Exports exists"
        );
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

    #[test]
    fn an_unknown_method_with_an_id_is_a_jsonrpc_error() {
        let req = serde_json::json!({ "jsonrpc": "2.0", "id": 6, "method": "resources/list" });
        let answer = handle(&req, &config(), &never).unwrap();
        assert!(answer.get("error").is_some());
    }
}
