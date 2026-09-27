//! The MCP tool contract both servers serve (review 2026-09-27, P2-32).
//!
//! The headless server and the Mac app's server expose one surface to one
//! skill, and they were two hand-kept copies of it: 26 tools here and 28
//! there, a bare `promo_render_frames` sampling 12 moments headless and
//! every half second across the piece in the app, arguments the skill
//! teaches (`topics`, `tracking`, `codec`) that the app neither advertised
//! nor read, a turntable the app's skill copy promised and the app lacked.
//! Now every descriptor lives HERE — names, descriptions, argument
//! schemas, annotations — built per [`Host`], and the few places the hosts
//! differ on purpose are one table, [`HOST_DIFFERENCES`], that a test holds
//! the two lists to. The app serves [`tools`] over the C ABI; so a
//! description is written once, and an argument is advertised only where a
//! handler reads it — [`check_arguments`] refuses anything else on both
//! servers, where a misspelt argument used to be ignored in silence.

use serde_json::{json, Value};

/// The server a contract is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    /// `promoshot-mcp`: the renderer over the promo CLI, no window.
    Headless,
    /// The Mac app's server: a person at the machine, the app's own
    /// renderer and library.
    App,
}

impl Host {
    /// `"headless"` or `"app"`.
    pub fn parse(name: &str) -> Option<Host> {
        match name {
            "headless" => Some(Host::Headless),
            "app" => Some(Host::App),
            _ => None,
        }
    }
}

/// Moments a bare `promo_render_frames` renders.
pub const SAMPLE_FRAMES: usize = 12;
/// The most moments one `promo_render_frames` call renders; the refusal
/// names the way out.
pub const FRAME_CAP: usize = 240;

