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
// The tool descriptors are one large `json!` literal; the macro recurses
// per nesting level, and the default limit is below what 19 tools need.
#![recursion_limit = "256"]

mod media;
mod preview;
mod speak;

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

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
/// Only `promo_open` stays app-side — putting a window in front of a person
/// has no headless meaning.
fn tool_descriptors() -> Value {
    let project = json!({ "type": "string", "description":
        "Path to the .promo project folder (metadata.json + Resources/)" });
    let preview = json!({ "type": "boolean", "description":
        "Attach an inline thumbnail of the composition (default true); the \
         same image lands at <project>/Exports/preview.png" });
    // The editor's Command enum as the `commands` item schema. Its $defs
    // are hoisted to the inputSchema root so "#/$defs/…" references
    // resolve from where a client resolves them.
    let mut command_items = promo_editor::command_schema();
    let command_defs = command_items
        .as_object_mut()
        .and_then(|m| {
            m.remove("$schema");
            m.remove("title");
            m.remove("$defs")
        })
        .unwrap_or_else(|| json!({}));
    json!([
        {
            "name": "promo_validate",
            "description": "Decode a project with the renderer's own parser and report \
                everything it would silently correct. 'ok' means it will render. A \
                mid-composition thumbnail comes attached — glance at it.",
            "inputSchema": { "type": "object",
                "properties": { "project": project, "preview": preview },
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
            "description": "The authoring subset of the .promo format plus four \
                complete, validated recipes. Read this once before authoring; \
                promo_schema_full is the whole format.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "promo_schema_types",
            "description": "The format as a JSON Schema, types only, GENERATED from the \
                parser's own structs — fill a structured object against this instead of \
                freehanding JSON; the prose lives in promo_schema.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "promo_schema_full",
            "description": "The whole .promo format, from the same single file the \
                engine compiles in — sprites, masks, motion paths, duration rules, \
                waits, gradients, palette roles and all.",
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
                    "proxy": { "type": "string", "enum": ["auto", "on", "off"], "description":
                        "auto (default) reads a built tier-1 proxy when the output fits it; on builds \
                         missing proxies first; off never reads one. A full-size render never does." },
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
                    "proxy": { "type": "string", "enum": ["auto", "on", "off"], "description":
                        "auto (default) reads a built tier-1 proxy when the output fits it; on builds \
                         missing proxies first; off never reads one. A full-size render never does." },
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
                    "proxy": { "type": "string", "enum": ["auto", "on", "off"], "description":
                        "auto (default) reads a built tier-1 proxy when the output fits it; on builds \
                         missing proxies first; off never reads one. A full-size render never does." },
                    "codec": { "type": "string", "enum": ["h264", "prores422", "prores4444"], "description":
                        "h264 in an mp4 (default); ProRes 422 HQ or 4444 want a .mov out path." },
                    "alpha": { "type": "boolean", "description":
                        "Render over nothing and keep the frames' alpha — ProRes 4444 in a .mov." },
                    "out": { "type": "string", "description":
                        "Output file (default: <project>/Exports/export.mp4)" }
                },
                "required": ["project"] }
        },
        {
            "name": "promo_render_gif",
            "description": "Render a looping GIF — the preview format, needing no ffmpeg. \
                Default 12fps. Returns the path written.",
            "inputSchema": { "type": "object",
                "properties": {
                    "project": project,
                    "fps": { "type": "number", "description": "Default 12" },
                    "size": { "type": "string", "description": "WxH (default: canvas)" },
                    "proxy": { "type": "string", "enum": ["auto", "on", "off"], "description":
                        "auto (default) reads a built tier-1 proxy when the output fits it; on builds \
                         missing proxies first; off never reads one. A full-size render never does." },
                    "out": { "type": "string", "description":
                        "Output file (default: <project>/Exports/export.gif)" }
                },
                "required": ["project"] }
        },
        {
            "name": "promo_proxy",
            "description": "Build tier-1 proxies (960 px long edge, every frame a keyframe) for every \
                video resource in a project, in the proxy cache outside the package. Stills, \
                frames and small renders then read them by default (proxy: auto) — what makes \
                an hour-long 4K source scrub and render like a short one.",
            "inputSchema": { "type": "object",
                "properties": { "project": { "type": "string" } },
                "required": ["project"] }
        },
        {
            "name": "promo_workspace",
            "description": "The folder this machine keeps assistant-authored projects in. \
                Create new .promo folders here.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "promo_media_probe",
            "description": "The facts of a source file before composing with it: container, \
                duration, streams — codec, size, fps, display rotation, channels. \
                Distilled JSON, not ffprobe's firehose.",
            "inputSchema": { "type": "object",
                "properties": { "file": { "type": "string" } },
                "required": ["file"] }
        },
        {
            "name": "promo_media_filmstrip",
            "description": "Eyes on the footage: N evenly spaced frames tiled into one \
                PNG contact sheet, sampled times returned so a cell maps to a moment. \
                Look at this before deciding what a clip shows.",
            "inputSchema": { "type": "object",
                "properties": {
                    "file": { "type": "string" },
                    "count": { "type": "number", "description": "Frames (default 12, max 48)" },
                    "out": { "type": "string", "description":
                        "Output PNG (default: the workspace folder)" }
                },
                "required": ["file"] }
        },
        {
            "name": "promo_media_silences",
            "description": "Ears on the footage: where the sound is NOT — silence spans \
                and their inverse, the sound spans an edit actually wants. Cuts and \
                captions land on these boundaries.",
            "inputSchema": { "type": "object",
                "properties": {
                    "file": { "type": "string" },
                    "thresholdDb": { "type": "number", "description": "Default -35" },
                    "minSeconds": { "type": "number", "description": "Default 0.35" }
                },
                "required": ["file"] }
        },
        {
            "name": "promo_media_scenes",
            "description": "Eyes for CUTS: per-frame scene-change scores distilled to \
                cut times and the shots between them — the footage-first answer when \
                a clip has no silence gaps to cut on. Scores are ffmpeg's scene \
                score (0..1, motion-suppressed); 0.4 catches hard cuts.",
            "inputSchema": { "type": "object",
                "properties": {
                    "file": { "type": "string" },
                    "threshold": { "type": "number", "description": "Default 0.4" }
                },
                "required": ["file"] }
        },
        {
            "name": "promo_transcribe",
            "description": "Ears for WORDS: a transcript with timings, the draft captions \
                are cut from. Headless this needs whisper.cpp's whisper-cli on PATH and \
                WHISPER_MODEL set; without them an agent cannot transcribe and the \
                refusal says so.",
            "inputSchema": { "type": "object",
                "properties": { "file": { "type": "string" } },
                "required": ["file"] }
        },
        {
            "name": "promo_init",
            "description": "Create a project folder: metadata.json boilerplate, canvas, \
                palette, a background layer, ids minted. The file it writes is ordinary \
                metadata.json — hand-edit it freely afterwards; the schema stays the \
                source of truth. Never overwrites. A thumbnail comes attached.",
            "inputSchema": { "type": "object",
                "properties": {
                    "project": project,
                    "preview": preview,
                    "canvas": { "type": "string", "description":
                        "\"1920x1080\" (or {width, height})" },
                    "palette": { "type": "object", "description":
                        "Named colours: {\"canvas\": \"10182B\", \"text\": \"F3F5FF\"} \
                         (or [{name, colorHex}]). \"canvas\" becomes the background." },
                    "id": { "type": "string", "description":
                        "Your own short project id; unnamed mints a UUID. The \
                         background layer is always \"bg\"." },
                    "name": { "type": "string" }
                },
                "required": ["project", "canvas"] }
        },
        {
            "name": "promo_upsert_layer",
            "description": "SCAFFOLD one layer — image, video or caption — with a \
                placement, a fadeIn, a device/border frame. Media is copied in, sizes \
                and durations probed, the composition re-stretched every call. Pass an \
                existing id to UPDATE: only the fields you pass change, placement \
                merges into the first keyframe, hand-added keyframes survive. This is \
                the scaffold, not the whole format: motion and viewport ride \
                promo_upsert_keyframe; transitions beyond fadeIn are ordinary JSON \
                edits — start from a promo_schema recipe. A thumbnail sampled at \
                the touched layer's midpoint comes attached — LOOK at it before \
                the next edit.",
            "inputSchema": { "type": "object",
                "properties": {
                    "project": project,
                    "preview": preview,
                    "kind": { "type": "string", "enum": ["image", "video", "caption"] },
                    "file": { "type": "string", "description":
                        "Image/video to copy in — required to create, optional on \
                         update (a repoint)" },
                    "fadeIn": { "type": "number", "description": "Seconds" },
                    "frame": { "type": "object", "description":
                        "Resource dressing: {kind: \"device\"|\"border\", material, \
                         tiltY, borderWidth, cornerRadius} — define @edge in the \
                         palette when you frame" },
                    "captionText": { "type": "string" },
                    "fontSize": { "type": "number", "description": "Caption points" },
                    "placement": { "type": "object", "description":
                        "{height|width|mode, anchor, offset} — media sizes too; a \
                         caption takes anchor and offset only" },
                    "startTime": { "type": "number" },
                    "duration": { "type": "number", "description":
                        "Seconds (default: a video's own length, else 3)" },
                    "id": { "type": "string", "description":
                        "An existing layer's id makes this an UPDATE; on create, \
                         your own short id (\"card\") is used verbatim" },
                    "resourceId": { "type": "string", "description":
                        "Your own short id for the created resource; unnamed \
                         mints a UUID" },
                    "name": { "type": "string" }
                },
                "required": ["project", "kind"] }
        },
        {
            "name": "promo_upsert_keyframe",
            "description": "MOTION in the format's own language: create or merge ONE \
                keyframe on a layer. A second placement keyframe is a push-in, \
                viewport keyframes are a Ken Burns ride, colorHex ramps a \
                background. Pass an existing keyframe id to UPDATE — only the \
                fields you pass change. Creating without transitionDuration ramps \
                from the previous keyframe (a stated 0 holds). Swaps, waits and \
                motion paths stay ordinary JSON edits.",
            "inputSchema": { "type": "object",
                "properties": {
                    "project": project,
                    "layer": { "type": "string", "description":
                        "The layer's id — promo_inspect lists them" },
                    "id": { "type": "string", "description":
                        "An existing keyframe's id makes this an UPDATE; on \
                         create, your own short id (\"k1\") is used verbatim" },
                    "time": { "type": "number", "description":
                        "Seconds, layer-local — required to create" },
                    "placement": { "type": "object", "description":
                        "{height|width|mode, anchor, offset} — a stored rule, \
                         re-resolved on every read" },
                    "viewport": { "type": "array", "description":
                        "[x, y, w, h] window onto the source, unit coordinates" },
                    "opacity": { "type": "number" },
                    "zoom": { "type": "number" },
                    "fontSize": { "type": "number", "description": "Caption points" },
                    "colorHex": { "type": "string", "description":
                        "Background layers only — ramps the colour" },
                    "tiltX": { "type": "number" },
                    "tiltY": { "type": "number" },
                    "easing": { "type": "string",
                        "enum": ["linear", "easeIn", "easeOut", "easeInOut"] },
                    "transitionDuration": { "type": "number", "description":
                        "Seconds of ramp INTO this keyframe" },
                    "preview": preview
                },
                "required": ["project", "layer"] }
        },
        {
            "name": "promo_apply",
            "description": "The whole vocabulary through one door: a batch of the editor's \
                own commands applied as ONE atomic step — delete, move, rename, enable, \
                retime; addLayer/addResource whole; updateLayer / patchResource / \
                patchSettings as JSON merge patches (only the fields you pass change; \
                null removes) — a wipe is {\"transitionIn\": {\"kind\": \"wipe\", \
                \"duration\": 0.5}}, a swap is upsertKeyframe with resourceID and a \
                transition, a trim is patchResource; setMarkers replaces the timeline's \
                markers and chapters whole. Every command succeeds or nothing is \
                written. The schema of `commands` IS the editor's Command enum.",
            "inputSchema": { "type": "object",
                "$defs": command_defs,
                "properties": {
                    "project": project,
                    "commands": { "type": "array", "minItems": 1, "items": command_items,
                        "description": "Commands in order; ids are the file's own \
                         (promo_inspect lists layers, resources by promo_schema_full)" },
                    "preview": preview
                },
                "required": ["project", "commands"] }
        },
        {
            "name": "promo_slideshow",
            "description": "The wizard, for agents: pictures and clips in, a complete show \
                out — the same arrangement the apps' wizard builds. kind classic (one \
                slide at a time, crossfade by default), carousel (cards fly in and \
                settle), or appStore (a store listing: your shots in one device frame \
                over a background, a headline per shot, the canvas the store's own \
                size). Creates the project folder and copies the media in; refine \
                with the other tools afterwards. Never overwrites.",
            "inputSchema": { "type": "object",
                "properties": {
                    "project": { "type": "string", "description":
                        "Folder to create, e.g. <workspace>/Show.promo" },
                    "name": { "type": "string" },
                    "kind": { "type": "string", "enum": ["classic", "carousel", "appStore"],
                        "description": "Default classic" },
                    "transition": { "type": "string",
                        "enum": ["none", "crossfade", "wipe", "slide", "push", "scale"],
                        "description": "Default crossfade" },
                    "transitionEdge": { "type": "string",
                        "enum": ["left", "right", "top", "bottom"] },
                    "direction": { "type": "string",
                        "enum": ["rightToLeft", "leftToRight"], "description": "Carousel" },
                    "sizing": { "type": "string", "enum": ["fit", "fill"] },
                    "device": { "type": "string", "enum": ["iPhone", "iPad", "mac"],
                        "description": "appStore: the frame and the store's canvas" },
                    "framing": { "type": "string", "enum": ["flat", "angled"] },
                    "canvas": { "type": "string", "description":
                        "\"1920x1080\" (ignored for appStore — the store decides)" },
                    "backgroundColorHex": { "type": "string" },
                    "slides": { "type": "array", "minItems": 1, "items": {
                        "type": "object",
                        "properties": {
                            "file": { "type": "string", "description": "Image or clip to copy in" },
                            "caption": { "type": "string", "description":
                                "Words over the slide — a caption layer that lives and arrives with \
                                 its picture: the headline band for appStore, a lower third otherwise" },
                            "duration": { "type": "number", "description":
                                "Seconds on screen (default 3; a clip's own length)" },
                            "transitionDuration": { "type": "number", "description":
                                "How long the NEXT slide takes to arrive (default 0.5)" },
                            "looped": { "type": "boolean" },
                            "displayName": { "type": "string" }
                        },
                        "required": ["file"] } },
                    "preview": preview
                },
                "required": ["project", "slides"] }
        },
        {
            "name": "promo_explain",
            "description": "The agent's debugger: why is this layer where it is — the \
                renderer's OWN numbers at a moment. Per layer: visible and why not, the \
                resource shown (swap-aware), the resolved transform and the rect on the \
                canvas in pixels, opacity, rotation, tilt, viewport, gain, the keyframes \
                bracketing the moment, transitions and fades; per project: timing \
                problems and validate's warnings. Defaults to the composition's midpoint.",
            "inputSchema": { "type": "object",
                "properties": {
                    "project": project,
                    "time": { "type": "number", "description": "Seconds (default: midpoint)" },
                    "layer": { "type": "string", "description": "One layer's id; absent = all" }
                },
                "required": ["project"] }
        },
        {
            "name": "promo_diff",
            "description": "What changed since you last looked, in the format's own terms: \
                two projects compared by entity — settings by key, resources and layers \
                by id, keyframes by id — as lines you can act on. Copy metadata.json \
                aside before a person's turn, then diff against the copy.",
            "inputSchema": { "type": "object",
                "properties": {
                    "project": project,
                    "against": { "type": "string", "description":
                        "The other project folder (or its metadata.json)" }
                },
                "required": ["project", "against"] }
        },
        {
            "name": "promo_voices",
            "description": "A narration provider's voices — id, name and a line of detail per \
                voice (openai's fixed roster; elevenlabs and google list live) — with the \
                person's own key: the OS keyring (`promoshot-mcp key set <provider>`), else a \
                secrets file (OPENAI_API_KEY_FILE, or /run/secrets/OPENAI_API_KEY) where \
                there is no keyring. Use before promo_speak to pick a voiceID.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "provider": { "type": "string", "enum": ["openai", "elevenlabs", "google"],
                        "description": "Default openai" }
                }
            }
        },
        {
            "name": "promo_speak",
            "description": "Synthesize narration for every resource whose speech.text \
                says something, spending the PERSON'S OWN provider key from the \
                OS keyring (`promoshot-mcp key set <provider>`) or, where there is none, a \
                secrets file (OPENAI_API_KEY_FILE or /run/secrets/OPENAI_API_KEY), \
                matching each script's provider (default openai/alloy). Unchanged \
                text is reused by receipt, never billed twice. Keys are checked for EVERY pending narration before \
                anything is bought, and each bought receipt is written back at once. Without a key an \
                agent CANNOT narrate — record a voice file into Resources/ and \
                reference it as an ordinary audio resource instead.",
            "inputSchema": { "type": "object",
                "properties": { "project": project,
                    "check": { "type": "boolean", "description":
                        "Spend nothing: report where each needed provider's key comes from \
                         (never the key) and what a real call would synthesize — ready, blocked, \
                         or nothing to do. With no project, the keys alone." } },
                "required": [] }
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
        "promo_media_probe" => media::probe(args),
        "promo_media_filmstrip" => media::filmstrip(args, &config.workspace),
        "promo_media_silences" => media::silences(args),
        "promo_media_scenes" => media::scenes(args),
        "promo_transcribe" => media::transcribe(args),
        "promo_explain" => promo_author::explain(args, config.root.as_deref()),
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
        "promo_schema_full" => Ok(promo_model::SCHEMA.to_string()),
        "promo_schema_types" => {
            serde_json::to_string_pretty(&promo_model::wire_schema()).map_err(|e| e.to_string())
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
            let mut argv = vec!["frames".to_string(), project, "--out".into(), out];
            if let Some(policy) = args.get("proxy").and_then(Value::as_str) {
                argv.extend(["--proxy".into(), policy.to_string()]);
            }
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
                "promo_schema_types",
                "promo_schema_full",
                "promo_render_still",
                "promo_render_frames",
                "promo_render_video",
                "promo_render_gif",
                "promo_proxy",
                "promo_workspace",
                "promo_media_probe",
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

    /// The shipped skill (skill/SKILL.md) is the workflow layer over this
    /// server, and a skill that names tools the server does not offer — or
    /// misses ones it does — teaches wrongly. Held here, where the tool
    /// list lives, the same discipline as the app's SkillDriftTests.
    #[test]
    fn the_skill_teaches_exactly_the_tools_the_server_offers() {
        let skill = include_str!("../../skill/SKILL.md");
        assert!(skill.starts_with("---\n"), "front matter, so it installs");
        let tools = tool_descriptors();
        for tool in tools.as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            assert!(skill.contains(name), "the skill never mentions `{name}`");
        }
        assert!(
            skill.contains("minReaderVersion\": 19"),
            "the stamp the skill teaches must be the current one"
        );
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
        assert!(project.join("Exports/preview.png").is_file());
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

    /// promo_apply's contract is the editor's Command enum: the descriptor
    /// carries the generated schema with its $defs hoisted so references
    /// resolve, and a batch through the tool reaches what the scaffold
    /// cannot — here, a deletion on a caption-only project (no probing).
    #[test]
    fn apply_carries_the_command_schema_and_reaches_the_long_tail() {
        let tools = tool_descriptors();
        let apply = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "promo_apply")
            .expect("promo_apply offered");
        let schema = &apply["inputSchema"];
        assert!(schema["$defs"].is_object(), "defs hoisted to the root");
        let text = schema.to_string();
        for kind in [
            "deleteLayer",
            "moveLayer",
            "updateLayer",
            "patchResource",
            "upsertKeyframe",
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