/// What differs between the hosts' contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Aspect {
    /// The tool exists on one host only.
    Tool,
    /// The tool takes this argument on one host only.
    Argument(&'static str),
    /// This annotation hint differs.
    Annotation(&'static str),
}

/// One place the two servers differ ON PURPOSE.
#[derive(Debug, Clone, Copy)]
pub struct Difference {
    pub tool: &'static str,
    pub aspect: Aspect,
    /// Where the tool or argument exists; for an annotation, the host
    /// whose hint is `true`.
    pub only: Host,
    pub why: &'static str,
}

/// Every intended difference between the hosts' tool lists. Anything else
/// the two lists disagree on is drift, and the contract's test fails on it.
pub const HOST_DIFFERENCES: &[Difference] = &[
    Difference {
        tool: "promo_open",
        aspect: Aspect::Tool,
        only: Host::App,
        why: "puts the project in front of a person; headless has no window",
    },
    Difference {
        tool: "promo_context",
        aspect: Aspect::Tool,
        only: Host::App,
        why: "what the person at the app is looking at",
    },
    Difference {
        tool: "promo_list_projects",
        aspect: Aspect::Tool,
        only: Host::App,
        why: "the app's own library",
    },
    Difference {
        tool: "promo_render_still",
        aspect: Aspect::Argument("proxy"),
        only: Host::Headless,
        why: "the app's renders pick their own proxy tier",
    },
    Difference {
        tool: "promo_render_frames",
        aspect: Aspect::Argument("proxy"),
        only: Host::Headless,
        why: "the app's renders pick their own proxy tier",
    },
    Difference {
        tool: "promo_render_video",
        aspect: Aspect::Argument("proxy"),
        only: Host::Headless,
        why: "the app's renders pick their own proxy tier",
    },
    Difference {
        tool: "promo_render_gif",
        aspect: Aspect::Argument("proxy"),
        only: Host::Headless,
        why: "the app's renders pick their own proxy tier",
    },
    Difference {
        tool: "promo_render_video",
        aspect: Aspect::Argument("size"),
        only: Host::Headless,
        why: "the app writes video at the project's export size, as its Export tab does",
    },
    Difference {
        tool: "promo_workspace",
        aspect: Aspect::Annotation("readOnlyHint"),
        only: Host::App,
        why: "headless creates the workspace folder on first ask; the app's is chosen in Settings",
    },
];

/// What a client is told at the handshake. The skill teaches this
/// properly, but a registry or Docker install hands a client the tools and
/// nothing else — the loop is the one thing a server can say for itself.
pub fn instructions(host: Host) -> String {
    let opening = match host {
        Host::Headless => {
            "PromoShot renders .promo projects — App Store shots, promo reels, product videos — headlessly."
        }
        Host::App => {
            "PromoShot renders .promo projects — App Store shots, promo reels, product videos."
        }
    };
    let mut text = format!(
        "{opening}

A project is a FOLDER named <Name>.promo holding `metadata.json` (the composition) and `Resources/` (the media, referenced by filename). You write metadata.json yourself; these tools scaffold it, check it and turn it into pixels.

The loop:
1. `promo_schema` once — the format's authority, with complete recipes. `promo_schema_full` when you need a feature it does not cover.
2. `promo_workspace` — where new projects may be created on this machine.
3. Write `metadata.json`. Ids are strings, unique in the file; short names are fine (the app keeps them as `handles` when it mints UUIDs). Sizes and positions are in canvas pixels; prefer a `placement` rule over raw shifts.
4. `promo_validate` — the renderer's own parser. The first word is the verdict: `NOT OK` lists what will not render or has no effect — fix those first; `ok` means it renders as written.
5. `promo_render_frames` — LOOK. It samples the piece and answers with one contact sheet as an image. Fix what you see, then `promo_render_video`.

Renders land BESIDE the project, in `<Name> Exports/`, and return paths, never bytes."
    );
    if host == Host::App {
        text.push_str(
            "\n\nA person is at this machine: `promo_open` puts the project in front of them, \
             `promo_context` says what they are looking at, and a write into a project they \
             have open lands in their editor with their undo history.",
        );
    }
    text
}

/// The tools a host serves, each with its annotations — what `tools/list`
/// answers.
pub fn tools(host: Host) -> Vec<Value> {
    let app = host == Host::App;
    let project = json!({ "type": "string", "description":
        "Path to the .promo project folder (metadata.json + Resources/)" });
    let preview = json!({ "type": "boolean", "description":
        "Attach an inline thumbnail of the composition (default true); the \
         same image lands at <Name> Exports/preview.png beside the project" });
    let proxy = json!({ "type": "string", "enum": ["auto", "on", "off"], "description":
        "auto (default) reads a built tier-1 proxy when the output fits it; on builds \
         missing proxies first; off never reads one. A full-size render never does." });
    let size = json!({ "type": "string", "description":
        "WxH, keeping the canvas's aspect (default: the canvas)" });
    // In an open project the app's server writes through the editor, so
    // the person's undo history holds each change.
    let undoable = if app {
        " In a project open in the app, the change lands in the editor as one undoable step."
    } else {
        ""
    };

    let mut list = vec![
        json!({
            "name": "promo_validate",
            "description": "Decode a project with the renderer's own parser and say whether \
                it renders as written. NOT OK leads with what will not render or has no \
                effect — a field nothing reads (with the name that was meant), a value that \
                does nothing where it is, missing media; ok means it renders, and any \
                warnings name what the renderer adjusts. Validating costs nothing — do it \
                before asking for a render. A mid-composition thumbnail comes attached — \
                glance at it.",
            "inputSchema": { "type": "object",
                "properties": { "project": project, "preview": preview },
                "required": ["project"] }
        }),
        json!({
            "name": "promo_inspect",
            "description": "What a project contains — canvas, duration, layers by kind, \
                resources — and any layer that cannot render, with the reason. Confirm a \
                composition matches what you meant before rendering it.",
            "inputSchema": { "type": "object",
                "properties": { "project": project },
                "required": ["project"] }
        }),
        json!({
            "name": "promo_schema",
            "description": "The authoring subset of the .promo format plus four \
                complete, validated recipes. Read this once before authoring; \
                promo_schema_full is the whole format.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "promo_schema_types",
            "description": "The format as a JSON Schema, types only, GENERATED from the \
                parser's own structs — fill a structured object against this instead of \
                freehanding JSON; the prose lives in promo_schema. 80 KB whole: pass \
                `types` for just the definitions you need.",
            "inputSchema": { "type": "object",
                "properties": {
                    "types": { "type": "array", "items": { "type": "string" }, "description":
                        "Type names — ProjectLayer, ProjectLayerKeyframe, CaptionStyle, … — \
                         for just those definitions and the names they reference; omit \
                         for the whole schema" }
                },
                "required": [] }
        }),
        json!({
            "name": "promo_schema_full",
            "description": "The whole .promo format, from the same single file the \
                engine compiles in — sprites, masks, motion paths, duration rules, \
                waits, gradients, palette roles and all. 73 KB whole: pass `topics` \
                to take only what this piece needs, no section over 5 KB.",
            "inputSchema": { "type": "object",
                "properties": {
                    "topics": { "type": "array", "items": { "type": "string" },
                        "description":
                            "\"core\" for the essentials, and any word a section's heading \
                             names: keyframe, placement, timing, transition, swap, motion, \
                             viewport, caption, tracking, reveal, background, palette, \
                             frame, look, mask, rect, media — and the features: \
                             composition, markers, audio, chroma, pointer, effects, lut, \
                             model, particles, route, morph, parts, recipe, environment, \
                             stage. Omit for everything." }
                },
                "required": [] }
        }),
        json!({
            "name": "promo_render_still",
            "description": "Render one PNG at a moment and return the path written, never \
                the bytes — SEE the composition at one time before rendering the whole. \
                Writes beside the project, in <Name> Exports/, unless `out` names a path.",
            "inputSchema": { "type": "object",
                "properties": {
                    "project": project,
                    "time": { "type": "number", "description": "Seconds (default 0)" },
                    "size": size,
                    "proxy": proxy,
                    "out": { "type": "string", "description":
                        "Output file (default: <Name> Exports/still-<time>s.png beside the project)" }
                },
                "required": ["project"] }
        }),
        json!({
            "name": "promo_render_frames",
            "description": format!("LOOK at the composition: moments rendered to PNGs and tiled \
                into one contact sheet, attached to this reply as an image. Bare, it samples \
                {SAMPLE_FRAMES} moments across the whole piece — the fastest way to catch an \
                off-centre card, an empty frame or a mis-aimed viewport. Name `times` for exact \
                moments, `sample` for a count spread over a from/to range, or a from/to/fps \
                range for every frame in it ({FRAME_CAP} at most). Frames from this tool's own \
                previous call in the same folder are replaced, never mixed."),
            "inputSchema": { "type": "object",
                "properties": {
                    "project": project,
                    "times": { "type": "array", "items": { "type": "number" }, "description":
                        "Exact seconds to render, inside the composition — rendered in time order" },
                    "sample": { "type": "integer", "description": format!(
                        "How many moments to spread across the range, both ends included \
                         (default {SAMPLE_FRAMES} when neither times nor a range is given)") },
                    "from": { "type": "number", "description": "Range start (default 0)" },
                    "to": { "type": "number", "description": "Range end (default: the end)" },
                    "fps": { "type": "number", "description":
                        "Every frame at this rate over the range — an export, not a look \
                         (default: the project's rate when a range is given without `sample`)" },
                    "size": size,
                    "proxy": proxy,
                    "outDir": { "type": "string", "description":
                        "Output directory (default: <Name> Exports/frames beside the project)" },
                    "preview": { "type": "boolean", "description":
                        "Attach the contact sheet to the reply (default true)" }
                },
                "required": ["project"] }
        }),
        json!({
            "name": "promo_render_video",
            "description": if app {
                "Render the whole composition, audio mixed — the file the app's Export tab \
                 makes, at the project's export size. H.264/HEVC in an mp4 by default, ProRes \
                 for an edit-ready master. On the free tier the output carries the PromoShot \
                 watermark, exactly as it does in the app. Returns the path written."
            } else {
                "Render the whole composition, audio mixed — needs ffmpeg on PATH. H.264 in an \
                 mp4 at the project's export size by default, ProRes for an edit-ready master. \
                 Returns the path written."
            },
            "inputSchema": { "type": "object",
                "properties": {
                    "project": project,
                    "fps": { "type": "number", "description":
                        "Default: the project's own, else 30" },
                    "size": { "type": "string", "description":
                        "WxH (default: the project's export size, else the canvas)" },
                    "proxy": proxy,
                    "codec": { "type": "string", "enum": ["h264", "prores422", "prores4444"],
                        "description": if app {
                            "h264: the app's automatic H.264/HEVC mp4 (default); ProRes 422 HQ \
                             or 4444 write a .mov"
                        } else {
                            "h264 in an mp4 (default); ProRes 422 HQ or 4444 want a .mov out path"
                        } },
                    "alpha": { "type": "boolean", "description":
                        "Render over nothing and keep the frames' alpha — ProRes 4444 in a .mov." },
                    "out": { "type": "string", "description":
                        "Output file (default: <Name> Exports/ beside the project)" }
                },
                "required": ["project"] }
        }),
        json!({
            "name": "promo_render_gif",
            "description": "Render a looping GIF — cheaper to look at than an mp4, and the \
                format for a README or a chat message. Returns the path written.",
            "inputSchema": { "type": "object",
                "properties": {
                    "project": project,
                    "fps": { "type": "number", "description":
                        "Default: the project's GIF rate (gifExportFps, 10 unless set)" },
                    "size": size,
                    "proxy": proxy,
                    "out": { "type": "string", "description":
                        "Output file (default: <Name> Exports/ beside the project)" }
                },
                "required": ["project"] }
        }),
        json!({
            "name": "promo_proxy",
            "description": if app {
                "Build tier-1 proxies (960 px long edge, every frame a keyframe) for every video \
                 resource in a project — the proxies the editor scrubs with, in the proxy cache \
                 outside the package. Headless renders read them for stills, frames and small \
                 renders; the app's own renders pick their tiers themselves."
            } else {
                "Build tier-1 proxies (960 px long edge, every frame a keyframe) for every video \
                 resource in a project, in the proxy cache outside the package. Stills, frames \
                 and small renders then read them by default (proxy: auto) — what makes an \
                 hour-long 4K source scrub and render like a short one."
            },
            "inputSchema": { "type": "object",
                "properties": { "project": project },
                "required": ["project"] }
        }),
        json!({
            "name": "promo_workspace",
            "description": if app {
                "The folder this Mac keeps assistant-authored projects in, and the answer to \
                 \"where may I write?\". Everything inside it is already approved, so a project \
                 you create there needs no further permission. Call it before writing a new \
                 project rather than inventing a path and being refused."
            } else {
                "The folder this machine keeps assistant-authored projects in, created if it \
                 is missing. Create new .promo folders here."
            },
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "promo_media_probe",
            "description": "The facts of one source file or SEVERAL, before composing with \
                them: container, duration, streams — codec, size, fps, display rotation, \
                channels — as distilled JSON; on a .glb, its material slots, clips and \
                bounds. Each answer carries the `resource` entry that file becomes, ready \
                to paste into `resources` with its pixel size already measured — which is \
                what a `placement` rule needs to resolve against anything other than a \
                square.",
            "inputSchema": { "type": "object",
                "properties": {
                    "file": { "type": "string", "description": "One path" },
                    "files": { "type": "array", "items": { "type": "string" }, "description":
                        "Several paths at once; the answer is keyed by the path asked for" }
                },
                "required": [] }
        }),
        json!({
            "name": "promo_media_turntable",
            "description": "Eyes on a model: a .glb rendered from N yaws round it, tiled into \
                one PNG contact sheet with each cell's yaw. Look at this before choosing a \
                camera for a model layer. (`promo_media_probe` on a .glb names its material \
                slots, clips and bounds.)",
            "inputSchema": { "type": "object",
                "properties": {
                    "file": { "type": "string", "description": "The .glb" },
                    "count": { "type": "integer", "description": "Yaws round it (default 6, at most 64)" },
                    "size": { "type": "integer", "description": "Cell size in px (default 320)" },
                    "out": { "type": "string", "description":
                        "Output PNG (default: the workspace folder)" }
                },
                "required": ["file"] }
        }),
        json!({
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
        }),
        json!({
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
        }),
        json!({
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
        }),
        json!({
            "name": "promo_transcribe",
            "description": if app {
                "Ears for WORDS: a transcript with timings, the draft captions are cut from — \
                 Apple's speech recognizer (the person allows Speech Recognition once). Cues: \
                 start, end, text."
            } else {
                "Ears for WORDS: a transcript with timings, the draft captions are cut from. \
                 Headless this needs whisper.cpp's whisper-cli on PATH and WHISPER_MODEL set; \
                 without them an agent cannot transcribe and the refusal says so."
            },
            "inputSchema": { "type": "object",
                "properties": { "file": { "type": "string" } },
                "required": ["file"] }
        }),
        json!({
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
        }),
        json!({
            "name": "promo_upsert_layer",
            "description": format!("SCAFFOLD one layer — image, video or caption — with a \
                placement, a fadeIn, a device/border frame. Media is copied in, sizes \
                and durations probed, the composition re-stretched every call. Pass an \
                existing id to UPDATE: only the fields you pass change, placement \
                merges into the first keyframe, hand-added keyframes survive. This is \
                the scaffold, not the whole format: motion and viewport ride \
                promo_upsert_keyframe; transitions beyond fadeIn, swaps, waits, \
                deletes and reorders are promo_apply commands.{undoable} A thumbnail \
                sampled at the touched layer's midpoint comes attached — LOOK at it \
                before the next edit."),
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
                    "tracking": { "type": "number", "description":
                        "Letter spacing in points — open a small eyebrow out (+6), \
                         tighten a big headline (-1.4)" },
                    "weight": { "type": "string", "description":
                        "The face's weight; heavier than bold is where a store \
                         headline lives",
                        "enum": ["ultraLight", "thin", "light", "regular", "medium",
                                 "semibold", "bold", "heavy", "black"] },
                    "lineHeight": { "type": "number", "description":
                        "Line spacing as a multiple of font size (default 1.25); \
                         ~1.05 for a stacked headline" },
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
        }),
        json!({
            "name": "promo_upsert_keyframe",
            "description": format!("MOTION in the format's own language: create or merge ONE \
                keyframe on a layer. A second placement keyframe is a push-in, \
                viewport keyframes are a Ken Burns ride, colorHex ramps a \
                background. Pass an existing keyframe id to UPDATE — only the \
                fields you pass change. Creating without transitionDuration ramps \
                from the previous keyframe (a stated 0 holds). Swaps, waits and \
                motion paths: promo_apply's upsertKeyframe carries any keyframe \
                field.{undoable}"),
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
                        "enum": ["linear", "easeIn", "easeOut", "easeInOut", "smooth"] },
                    "transitionDuration": { "type": "number", "description":
                        "Seconds of ramp INTO this keyframe" },
                    "preview": preview
                },
                "required": ["project", "layer"] }
        }),
        json!({
            "name": "promo_apply",
            "description": format!("The whole vocabulary through one door: a batch of the \
                editor's own commands applied as ONE atomic step — delete, move, rename, \
                enable, retime; addLayer/addResource whole; updateLayer / patchResource / \
                patchSettings as JSON merge patches (only the fields you pass change; \
                null removes) — a wipe is {{\"transitionIn\": {{\"kind\": \"wipe\", \
                \"duration\": 0.5}}}}, a swap is upsertKeyframe with resourceID and a \
                transition, a trim is patchResource; setMarkers replaces the timeline's \
                markers and chapters whole. Every command succeeds or nothing is \
                written.{undoable} The schema of `commands` IS the editor's Command enum."),
            "inputSchema": { "type": "object",
                "properties": {
                    "project": project,
                    "commands": { "type": "array", "minItems": 1, "items": command_items(),
                        "description": "Commands in order; ids are the file's own \
                         (promo_inspect lists layers, resources by promo_schema_full)" },
                    "preview": preview
                },
                "required": ["project", "commands"] }
        }),
        json!({
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
        }),
        json!({
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
        }),
        json!({
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
        }),
        json!({
            "name": "promo_voices",
            "description": if app {
                "A narration provider's voices as provider:voice ids, each with a line of \
                 detail — openai's fixed roster; elevenlabs and google list live with the key \
                 the person added in Settings → Narration. Call it before writing any speech \
                 resource: the ids are provider-specific and cannot be guessed."
            } else {
                "A narration provider's voices — id, name and a line of detail per voice \
                 (openai's fixed roster; elevenlabs and google list live) — with the person's \
                 own key: the OS keyring (`promoshot-mcp key set <provider>`), else a secrets \
                 file (OPENAI_API_KEY_FILE, or /run/secrets/OPENAI_API_KEY) where there is no \
                 keyring. Use before promo_speak to pick a voiceID."
            },
            "inputSchema": {
                "type": "object",
                "properties": {
                    "provider": { "type": "string", "enum": ["openai", "elevenlabs", "google"],
                        "description": if app { "Default: the provider chosen in Settings" }
                            else { "Default openai" } }
                }
            }
        }),
        json!({
            "name": "promo_speak",
            "description": if app {
                "Synthesize every speech resource in a project that has text but no current \
                 audio, and write each one's DURATION back into metadata.json — call it after \
                 writing the text and BEFORE laying out layers. Text that has not changed is \
                 never re-synthesized. This spends the person's own API credit, with the key \
                 they added in Settings → Narration — ask with `check: true` first: it spends \
                 nothing and says which keys are present (never the keys) and what a real call \
                 would synthesize."
            } else {
                "Synthesize narration for every resource whose speech.text says something, \
                 spending the PERSON'S OWN provider key from the OS keyring (`promoshot-mcp key \
                 set <provider>`) or, where there is none, a secrets file (OPENAI_API_KEY_FILE \
                 or /run/secrets/OPENAI_API_KEY), matching each script's provider (default \
                 openai/alloy). Unchanged text is reused by receipt, never billed twice. Keys \
                 are checked for EVERY pending narration before anything is bought, and each \
                 bought receipt is written back at once. Without a key an agent CANNOT narrate \
                 — record a voice file into Resources/ and reference it as an ordinary audio \
                 resource instead."
            },
            "inputSchema": { "type": "object",
                "properties": { "project": project,
                    "check": { "type": "boolean", "description":
                        "Spend nothing: report where each needed provider's key comes from \
                         (never the key) and what a real call would synthesize — ready, blocked, \
                         or nothing to do. With no project, the keys alone." } },
                "required": [] }
        }),
    ];
    if app {
        list.push(json!({
            "name": "promo_list_projects",
            "description": "The projects in the app's library, with their folders.",
            "inputSchema": { "type": "object", "properties": {} }
        }));
        list.push(json!({
            "name": "promo_context",
            "description": "The person's gaze: which projects are open (and unsaved), which \
                one is in front, the selected layer (by the id the FILE spells), the \
                playhead, the open section, their standing note to you, and whether \
                Ask-before-applying is on. Read this before \"make this one bigger\". \
                Subscribe to the project resource to be told when they save.",
            "inputSchema": { "type": "object", "properties": {} }
        }));
        list.push(json!({
            "name": "promo_open",
            "description": "Open a project folder in PromoShot so a person can see it. Use \
                this after authoring, so the work lands somewhere visible rather than only \
                on disk.",
            "inputSchema": { "type": "object",
                "properties": { "project": project },
                "required": ["project"] }
        }));
    }
    // The arguments a host does not take come out here, from the one
    // table — so the table cannot say one thing and the lists another.
    for difference in HOST_DIFFERENCES {
        if difference.only == host {
            continue;
        }
        match difference.aspect {
            Aspect::Tool => list.retain(|t| t["name"] != difference.tool),
            Aspect::Argument(argument) => {
                for tool in list.iter_mut().filter(|t| t["name"] == difference.tool) {
                    if let Some(properties) = tool
                        .pointer_mut("/inputSchema/properties")
                        .and_then(Value::as_object_mut)
                    {
                        properties.remove(argument);
                    }
                }
            }
            Aspect::Annotation(_) => {}
        }
    }
    for tool in &mut list {
        let name = tool["name"].as_str().unwrap_or_default().to_string();
        tool["annotations"] = annotations(host, &name);
    }
    list
}

/// What a tool does to the world, in the protocol's own vocabulary.
///
/// A host that gates on `readOnlyHint` — a planning mode, an approval
/// prompt — has to assume the worst without these, so asking for the
/// format's own schema looked exactly like asking to render a video over
/// someone's file. Read-only means it writes nothing a person would miss:
/// a probe's scratch file in the system temp directory does not count, a
/// PNG in the project's Exports does.
pub fn annotations(host: Host, name: &str) -> Value {
    // (title, read only, destructive, idempotent, reaches the network)
    let (title, read_only, destructive, idempotent, open_world) = match name {
        "promo_schema" => ("The format, in brief", true, false, true, false),
        "promo_schema_types" => ("The format's machine schema", true, false, true, false),
        "promo_schema_full" => ("The format, in full", true, false, true, false),
        "promo_validate" => ("Check a project", false, false, true, false),
        "promo_inspect" => ("What is in a project", true, false, true, false),
        "promo_explain" => ("Why a layer looks like that", true, false, true, false),
        "promo_diff" => ("What changed since a copy", true, false, true, false),
        "promo_workspace" => (
            "Where new projects go",
            host == Host::App,
            false,
            true,
            false,
        ),
        "promo_media_probe" => ("Facts about a media file", true, false, true, false),
        "promo_media_silences" => ("Where a recording goes quiet", true, false, true, false),
        "promo_media_scenes" => ("Where a clip cuts", true, false, true, false),
        "promo_transcribe" => ("Words from a recording", true, false, true, false),
        "promo_voices" => ("A provider's voices", true, false, true, true),
        "promo_media_filmstrip" => ("A contact sheet of a clip", false, false, true, false),
        "promo_media_turntable" => ("A model from every side", false, false, true, false),
        "promo_render_still" => ("One frame", false, false, true, false),
        "promo_render_frames" => ("Look at the composition", false, false, true, false),
        "promo_render_video" => ("The video", false, false, true, false),
        "promo_render_gif" => ("The looping preview", false, false, true, false),
        "promo_proxy" => ("Build proxies for long sources", false, false, true, false),
        "promo_init" => ("Start a project", false, false, false, false),
        "promo_upsert_layer" => ("Add or change a layer", false, false, false, false),
        "promo_upsert_keyframe" => ("Add or change a keyframe", false, false, false, false),
        "promo_slideshow" => ("Author a whole show", false, false, false, false),
        // The one door that can DELETE: a layer, a resource, a keyframe.
        "promo_apply" => ("Apply editor commands", false, true, false, false),
        "promo_speak" => ("Synthesize the narration", false, false, false, true),
        "promo_list_projects" => ("The projects on this Mac", true, false, true, false),
        "promo_context" => ("What the person is looking at", true, false, true, false),
        "promo_open" => ("Open the project in PromoShot", false, false, true, false),
        _ => (name, false, false, false, false),
    };
    json!({
        "title": title,
        "readOnlyHint": read_only,
        "destructiveHint": destructive,
        "idempotentHint": idempotent,
        "openWorldHint": open_world,
    })
}

/// The editor's Command enum as promo_apply's `commands` item schema —
/// the vocabulary (which commands exist, and what each names) and NOT the
/// model's whole type graph.
///
/// Those types used to ride along: schemars pulls every type a command
/// mentions, and hoisting them put 90 definitions and 84 KB into
/// `tools/list`, which every client loads on connect and carries in every
/// request after. Each reference becomes a named placeholder that says
/// where its shape lives; a placeholder is an empty schema, so no client
/// rejects an argument it would have accepted.
fn command_items() -> Value {
    let mut items = promo_editor::command_schema();
    if let Some(map) = items.as_object_mut() {
        map.remove("$schema");
        map.remove("title");
        map.remove("$defs");
    }
    without_type_graph(items)
}

/// Every `$ref` in a schema replaced by a named placeholder.
///
/// `#/$defs/ProjectLayer` becomes "a ProjectLayer — its shape is in
/// promo_schema_types, its prose in promo_schema_full"; the bare `#`
/// schemars writes for a command nested inside another (inComposition)
/// used to resolve to promo_apply's own ARGUMENT object, which was never
/// what it meant, and becomes a placeholder saying so. The placeholder
/// carries no `type`, so it accepts whatever the reference accepted.
pub fn without_type_graph(node: Value) -> Value {
    match node {
        Value::Object(map) => {
            if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
                let named = reference.rsplit('/').next().unwrap_or(reference);
                let text = if reference == "#" {
                    "another command, applied inside the composition".to_string()
                } else {
                    format!(
                        "a {named} — its shape is in promo_schema_types, its prose in \
                         promo_schema_full"
                    )
                };
                return json!({ "description": text });
            }
            Value::Object(
                map.into_iter()
                    .map(|(key, value)| (key, without_type_graph(value)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.into_iter().map(without_type_graph).collect()),
        other => other,
    }
}

/// Refuses a call the host's contract does not describe: a tool it does
/// not serve, or an argument the tool does not take. Both servers ignored
/// an unknown argument in silence, so `codec` on a server that had no
/// codec rendered an h264 and said nothing.
pub fn check_arguments(host: Host, tool: &str, arguments: &Value) -> Result<(), String> {
    let list = tools(host);
    let Some(descriptor) = list.iter().find(|t| t["name"] == tool) else {
        let names: Vec<&str> = list.iter().filter_map(|t| t["name"].as_str()).collect();
        return Err(format!(
            "unknown tool `{tool}` — this server offers {}",
            names.join(", ")
        ));
    };
    let supplied = match arguments {
        Value::Null => return Ok(()),
        Value::Object(map) => map,
        _ => return Err(format!("{tool}: arguments must be an object")),
    };
    let taken: Vec<&str> = descriptor
        .pointer("/inputSchema/properties")
        .and_then(Value::as_object)
        .map(|p| p.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let mut unknown: Vec<&str> = supplied
        .keys()
        .map(String::as_str)
        .filter(|k| !taken.contains(k))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    unknown.sort_unstable();
    let quoted = |names: &[&str]| {
        names
            .iter()
            .map(|n| format!("`{n}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    Err(format!(
        "{tool} does not take {} — nothing was done. It takes {}.",
        quoted(&unknown),
        if taken.is_empty() {
            "no arguments".to_string()
        } else {
            quoted(&taken)
        }
    ))
}

/// A `promo_render_frames` call's ask, the tool's defaults applied: exact
/// times, else a range, else a sample of [`SAMPLE_FRAMES`] across the
/// piece.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LookRequest {
    pub times: Vec<f64>,
    pub sample: Option<usize>,
    pub from: Option<f64>,
    pub to: Option<f64>,
    pub fps: Option<f64>,
}

/// The ask in a `promo_render_frames` call — the headless server spells it
/// as CLI flags, the app hands it to [`look_times`].
pub fn look_request(arguments: &Value) -> LookRequest {
    let number = |key: &str| arguments.get(key).and_then(Value::as_f64);
    let times: Vec<f64> = arguments
        .get("times")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    let (from, to, fps) = (number("from"), number("to"), number("fps"));
    let mut sample = arguments
        .get("sample")
        .and_then(Value::as_f64)
        .map(|n| n.max(0.0).round() as usize);
    // A LOOK, not an export. Asked for nothing in particular this rendered
    // every frame of the composition at its own rate — 11,880 PNGs across
    // the demo corpus, none of them read as a whole. Twelve moments is a
    // contact sheet.
    if times.is_empty() && sample.is_none() && from.is_none() && to.is_none() && fps.is_none() {
        sample = Some(SAMPLE_FRAMES);
    }
    LookRequest {
        times,
        sample,
        from,
        to,
        fps,
    }
}

/// The moments a `promo_render_frames` call renders from a project — the
/// app's server renders exactly these; the headless server's CLI picks
/// them by the same [`promo_timeline::look_times`] from the same request.
pub fn look_times(
    meta: &promo_model::ProjectMetadata,
    arguments: &Value,
) -> Result<Vec<f64>, String> {
    let ask = look_request(arguments);
    let plan = promo_timeline::export_plan(meta, ask.fps, ask.from, ask.to);
    promo_timeline::look_times(
        &ask.times,
        ask.sample,
        &plan,
        promo_timeline::composition_duration(meta),
        Some(FRAME_CAP),
    )
}

/// The format's prose, whole or by topic.
///
/// The whole document is 73 KB, and nearly every session pulled all of it
/// — and "core", the format proper, was 51 KB of it, fetched in every run
/// the review read (review 2026-09-27, P2-34). The format proper is now
/// sections of at most 5 KB, each headed `## Title — word, word, …` with
/// the words an author reaches for (the skill's among them), and the
/// features after it are sections headed by their rung sentence. "core"
/// is the essentials before the first section; any heading word takes
/// its section. No topics still answers with everything.
pub fn schema_text(topics: &[String]) -> String {
    if topics.is_empty() {
        return promo_model::SCHEMA.to_string();
    }
    let (core, sections) = schema_split();
    let mut out = String::new();
    let mut missed: Vec<&str> = Vec::new();
    let mut taken: Vec<usize> = Vec::new();
    for asked in topics {
        let topic = asked.trim().to_ascii_lowercase();
        if topic == "core" || topic == "format" {
            out.push_str(core);
            out.push('\n');
            continue;
        }
        let mut found = false;
        for (index, section) in sections.iter().enumerate() {
            if section_answers(section, &topic) {
                found = true;
                // Two words for one section give it once.
                if !taken.contains(&index) {
                    taken.push(index);
                    out.push_str(section);
                    out.push_str("\n\n");
                }
            }
        }
        if !found {
            missed.push(asked.as_str());
        }
    }
    if !missed.is_empty() {
        let names: Vec<String> = sections.iter().map(|s| section_title(s)).collect();
        out.push_str(&format!(
            "\n(no section for {}; the topics are \"core\" plus: {})\n",
            missed.join(", "),
            names.join(", ")
        ));
    }
    out
}

/// The format's machine schema, whole or by type name.
///
/// Whole it is 80 KB pretty-printed, which an agent read to find one
/// struct (review 2026-09-27, P2-34). Named types answer with just their
/// definitions, the names each references (ask for those next), and the
/// case of a name need not match. An unknown name is refused with the
/// names there are.
pub fn schema_types_text(types: &[String]) -> Result<String, String> {
    let schema = promo_model::wire_schema();
    if types.is_empty() {
        return serde_json::to_string_pretty(&schema).map_err(|e| e.to_string());
    }
    let defs = schema
        .get("$defs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let root_name = schema
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("ProjectMetadata")
        .to_string();
    let mut picked = serde_json::Map::new();
    let mut unknown: Vec<&str> = Vec::new();
    for asked in types {
        let wanted = asked.trim();
        if wanted.eq_ignore_ascii_case(&root_name) {
            let mut root = schema.clone();
            if let Some(map) = root.as_object_mut() {
                map.remove("$defs");
                map.remove("$schema");
            }
            picked.insert(root_name.clone(), root);
            continue;
        }
        match defs
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
        {
            Some((name, body)) => {
                picked.insert(name.clone(), body.clone());
            }
            None => unknown.push(asked.as_str()),
        }
    }
    if !unknown.is_empty() {
        let mut names: Vec<&str> = defs.keys().map(String::as_str).collect();
        names.push(root_name.as_str());
        names.sort_unstable();
        return Err(format!(
            "no type named {} — the types are {}",
            unknown.join(", "),
            names.join(", ")
        ));
    }
    // The names the picked definitions point at, for the next ask.
    fn references(node: &Value, into: &mut std::collections::BTreeSet<String>) {
        match node {
            Value::Object(map) => {
                if let Some(target) = map.get("$ref").and_then(Value::as_str) {
                    if let Some(name) = target.rsplit('/').next() {
                        into.insert(name.to_string());
                    }
                }
                map.values().for_each(|v| references(v, into));
            }
            Value::Array(items) => items.iter().for_each(|v| references(v, into)),
            _ => {}
        }
    }
    let mut referenced = std::collections::BTreeSet::new();
    picked.values().for_each(|v| references(v, &mut referenced));
    let referenced: Vec<String> = referenced
        .into_iter()
        .filter(|name| !picked.contains_key(name))
        .collect();
    serde_json::to_string_pretty(&json!({ "types": picked, "references": referenced }))
        .map_err(|e| e.to_string())
}

/// Does `section` answer to `topic` (lowercase)? A `## Title — words`
/// section answers to its title and each word, singular or plural; a
/// feature section to any part of its rung sentence, as it always has.
fn section_answers(section: &str, topic: &str) -> bool {
    let heading = section.lines().next().unwrap_or_default();
    match heading.strip_prefix("## ") {
        Some(rest) => {
            let (title, words) = rest.split_once(" — ").unwrap_or((rest, ""));
            std::iter::once(title)
                .chain(words.split(','))
                .map(|w| w.trim().to_ascii_lowercase())
                .filter(|w| !w.is_empty())
                .any(|w| w == topic || format!("{w}s") == topic || w == format!("{topic}s"))
        }
        None => heading.to_ascii_lowercase().contains(topic),
    }
}

/// A section's name in the list of topics: its title, or its rung
/// sentence up to the rung.
fn section_title(section: &str) -> String {
    let heading = section.lines().next().unwrap_or_default();
    match heading.strip_prefix("## ") {
        Some(rest) => rest
            .split(" — ")
            .next()
            .unwrap_or(rest)
            .to_ascii_lowercase(),
        None => heading
            .split(" (rung")
            .next()
            .unwrap_or(heading)
            .to_string(),
    }
}

/// The format proper, and the sections after it.
///
/// A section starts at a `## ` heading or at an unindented sentence naming
/// its rung — the feature sections' shape — and runs to the next one.
/// Splitting on blank lines instead missed every section glued to the
/// paragraph above it, which is most of them.
fn schema_split() -> (&'static str, Vec<&'static str>) {
    let text = promo_model::SCHEMA;
    let mut starts: Vec<usize> = Vec::new();
    let mut at = 0usize;
    for line in text.split_inclusive('\n') {
        let head = line.trim_end();
        let titled = head.starts_with("## ");
        let rung = head.contains("(rung ") && head.starts_with(|c: char| c.is_ascii_uppercase());
        if titled || rung {
            starts.push(at);
        }
        at += line.len();
    }
    let Some(&first) = starts.first() else {
        return (text, Vec::new());
    };
    let sections: Vec<&'static str> = starts
        .iter()
        .enumerate()
        .map(|(i, &start)| {
            let end = starts.get(i + 1).copied().unwrap_or(text.len());
            text[start..end].trim_end()
        })
        .collect();
    (&text[..first], sections)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn by_name(list: &[Value]) -> std::collections::BTreeMap<String, &Value> {
        list.iter()
            .map(|t| (t["name"].as_str().unwrap().to_string(), t))
            .collect()
    }

    fn keys(tool: &Value) -> BTreeSet<String> {
        tool.pointer("/inputSchema/properties")
            .and_then(Value::as_object)
            .map(|p| p.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// The structural difference between two argument schemas: type and
    /// enum per argument, required list — what a caller relies on. The
    /// prose may be host-specific; the shape may not.
    fn shape(tool: &Value, argument: &str) -> Value {
        let schema = &tool["inputSchema"]["properties"][argument];
        json!({ "type": schema["type"], "enum": schema["enum"], "items": schema["items"]["type"] })
    }

    /// The two servers' lists differ exactly where [`HOST_DIFFERENCES`]
    /// says, and nowhere else: the same tools, the same arguments with the
    /// same types and choices, the same required fields, the same hints.
    #[test]
    fn the_hosts_differ_only_where_declared() {
        let headless = tools(Host::Headless);
        let app = tools(Host::App);
        let (h, a) = (by_name(&headless), by_name(&app));
        let mut found: BTreeSet<String> = BTreeSet::new();
        for name in h.keys().chain(a.keys()).collect::<BTreeSet<_>>() {
            let (Some(ht), Some(at)) = (h.get(name), a.get(name)) else {
                let only = if h.contains_key(name) {
                    "Headless"
                } else {
                    "App"
                };
                found.insert(format!("{name} tool only {only}"));
                continue;
            };
            for argument in keys(ht).symmetric_difference(&keys(at)) {
                let only = if keys(ht).contains(argument) {
                    "Headless"
                } else {
                    "App"
                };
                found.insert(format!("{name} argument {argument} only {only}"));
            }
            for argument in keys(ht).intersection(&keys(at)) {
                assert_eq!(
                    shape(ht, argument),
                    shape(at, argument),
                    "{name}.{argument} has one shape"
                );
            }
            assert_eq!(
                ht["inputSchema"]["required"], at["inputSchema"]["required"],
                "{name}: one required list"
            );
            for hint in [
                "readOnlyHint",
                "destructiveHint",
                "idempotentHint",
                "openWorldHint",
            ] {
                if ht["annotations"][hint] != at["annotations"][hint] {
                    let only = if at["annotations"][hint] == true {
                        "App"
                    } else {
                        "Headless"
                    };
                    found.insert(format!("{name} annotation {hint} only {only}"));
                }
            }
        }
        let declared: BTreeSet<String> = HOST_DIFFERENCES
            .iter()
            .map(|d| {
                let only = format!("{:?}", d.only);
                match d.aspect {
                    Aspect::Tool => format!("{} tool only {only}", d.tool),
                    Aspect::Argument(a) => format!("{} argument {a} only {only}", d.tool),
                    Aspect::Annotation(a) => format!("{} annotation {a} only {only}", d.tool),
                }
            })
            .collect();
        assert_eq!(found, declared, "drift between the servers' tool lists");
        assert!(HOST_DIFFERENCES.iter().all(|d| !d.why.is_empty()));
    }

    /// Every descriptor is well formed on both hosts: a description that
    /// says what the tool is for, required fields it actually takes, a
    /// human title and all four hints.
    #[test]
    fn every_descriptor_is_complete() {
        for host in [Host::Headless, Host::App] {
            for tool in tools(host) {
                let name = tool["name"].as_str().unwrap();
                assert!(
                    tool["description"].as_str().is_some_and(|d| d.len() > 40),
                    "{name} says what it is for"
                );
                let taken = keys(&tool);
                for required in tool["inputSchema"]["required"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    assert!(
                        taken.contains(required.as_str().unwrap()),
                        "{name} requires {required}"
                    );
                }
                let a = &tool["annotations"];
                assert!(
                    a["title"].as_str().is_some_and(|t| t != name),
                    "{name} has a title"
                );
                for hint in [
                    "readOnlyHint",
                    "destructiveHint",
                    "idempotentHint",
                    "openWorldHint",
                ] {
                    assert!(a[hint].is_boolean(), "{name}.{hint}");
                }
            }
        }
    }

    /// An argument a tool does not take is refused, naming what it does
    /// take; so is a tool the host does not serve.
    #[test]
    fn a_call_outside_the_contract_is_refused() {
        let render = json!({ "project": "/tmp/A.promo", "codec": "prores4444", "scale": 50 });
        let refused = check_arguments(Host::App, "promo_render_video", &render).unwrap_err();
        assert!(refused.contains("does not take `scale`"), "{refused}");
        assert!(refused.contains("`codec`"), "codec is taken: {refused}");
        assert!(check_arguments(
            Host::Headless,
            "promo_render_video",
            &json!({ "size": "640x360" })
        )
        .is_ok());
        assert!(check_arguments(
            Host::App,
            "promo_render_video",
            &json!({ "size": "640x360" })
        )
        .is_err());
        assert!(check_arguments(Host::Headless, "promo_open", &json!({}))
            .unwrap_err()
            .contains("unknown tool"));
        assert!(check_arguments(Host::App, "promo_open", &Value::Null).is_ok());
        assert!(check_arguments(Host::App, "promo_schema", &json!(["x"])).is_err());
    }

    /// A bare look samples twelve moments across the piece; exact times,
    /// a sample or a range say otherwise; the cap and the composition's
    /// end are enforced with the way out named.
    #[test]
    fn a_look_asks_for_twelve_moments_unless_told() {
        assert_eq!(look_request(&json!({})).sample, Some(SAMPLE_FRAMES));
        assert_eq!(look_request(&json!({ "times": [1.0] })).sample, None);
        assert_eq!(
            look_request(&json!({ "from": 1.0, "to": 2.0 })).sample,
            None
        );
        assert_eq!(look_request(&json!({ "sample": 4 })).sample, Some(4));
        let meta: promo_model::ProjectMetadata = serde_json::from_value(json!({
            "id": "P", "name": "look", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
            "compositionSettings": { "canvasWidth": 64, "canvasHeight": 64, "fps": 30 },
            "layers": [{ "id": "bg", "name": "bg", "sortIndex": 0, "kind": "background",
                         "isEnabled": true, "startTime": 0, "duration": 10, "keyframes": [] }]
        }))
        .unwrap();
        let bare = look_times(&meta, &json!({})).unwrap();
        assert_eq!(bare.len(), SAMPLE_FRAMES);
        assert_eq!((bare[0], *bare.last().unwrap()), (0.0, 10.0));
        let every = look_times(&meta, &json!({ "from": 0, "to": 10 })).unwrap_err();
        assert!(every.starts_with("300 frames"), "{every}");
        assert_eq!(
            look_times(&meta, &json!({ "from": 1, "to": 2, "fps": 2 })).unwrap(),
            vec![1.0, 1.5]
        );
        assert!(look_times(&meta, &json!({ "times": [11] })).is_err());
    }

    /// The format's prose by topic. The whole document is 73 KB and nearly
    /// every session pulled all of it; a piece that uses particles can have
    /// the particles instead. No argument still answers with everything.
    #[test]
    fn the_format_can_be_asked_for_by_topic() {
        assert_eq!(
            schema_text(&[]),
            promo_model::SCHEMA,
            "everything, as before"
        );

        let core = schema_text(&["core".into()]);
        assert!(core.starts_with("A PromoShot project is a FOLDER"));
        assert!(core.len() <= 5_000, "core is {} bytes", core.len());
        assert!(
            !core.contains("(rung 36)"),
            "the feature sections are not in it"
        );

        let particles = schema_text(&["Particles".into()]);
        assert!(particles.contains("Particles (rung 36)"));
        assert!(
            particles.contains("MORPH (rung 39)"),
            "both particle sections"
        );
        assert!(
            !particles.contains("Chroma key (rung 22)"),
            "and nothing else"
        );
        assert!(particles.len() < 4_000, "{} bytes", particles.len());

        // An unknown topic answers with the ones that exist rather than
        // with silence.
        let missed = schema_text(&["confetti".into()]);
        assert!(missed.contains("no section for confetti"), "{missed}");
        assert!(
            missed.contains("Particles") && missed.contains("Stages"),
            "{missed}"
        );
    }

    /// Every section of the format is at most 5 KB, and every topic word
    /// the skill hands an agent takes a section — `reveal`, `tracking`,
    /// `rect` and `transition` took nothing, and "core" was 51 KB fetched
    /// in every run (review 2026-09-27, P2-34).
    #[test]
    fn every_section_is_small_and_every_skill_word_finds_one() {
        let (core, sections) = schema_split();
        assert!(core.len() <= 5_000, "core is {} bytes", core.len());
        for section in &sections {
            assert!(
                section.len() <= 5_000,
                "{} is {} bytes",
                section_title(section),
                section.len()
            );
        }
        let skill = include_str!("../../skill/SKILL.md");
        let start = skill
            .find("The features, by the topic word that fetches them")
            .expect("the skill's topic list");
        let list = &skill[start..];
        let list = &list[..list.find("\n\n**").unwrap_or(list.len())];
        let mut words: Vec<&str> = list
            .split("** (`")
            .skip(1)
            .filter_map(|rest| rest.split('`').next())
            .collect();
        words.sort_unstable();
        words.dedup();
        assert!(words.len() >= 15, "the skill's words: {words:?}");
        for word in words {
            let text = schema_text(&[word.to_string()]);
            assert!(
                !text.contains("no section for"),
                "`{word}` takes no section"
            );
            assert!(text.len() <= 12_000, "`{word}` takes {} bytes", text.len());
        }
        // A word two sections answer to comes once per section.
        let both = schema_text(&["reveal".into(), "kinetic".into()]);
        assert_eq!(both.matches("## Reveal").count(), 1);
        for word in [
            "transition",
            "tracking",
            "rect",
            "keyframe",
            "placement",
            "mask",
            "palette",
        ] {
            let text = schema_text(&[word.to_string()]);
            assert!(
                text.starts_with("## "),
                "`{word}` takes a titled section: {}",
                &text[..80]
            );
        }
    }

    /// The machine schema by type name: just those definitions and what
    /// they reference, any case; an unknown name lists the names there are.
    #[test]
    fn the_types_can_be_asked_for_by_name() {
        let whole = schema_types_text(&[]).unwrap();
        assert!(whole.len() > 40_000, "{} bytes whole", whole.len());
        let layer = schema_types_text(&["projectlayer".into()]).unwrap();
        let answer: Value = serde_json::from_str(&layer).unwrap();
        assert!(answer["types"]["ProjectLayer"].is_object(), "{layer}");
        assert!(layer.len() < whole.len() / 4, "{} bytes", layer.len());
        assert!(
            answer["references"]
                .as_array()
                .is_some_and(|r| !r.is_empty()),
            "it names what it points at"
        );
        let refused = schema_types_text(&["Confetti".into()]).unwrap_err();
        assert!(refused.contains("no type named Confetti"), "{refused}");
        assert!(refused.contains("ProjectLayer"), "{refused}");
    }

    /// The handshake teaches the loop on both hosts; only the app speaks
    /// of a person at the machine.
    #[test]
    fn the_handshake_teaches_the_loop() {
        for host in [Host::Headless, Host::App] {
            let text = instructions(host);
            for step in [
                "promo_schema",
                "promo_workspace",
                "promo_validate",
                "promo_render_frames",
            ] {
                assert!(text.contains(step), "{host:?} names {step}");
            }
        }
        assert!(instructions(Host::App).contains("promo_open"));
        assert!(!instructions(Host::Headless).contains("promo_open"));
    }
}
