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

use project::Project;
use promo_cli::sheet::{blit, grid_for};
use promo_cli::{project, render};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
promo — render a PromoShot project folder

USAGE:
    promo validate <project-dir> [--strict] [--json]
    promo schema [--full|--types]
    promo skill [install [--home DIR | --into DIR] [--dry-run]]
        print the agent skill, or install it for every agent tool found
    promo inspect <project-dir> [--json]
    promo proxy   <project-dir>
        a playback proxy for every video resource, built into the proxy
        cache (never into the project folder); idempotent
    promo still   <project-dir> --out <file.png> [--time <s>] [--size <WxH>]
    promo frames  <project-dir> --out <dir> [--times <s,s,…>|--sample <n>|--fps <n>]
                  [--from <s>] [--to <s>] [--size <WxH>] [--sheet <file.png>] [--cap <n>]
    promo video   <project-dir> --out <file.mp4> [--fps <n>] [--size <WxH>]
                  [--from <s|Nf>] [--to <s|Nf>] [--segment <s>]
        resumable: finished segments wait in <file.mp4>.promo-parts/, and
        the same command run again after a kill finishes from there
    promo gif     <project-dir> --out <file.gif> [--fps <n>] [--size <WxH>]
    promo model   <file.glb> [--json]
    promo device  <phone|tablet|tablet-portrait|laptop> --out <file.glb> [--json]
    promo turntable <file.glb> --out <sheet.png> [--count <n>] [--size <WxH>] [--json]
    promo beats   <audio-file | project-dir> [--resource <id>] [--json]
                  [--meter <n>] [--bpm-hint <n> | --clear-hint] [--refresh]
        the beats of a song: tempo, beats, downbeats, sections, confidence.
        On a project, the clock's (or the named) audio resource — the
        analysis is cached in Resources/.beats/ and is the answer from then
        on; --refresh analyses again. On a project --bpm-hint is kept on the
        song (its bpmHint) for every later measure; --clear-hint drops it
    promo words   <audio-file | project-dir> [--resource <id>]
                  [--script <text> | --script-file <f>] [--json] [--timeline]
                  [--refresh] [--no-analyse] [--transcribe openai|elevenlabs [--check]]
                  [--import <timings.json> [--provider <name>]] [--prune]
        when each word of a take is said, and its sentences: the record in
        the project's Resources/.words/ (provider or local-voice timing),
        else the take aligned to its script, offline, and stored. On an
        audio file, the script is given with --script. --transcribe spends
        the person's key and is never automatic (--check says the cost).
        --import stores word timings the voice handed over (edge-tts
        WordBoundary events, or [{\"text\",\"start\",\"end\"}] in seconds) as the
        take's provider timing (--provider names it; edge-tts is guessed)
    promo midi render <project-dir> [resource] [--bank <file.sf2>] [--check] [--json]
        render a MIDI-sequenced audio resource (every one a layer uses,
        when none is named) into Resources/ with the sound bank, writing its
        filename, duration, beatGrid and receipt; an unchanged block is
        reused. `promo video` settles the used ones itself
    promo midi generate <project-dir> <resource> --track <name> [--caption <text>]
                  [--seed <n>] [--range <from>-<to>] [--as <track_name>] [--bank <file.sf2>]
                  [--json]
        a generated layer: the music model (lego) hears the MIDI and adds the
        track (a new one needs --caption; an existing one regenerates), kept in
        Resources/.midi/ when it keeps to the grid (drums, percussion) or the
        chords (the rest), mixed into the take. --as is lego's track_name
        (strings, synth, keyboard, drums…; else read from the caption). Needs
        the music model installed; without it, it says so
    promo sounds [list | install generaluser]
        the sound banks MIDI renders with (PROMOSHOT_SOUNDS, else the
        per-user folder); install fetches GeneralUser GS and its licence
    promo music [list | install ace-step-base | remove ace-step-base]
        the music model `midi generate` runs (ACE-Step 1.5 base, native, about
        6.3 GB; PROMOSHOT_MUSIC_MODELS, else the per-user folder); install
        downloads the pinned Hugging Face files, verifies each SHA-256 and
        resumes an interrupted download
    promo resource add <project-dir> <file> [--id <id>] [--name <text>] [--json]
        copy a picture, a clip or an SVG into Resources/ and declare it. An
        SVG stays itself: a vector resource the core draws sharp at any size,
        its text kept as outlines in Resources/.svg/ (no fonts at render)
    promo media activity <video> [--rate <n>] [--pointer <pointer.json>]
                  [--masks auto|phone|desktop|none]
        where a screen recording's picture changed, as the resource's
        `activity` JSON (what Auto zoom plans from)

AUTHORING — the MCP tools' own functions, so the defaults and checks match:
    promo init            <project-dir> --args '{\"canvas\":\"1920x1080\"}'
    promo upsert-layer    <project-dir> --args '{\"kind\":\"caption\",\"captionText\":\"Hi\"}'
    promo upsert-keyframe <project-dir> --args '{\"layer\":\"bg\",\"time\":2,\"zoom\":1.2}'
    promo apply           <project-dir> --args '{\"commands\":[{\"kind\":\"renameLayer\",…}]}'
    promo slideshow       <project-dir> --args '{\"slides\":[{\"file\":\"a.png\"}]}'
    promo explain         <project-dir> --args '{\"layer\":\"bg\",\"time\":2}'
    promo diff            <project-dir> --args '{\"against\":\"before.json\"}'
        --args is the tool's arguments as one JSON object (inline, @file.json,
        or - for stdin) — the same keys promo_<verb> takes, minus `project`.

OPTIONS (`promo <command> --help` lists the ones a command takes):
    --out <path>   Where the render, frames or model is written
    --time <s>     Timestamp for a still (default 0)
    --fps <n>      Frames per second, overriding the project's own
                   (default: the project's `fps`, else 30). Fractional rates
                   are allowed — 59.94 matches a typical screen recording
                   exactly, where 30 resamples it.
    --from/--to    Time range (default: the whole composition): seconds
                   (12.5 or 12.5s) or frames at the render's rate (375f)
    --segment <s>  video: seconds of video per resumable segment (default 10)
    --times <list> Render exactly these seconds, comma separated
    --sample <n>   n moments spread evenly across the range — a contact
                   sheet rather than every frame
    --sheet <file> Also tile the rendered moments into one PNG
    --cap <n>      Refuse rather than render more than n frames
    --size <WxH>   Output size (default: the project's canvas size)
    --json         Machine output: one JSON object on stdout (errors too)
    --no-analyse   A beat clock's song with no beatGrid is read from the
                   project's cache (Resources/.beats/) only — by default it
                   is analysed on open and the analysis cached; for `words`,
                   a take with no record answers an estimate instead of
                   being aligned
    --strict       validate: exit 1 when anything is reported, and list the
                   notes too
    --full         schema: the whole format, not the authoring subset
    --types        schema: the wire types as JSON Schema
    --home <dir>   skill install: look for agent tools under this home folder
    --into <dir>   skill install: install into this one folder only
    --dry-run      skill install: say what would be written, write nothing
    --count <n>    turntable: how many views around the model (1–64)
    --id <id>      resource add: the new resource's id (default: a UUID)
    --name <text>  resource add: its display name (default: the file's name)
    --resource <id>
                   beats, words: the project's resource to measure (default:
                   the beat clock's song / the take a speech value names)
    --refresh      beats, words: measure again, replacing the cached answer
    --meter <n>    beats: beats per bar, 1–16 (default 4)
    --bpm-hint <n> beats: the tempo to look near; on a project it is kept as
                   the song's bpmHint for every later measure
    --clear-hint   beats: drop the song's stored bpmHint
    --script <text>, --script-file <f>
                   words: what an audio file says, to align it to
    --timeline     words: also the times on the output timeline, through the
                   first layer that plays the take
    --transcribe openai|elevenlabs
                   words: ask a provider to time the take, on the person's
                   key — never automatic
    --check        words, with --transcribe: say what it would cost, spend
                   nothing; midi render: render nothing, say whether each
                   take is current
    --bank <file.sf2>
                   midi render, midi generate: the SoundFont to render with
                   (default: the installed GeneralUser GS)
    --track <name> midi generate: the track the layer is (new, or one to
                   regenerate)
    --caption <text>
                   midi generate: what the layer plays (\"warm Rhodes
                   comping\"); a new track needs one
    --seed <n>     midi generate: the take's seed (default: a new one)
    --range <from>-<to>
                   midi generate: the bars it plays, 1-based, the upper
                   bound exclusive (default: the whole take)
    --as <track_name>
                   midi generate: lego's track_name (strings, synth,
                   keyboard, drums…; default: read from the caption)
    --import <timings.json>
                   words: store word timings the voice handed over (edge-tts
                   WordBoundary events, or [{\"text\",\"start\",\"end\"}] in
                   seconds) as the take's provider timing
    --provider <name>
                   words, with --import: whose timing it is (edge-tts is
                   guessed)
    --prune        words: remove the records no file in Resources/ has
    --rate <n>     media activity: frames a second to measure at (0–120)
    --pointer <pointer.json>
                   media activity: the recorder's pointer track (or a
                   resource carrying one under `pointer`)
    --masks auto|phone|desktop|none
                   media activity: the screen regions to ignore (status bar,
                   clock…)
    --args <json>  authoring: the tool's arguments as one JSON object
                   (inline, @file.json, or - for stdin)

    `validate` decodes the project and reports, first, anything that will not
    render or has no effect — a field nothing reads, a value that does
    nothing where it is, missing media; the answer then starts NOT OK — and
    then what the renderer adjusts (warnings). It exits 1 when the project
    cannot be decoded; with --strict, also when anything is reported, and
    the notes (things usually meant, like a pulse over keyed keyframes) are
    listed too.

    `schema` prints the authoring subset plus six complete recipes — the
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
    if args[0] == "--version" || args[0] == "-V" {
        println!("promo {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    match run(&args) {
        Ok(code) => code,
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

fn run(args: &[String]) -> Result<ExitCode, String> {
    let command = args[0].as_str();
    let rest = &args[1..];
    // `promo <command> --help`, for every command: its usage and the
    // options it takes, before any command reads its arguments (issue #20).
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        let verb = match (command, rest.first().map(String::as_str)) {
            ("media", Some(sub)) if !sub.starts_with('-') => format!("media {sub}"),
            _ => command.to_string(),
        };
        return match command_help(&verb) {
            Some(help) => {
                print!("{help}");
                Ok(ExitCode::SUCCESS)
            }
            None => Err(format!("unknown command `{verb}`\n\n{USAGE}")),
        };
    }
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
        return Ok(ExitCode::SUCCESS);
    }
    // The skill travels with the binary: printed, or installed for every
    // agent tool found under the home folder. No project either.
    if command == "skill" {
        return skill(rest).map(|()| ExitCode::SUCCESS);
    }
    // Beats take a song or a project, with options of their own.
    if command == "beats" {
        println!("{}", beats(rest)?);
        return Ok(ExitCode::SUCCESS);
    }
    // Word timing takes a take or a project, with options of its own.
    if command == "words" {
        println!("{}", words(rest)?);
        return Ok(ExitCode::SUCCESS);
    }
    // MIDI renders take a project and a verb; the banks take neither.
    if command == "midi" {
        println!("{}", midi(rest)?);
        return Ok(ExitCode::SUCCESS);
    }
    if command == "sounds" {
        println!("{}", sounds(rest)?);
        return Ok(ExitCode::SUCCESS);
    }
    if command == "music" {
        println!("{}", music(rest)?);
        return Ok(ExitCode::SUCCESS);
    }
    // `resource add` takes a project AND a file.
    if command == "resource" {
        println!("{}", resource(rest)?);
        return Ok(ExitCode::SUCCESS);
    }
    // Media senses take a FILE and a verb, not a project.
    if command == "media" {
        println!("{}", media(rest)?);
        return Ok(ExitCode::SUCCESS);
    }
    let dir = rest
        .first()
        .filter(|a| !a.starts_with("--"))
        .ok_or_else(|| format!("{command}: expected a project directory\n\n{USAGE}"))?;
    // Authoring answers before a project is opened: `init` creates one, and
    // the rest write through promo-author, which reads the file itself.
    if AUTHORING.contains(&command) {
        let answer = author(command, dir, &rest[1..])?;
        println!("{answer}");
        return Ok(ExitCode::SUCCESS);
    }
    let mut opts = Options::parse(&rest[1..])?;
    // The model senses take a FILE, not a project: what a glb holds, and
    // how it looks from around — before anyone places it.
    if command == "model" || command == "turntable" || command == "device" {
        let file = Path::new(dir);
        let answer = match command {
            "model" => model_probe(file, &opts),
            "device" => device(dir, &opts),
            _ => turntable(file, &opts),
        }?;
        println!("{answer}");
        return Ok(ExitCode::SUCCESS);
    }
    let analysis = if opts.no_analyse {
        promo_cli::project::Analysis::CacheOnly
    } else {
        promo_cli::project::Analysis::Allowed
    };
    // An export's sound includes every MIDI take a layer uses: settled
    // first (rendered when stale), or refused with how to get the bank.
    if command == "video" {
        for line in settle_used_midi(Path::new(dir))? {
            eprintln!("{line}");
        }
    }
    let project = Project::open_with(Path::new(dir), analysis)?;
    opts.resolve_frames(&project, command);

    // One report, printed once — text or JSON — and with --strict the exit
    // code says whether it had anything in it.
    if command == "validate" {
        let report = validation(&project);
        println!("{}", report_answer(&report, &opts));
        return Ok(if opts.strict && !report.is_clean_with_notes() {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        });
    }

    // A render lays out on the beats; with a clock whose audio has no grid
    // it would lay out on the nominal 120 BPM instead — so it waits.
    if matches!(command, "still" | "frames" | "video" | "gif") && !project.missing_grids.is_empty()
    {
        return Err(format!(
            "not rendering: the beat clock's audio {} has no beat grid yet — set \
             \"beatGrid\": {{\"bpm\": …}} on the resource (promo validate says the same)",
            project
                .missing_grids
                .iter()
                .map(|id| format!("\"{id}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    // Speech values laid out on an estimate would cut on guessed words —
    // so a render waits for the takes' timing, as for a grid.
    if matches!(command, "still" | "frames" | "video" | "gif")
        && !project.timing.missing_words.is_empty()
    {
        let takes = project
            .timing
            .missing_words
            .iter()
            .map(
                |id| match project.missing_words.iter().find(|(take, _)| take == id) {
                    Some((_, why)) => format!("\"{id}\" ({why})"),
                    None => format!("\"{id}\""),
                },
            )
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "not rendering: speech values count on {takes}, which has no word timing yet — \
             run `promo words {dir} --resource <id>` (promo validate says the same)"
        ));
    }
    let answer = match command {
        "inspect" => inspect(&project, &opts),
        "proxy" => build_proxies(&project, &opts),
        "still" => still(&project, &opts).map(|a| with_credits(a, command, &project, &opts)),
        "frames" => frames(&project, &opts).map(|a| with_credits(a, command, &project, &opts)),
        "video" => video(&project, &opts).map(|a| with_credits(a, command, &project, &opts)),
        "gif" => gif(&project, &opts).map(|a| with_credits(a, command, &project, &opts)),
        other => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    }?;
    println!("{answer}");
    Ok(ExitCode::SUCCESS)
}

/// One command's help, cut from [`USAGE`] so the two never disagree: its
/// usage lines and description, every option it names with that option's
/// entry under OPTIONS, and its own paragraph below them (`validate`,
/// `schema`). None for a word that is not a command. `media` alone is
/// every `promo media …` verb.
fn command_help(command: &str) -> Option<String> {
    let lines: Vec<&str> = USAGE.lines().collect();
    let options_at = lines.iter().position(|l| l.starts_with("OPTIONS"))?;
    let head = format!("promo {command}");
    // The usage block: its `promo <command>` lines, each with the deeper
    // lines under it.
    let mut block: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < options_at {
        let line = lines[i].trim_start();
        let named = line == head
            || line
                .strip_prefix(head.as_str())
                .is_some_and(|after| after.starts_with(' '));
        if named && lines[i].starts_with("    promo ") {
            block.push(lines[i]);
            i += 1;
            while i < options_at
                && lines[i].starts_with("      ")
                && !lines[i].trim_start().starts_with("promo ")
            {
                block.push(lines[i]);
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    if block.is_empty() {
        return None;
    }
    let mut out = String::from("USAGE:\n");
    for line in &block {
        out.push_str(line);
        out.push('\n');
    }
    // The authoring verbs share one note, which sits under the last.
    if AUTHORING.contains(&command) && !block.iter().any(|l| l.contains("--args is")) {
        out.push_str(
            "        --args is the tool's arguments as one JSON object (inline, @file.json,\n        \
             or - for stdin) — the same keys promo_<verb> takes, minus `project`.\n",
        );
    }
    // The flags the block names, and each one's entry under OPTIONS.
    let named: std::collections::BTreeSet<String> = block
        .iter()
        .flat_map(|l| l.split(|c: char| c.is_whitespace() || "[]|,(){}".contains(c)))
        .filter(|w| w.starts_with("--") && w.len() > 2)
        .map(|w| w.trim_end_matches(['.', ';', ':']).to_string())
        .collect();
    let mut options = String::new();
    let mut j = options_at + 1;
    while j < lines.len() && !lines[j].is_empty() {
        let entry_start = j;
        j += 1;
        while j < lines.len() && lines[j].starts_with("     ") {
            j += 1;
        }
        let first = lines[entry_start].trim_start();
        let flags = first
            .split(|c: char| c.is_whitespace() || c == '/' || c == ',')
            .filter(|w| w.starts_with("--"));
        if flags.into_iter().any(|f| named.contains(f)) {
            for line in &lines[entry_start..j] {
                options.push_str(line);
                options.push('\n');
            }
        }
    }
    if !options.is_empty() {
        out.push_str("\nOPTIONS:\n");
        out.push_str(&options);
    }
    // Its own paragraph after the options, when it has one.
    let own = format!("    `{command}`");
    if let Some(at) = lines.iter().position(|l| l.starts_with(&own)) {
        out.push('\n');
        for line in lines[at..].iter().take_while(|l| !l.is_empty()) {
            out.push_str(line);
            out.push('\n');
        }
    }
    Some(out)
}

/// The authoring verbs, each the MCP tool of the same name.
const AUTHORING: [&str; 7] = [
    "init",
    "upsert-layer",
    "upsert-keyframe",
    "apply",
    "slideshow",
    "explain",
    "diff",
];

/// Measured the way the renderer will read it: a clip's DISPLAYED size,
/// and a picture upright in sRGB — rewritten as a PNG when it needed
/// anything to get there (HEIC, an EXIF rotation, a wide-gamut profile),
/// so the file in Resources/ reads the same on every host.
fn host_probe(path: &Path, video: bool) -> promo_author::MediaInfo {
    if !video {
        if let Ok(Some((png, width, height))) = promo_media::still::normalized_png(path) {
            return promo_author::MediaInfo {
                duration: None,
                pixels: Some((width as f64, height as f64)),
                normalized: Some(png),
            };
        }
    }
    match promo_media::probe_stream(path) {
        Ok(info) => promo_author::MediaInfo {
            duration: (info.duration_s > 0.0).then_some(info.duration_s),
            pixels: (info.width > 0 && info.height > 0)
                .then_some((info.width as f64, info.height as f64)),
            normalized: None,
        },
        Err(_) => promo_author::MediaInfo::default(),
    }
}

/// `promo resource add <project> <file> [--id] [--name] [--json]`.
fn resource(rest: &[String]) -> Result<String, String> {
    let usage = "resource: expected `add <project-dir> <file>`";
    let (Some("add"), Some(dir), Some(file)) = (
        rest.first().map(String::as_str),
        rest.get(1).filter(|a| !a.starts_with("--")),
        rest.get(2).filter(|a| !a.starts_with("--")),
    ) else {
        return Err(usage.into());
    };
    let mut args = serde_json::json!({ "project": dir, "file": file });
    let mut json = false;
    let mut i = 3;
    while i < rest.len() {
        let value = || {
            rest.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{}: expected a value", rest[i]))
        };
        match rest[i].as_str() {
            "--id" => args["resourceId"] = serde_json::json!(value()?),
            "--name" => args["name"] = serde_json::json!(value()?),
            "--json" => {
                json = true;
                i += 1;
                continue;
            }
            other => return Err(format!("resource add: unknown option `{other}`")),
        }
        i += 2;
    }
    let answer = promo_author::add_resource(&args, None, &host_probe)?;
    Ok(if json {
        serde_json::json!({ "ok": true, "answer": answer }).to_string()
    } else {
        answer
    })
}

/// An authoring verb through promo-author — the functions the headless MCP
/// server calls (review 2026-09-27, P1-22). A CLI-only agent hand-wrote
/// JSON and met none of the tools' guardrails: a keyframe created by
/// `promo_upsert_keyframe` ramps from the previous one, a hand-written one
/// holds until 0.5 s before it — the stress test's "keyframes never
/// animate". The arguments are the tool's own, as one JSON object.
fn author(command: &str, dir: &str, rest: &[String]) -> Result<String, String> {
    let tool = format!("promo_{}", command.replace('-', "_"));
    let mut args = serde_json::Map::new();
    let mut json = false;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--args" => {
                let raw = rest
                    .get(i + 1)
                    .ok_or("--args: expected a JSON object, @file.json or -")?;
                let text = if raw == "-" {
                    let mut buffer = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buffer)
                        .map_err(|e| format!("--args -: {e}"))?;
                    buffer
                } else if let Some(file) = raw.strip_prefix('@') {
                    std::fs::read_to_string(file).map_err(|e| format!("--args {raw}: {e}"))?
                } else {
                    raw.clone()
                };
                match serde_json::from_str::<serde_json::Value>(&text) {
                    Ok(serde_json::Value::Object(map)) => args.extend(map),
                    Ok(_) => return Err("--args: expected a JSON object".into()),
                    Err(e) => return Err(format!("--args: {e}")),
                }
                i += 2;
            }
            "--json" => {
                json = true;
                i += 1;
            }
            other => {
                return Err(format!(
                    "{command}: unknown option `{other}` — the arguments go in --args as one \
                     JSON object, the same keys {tool} takes"
                ))
            }
        }
    }
    args.insert("project".into(), serde_json::Value::String(dir.to_string()));
    let args = serde_json::Value::Object(args);
    let probe = host_probe;
    let answer = match command {
        "init" => promo_author::init(&args, None),
        "upsert-layer" => promo_author::upsert_layer(&args, None, &probe),
        "upsert-keyframe" => promo_author::upsert_keyframe(&args, None),
        "apply" => promo_author::apply(&args, None),
        "slideshow" => promo_author::slideshow(&args, None, &probe),
        // With the renderer's measurements of where each bound slot lands.
        "explain" => promo_cli::placement::explain(&args, None),
        _ => promo_author::diff(&args, None),
    }?;
    Ok(if json {
        serde_json::json!({ "ok": true, "answer": answer }).to_string()
    } else {
        answer
    })
}

/// The shipped skill, the workflow layer over the tools.
const SKILL: &str = include_str!("../../skill/SKILL.md");

/// A tool that reads skills: its name, its home folder under ~, and the
/// skills folder of its own it reads when the shared one is not enough —
/// `None` when `~/.agents/skills` already covers it.
const SKILL_TOOLS: &[(&str, &str, Option<&str>)] = &[
    ("Claude Code", ".claude", Some(".claude/skills")),
    ("Codex", ".codex", None),
    ("Gemini CLI", ".gemini", None),
    (
        "Antigravity",
        ".gemini/antigravity",
        Some(".gemini/antigravity/skills"),
    ),
    (
        "Antigravity",
        ".gemini/config",
        Some(".gemini/config/skills"),
    ),
    ("Cursor", ".cursor", None),
    ("GitHub Copilot", ".copilot", None),
    ("Grok Build", ".grok", None),
];

/// `promo skill` prints the skill; `promo skill install [--home DIR]
/// [--dry-run]` writes `<folder>/promoshot/SKILL.md` into the Agent Skills
/// standard's shared home, `~/.agents/skills` — read by Codex, Gemini CLI,
/// Cursor, GitHub Copilot and Grok Build — and into the own folder of each
/// tool found that reads only its own: Claude Code (`~/.claude/skills`),
/// Antigravity (`~/.gemini/config/skills`; older builds
/// `~/.gemini/antigravity/skills`). A tool is found by its home folder.
/// The app's installer is the twin.
fn skill(rest: &[String]) -> Result<(), String> {
    if rest.first().map(String::as_str) != Some("install") {
        print!("{SKILL}");
        return Ok(());
    }
    let home = rest
        .iter()
        .position(|a| a == "--home")
        .and_then(|i| rest.get(i + 1))
        .map(PathBuf::from)
        .or_else(home_dir)
        .ok_or("no home folder to install into; pass --home DIR")?;
    let dry = rest.iter().any(|a| a == "--dry-run");
    // `--into DIR`: one skills folder of the person's choosing, for a tool
    // this list does not know or a project's own skills folder.
    let chosen = rest
        .iter()
        .position(|a| a == "--into")
        .and_then(|i| rest.get(i + 1))
        .map(PathBuf::from);
    let destinations = match chosen {
        Some(dir) => vec![("installed".to_string(), dir)],
        None => skill_destinations(&home),
    };
    if destinations.is_empty() {
        println!(
            "no agent tool found under {} — nothing written; `--into DIR` writes to a skills folder you choose",
            home.display()
        );
        return Ok(());
    }
    for (label, folder) in destinations {
        let dir = folder.join("promoshot");
        if !dry {
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            std::fs::write(dir.join("SKILL.md"), SKILL)
                .map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        println!("{label}: {}", dir.display());
    }
    Ok(())
}

fn skill_destinations(home: &Path) -> Vec<(String, PathBuf)> {
    let found: Vec<&(&str, &str, Option<&str>)> = SKILL_TOOLS
        .iter()
        .filter(|(_, tool_home, _)| home.join(tool_home).is_dir())
        .collect();
    let covered: Vec<&str> = found
        .iter()
        .filter(|(_, _, own)| own.is_none())
        .map(|(name, _, _)| *name)
        .collect();
    // The shared home only when a tool that reads it is here, or ~/.agents
    // already exists: no hidden folder for tools a person does not have.
    let mut out = Vec::new();
    if !covered.is_empty() {
        out.push((
            format!("shared, read by {}", covered.join(", ")),
            home.join(".agents/skills"),
        ));
    } else if home.join(".agents").is_dir() {
        out.push((
            "shared, ~/.agents already present".to_string(),
            home.join(".agents/skills"),
        ));
    }
    for (name, _, own) in found {
        if let Some(own) = own {
            out.push((name.to_string(), home.join(own)));
        }
    }
    out
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// A render's answer with the credits of what it reached (resource
/// `provenance`, docs/BEAT-SYNC.md §8): a `credits` block in the text, a
/// `credits` key in the JSON — only when a resource it drew states one.
fn with_credits(answer: String, command: &str, project: &Project, opts: &Options) -> String {
    let span = if command == "still" {
        let t = opts.time.unwrap_or(0.0);
        (t, t)
    } else if !opts.times.is_empty() {
        let lo = opts.times.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = opts.times.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        (lo, hi)
    } else {
        (
            opts.from.unwrap_or(0.0),
            opts.to.unwrap_or_else(|| project.duration()),
        )
    };
    let credits = promo_cli::credits::credits(&project.meta, Some(span));
    if credits.is_empty() {
        return answer;
    }
    if opts.json {
        if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&answer) {
            value["credits"] = promo_cli::credits::json(&credits);
            return value.to_string();
        }
    }
    format!("{answer}\ncredits:\n{}", promo_cli::credits::text(&credits))
}

/// `promo beats`: the tracker on one audio file, or on a project's audio
/// resource through the project's cache.
/// `promo midi render <project> [resource] [--bank f] [--check] [--json]`:
/// promo-midi's settle for one MIDI resource, or every one.
fn midi(args: &[String]) -> Result<String, String> {
    let usage =
        "usage: promo midi render <project-dir> [resource] [--bank <file.sf2>] [--check] [--json]\n       \
         promo midi generate <project-dir> <resource> --track <name> [--caption <text>] [--seed <n>] \
         [--range <from>-<to>] [--as strings|synth|drums|…] [--bank <file.sf2>] [--json]";
    if args.first().map(String::as_str) == Some("generate") {
        return midi_generate(&args[1..], usage);
    }
    if args.first().map(String::as_str) != Some("render") {
        return Err(usage.into());
    }
    let (mut positional, mut bank, mut check, mut json) = (Vec::new(), None, false, false);
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--bank" => {
                bank = Some(PathBuf::from(
                    args.get(i + 1).ok_or("--bank expects a .sf2 file")?,
                ));
                i += 1;
            }
            "--check" => check = true,
            "--json" => json = true,
            other if other.starts_with("--") => {
                return Err(format!("midi: unknown option `{other}`\n{usage}"))
            }
            other => positional.push(other.to_string()),
        }
        i += 1;
    }
    let dir = PathBuf::from(positional.first().ok_or(usage)?);
    let text = std::fs::read_to_string(dir.join("metadata.json"))
        .map_err(|_| format!("no metadata.json in {}", dir.display()))?;
    let doc: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("decode: {e}"))?;
    let ids = match positional.get(1) {
        Some(id) => vec![id.clone()],
        None => promo_midi::used_midi(&doc),
    };
    if ids.is_empty() {
        return Ok(
            "no MIDI resource a layer uses — nothing to render (name one to render it anyway)"
                .into(),
        );
    }
    let bank = if check {
        promo_midi::bank(bank.as_deref()).ok()
    } else {
        Some(promo_midi::bank(bank.as_deref())?)
    };
    let mut answers = Vec::new();
    let mut lines = Vec::new();
    for id in &ids {
        if check {
            let sha = bank.as_ref().map(|b| b.sha256.clone()).unwrap_or_else(|| {
                doc["resources"]
                    .as_array()
                    .and_then(|l| l.iter().find(|r| r["id"] == id.as_str()))
                    .and_then(|r| r["midi"]["bank"]["sha256"].as_str())
                    .unwrap_or_default()
                    .to_string()
            });
            let why = promo_midi::stale(&doc, &dir.join("Resources"), id, &sha)?;
            lines.push(match &why {
                None => format!("{id}: current"),
                Some(why) => format!("{id}: not current — {why}"),
            });
            answers
                .push(serde_json::json!({ "resource": id, "current": why.is_none(), "why": why }));
        } else {
            let bank = bank.as_ref().ok_or("no bank")?;
            let settled = promo_midi::settle(&dir, id, bank)?;
            lines.push(settled.report());
            answers.push(settled.to_json());
        }
    }
    Ok(if json {
        serde_json::Value::Array(answers).to_string()
    } else {
        lines.join("\n")
    })
}

/// `promo midi generate`: one generated layer over the MIDI (MIDI.md §7),
/// progress on stderr.
fn midi_generate(args: &[String], usage: &str) -> Result<String, String> {
    use promo_midi::generate::{self, Progress, Request};
    let mut positional = Vec::new();
    let (mut track, mut caption, mut seed, mut range) = (None, None, None, None);
    let (mut track_name, mut bank) = (None, None);
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        let value = |i: usize| {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{} expects a value\n{usage}", args[i]))
        };
        match args[i].as_str() {
            "--track" => track = Some(value(i)?),
            "--caption" => caption = Some(value(i)?),
            "--seed" => {
                seed = Some(
                    value(i)?
                        .parse::<i64>()
                        .map_err(|_| "--seed expects a whole number")?,
                )
            }
            "--range" => {
                let text = value(i)?;
                let (from, to) = text
                    .split_once(['-', ':', ','])
                    .and_then(|(a, b)| {
                        Some((a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?))
                    })
                    .ok_or("--range expects bars as <from>-<to> (1-based, `to` exclusive)")?;
                range = Some((from, to));
            }
            "--as" => track_name = Some(value(i)?),
            "--bank" => bank = Some(PathBuf::from(value(i)?)),
            "--json" => {
                json = true;
                i += 1;
                continue;
            }
            other if other.starts_with("--") => {
                return Err(format!("midi generate: unknown option `{other}`\n{usage}"))
            }
            other => {
                positional.push(other.to_string());
                i += 1;
                continue;
            }
        }
        i += 2;
    }
    let (Some(dir), Some(resource)) = (positional.first(), positional.get(1)) else {
        return Err(usage.into());
    };
    let track = track.ok_or_else(|| format!("--track names the layer\n{usage}"))?;
    let bank = promo_midi::bank(bank.as_deref())?;
    let mut request = Request::new(&track);
    request.caption = caption;
    request.seed = seed;
    request.range = range;
    request.track_name = track_name;
    // The native model, when its weights are installed (kept loaded: this
    // process ends after the one generation).
    #[cfg(feature = "music")]
    promo_music::install_generator(true);
    let model = generate::generator()?;
    struct Stderr(std::time::Instant);
    impl Progress for Stderr {
        fn phase(&mut self, phase: &str) {
            eprintln!(
                "generate: {phase} ({:.1} s)",
                self.0.elapsed().as_secs_f64()
            );
        }
    }
    let (made, settled) = generate::generate(
        Path::new(dir),
        resource,
        &request,
        model.as_ref(),
        &bank,
        &mut Stderr(std::time::Instant::now()),
    )?;
    Ok(if json {
        serde_json::json!({ "generated": made.to_json(), "result": settled.to_json() }).to_string()
    } else {
        format!("{}\n{}", made.report(), settled.report())
    })
}

/// Settles every MIDI resource a layer uses (nested compositions too)
/// whose take is not current, writing metadata.json once; with no bank,
/// refuses and says how to install it. Answers one line per take settled.
fn settle_used_midi(dir: &Path) -> Result<Vec<String>, String> {
    let meta_path = dir.join("metadata.json");
    let Ok(text) = std::fs::read_to_string(&meta_path) else {
        return Ok(Vec::new());
    };
    let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Ok(Vec::new());
    };
    let used = promo_midi::used_midi(&doc);
    if used.is_empty() {
        return Ok(Vec::new());
    }
    let resources = dir.join("Resources");
    let bank = promo_midi::bank(None);
    let mut stale = Vec::new();
    for id in &used {
        let sha = match &bank {
            Ok(bank) => bank.sha256.clone(),
            Err(_) => doc["resources"]
                .as_array()
                .and_then(|l| l.iter().find(|r| r["id"] == id.as_str()))
                .and_then(|r| r["midi"]["bank"]["sha256"].as_str())
                .unwrap_or_default()
                .to_string(),
        };
        if let Some(why) = promo_midi::stale(&doc, &resources, id, &sha)? {
            stale.push((id.clone(), why));
        }
    }
    if stale.is_empty() {
        return Ok(Vec::new());
    }
    let bank = bank.map_err(|e| {
        format!(
            "not rendering: MIDI {} not rendered ({}) — {e}",
            stale
                .iter()
                .map(|(id, _)| format!("\"{id}\""))
                .collect::<Vec<_>>()
                .join(", "),
            stale[0].1
        )
    })?;
    let mut lines = Vec::new();
    for (id, _) in &stale {
        let settled = promo_midi::settle_doc(&mut doc, &resources, id, &bank)?;
        lines.push(format!(
            "midi: {}",
            settled.report().lines().next().unwrap_or_default()
        ));
    }
    promo_midi::settle::stamp(&mut doc);
    let out = serde_json::to_string(&doc).map_err(|e| e.to_string())?;
    let temporary =
        meta_path.with_file_name(format!(".metadata.json.partial-{}", std::process::id()));
    std::fs::write(&temporary, out).map_err(|e| format!("write: {e}"))?;
    std::fs::rename(&temporary, &meta_path).map_err(|e| format!("write: {e}"))?;
    Ok(lines)
}

/// `promo music [list | install <id> | remove <id>]` (promoshot-mcp's
/// `music` door is the same function).
#[cfg(feature = "music")]
fn music(args: &[String]) -> Result<String, String> {
    promo_music::store::command("promo", args)
}

#[cfg(not(feature = "music"))]
fn music(_: &[String]) -> Result<String, String> {
    Err("this build of promo has no music model support".into())
}

/// `promo sounds [list | install <bank>]`.
fn sounds(args: &[String]) -> Result<String, String> {
    let dir = promo_midi::sounds::sounds_dir();
    match (args.first().map(String::as_str), args.get(1)) {
        (None | Some("list"), None) => {
            let mut lines = vec![format!("sound banks in {}:", dir.display())];
            for bank in promo_midi::sounds::BANKS {
                let installed = std::fs::metadata(promo_midi::sounds::bank_path(&bank, &dir))
                    .is_ok_and(|m| m.len() == bank.bytes);
                lines.push(format!(
                    "  {:<12} {:<2} {:>4.0} MB  {}  {}",
                    bank.id,
                    if installed { "✓" } else { "-" },
                    bank.bytes as f64 / 1e6,
                    bank.title,
                    bank.license
                ));
            }
            Ok(lines.join("\n"))
        }
        (Some("install"), Some(id)) => {
            let mut shown = 0u64;
            let path = promo_midi::sounds::install(id, &dir, &mut |done, total| {
                let percent = (done * 100).checked_div(total).unwrap_or(0).min(100);
                if percent >= shown + 10 {
                    shown = percent;
                    eprintln!("{id}: {percent}% of {:.0} MB", total as f64 / 1e6);
                }
            })?;
            Ok(format!(
                "{id}: installed and verified at {}",
                path.display()
            ))
        }
        _ => Err("usage: promo sounds [list | install generaluser]".into()),
    }
}

fn beats(args: &[String]) -> Result<String, String> {
    use promo_media::beats::{self, BeatOptions};
    let mut path: Option<PathBuf> = None;
    let mut resource: Option<String> = None;
    let mut json = false;
    let mut refresh = false;
    let mut clear_hint = false;
    let mut options = BeatOptions::default();
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let value = || {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{flag}: expected a value"))
        };
        match flag {
            "--json" => json = true,
            "--refresh" => refresh = true,
            "--clear-hint" => clear_hint = true,
            "--resource" => {
                resource = Some(value()?);
                i += 1;
            }
            "--meter" => {
                let n = parse_f64(&value()?, flag)?.round();
                if !(1.0..=16.0).contains(&n) {
                    return Err("--meter: beats per bar, 1 to 16".into());
                }
                options.meter = n as u32;
                i += 1;
            }
            "--bpm-hint" => {
                options.bpm_hint = Some(parse_f64(&value()?, flag)?);
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("beats: unknown option `{other}`\n\n{USAGE}"))
            }
            other => path = Some(PathBuf::from(other)),
        }
        i += 1;
    }
    let path =
        path.ok_or_else(|| format!("beats: expected an audio file or a project\n\n{USAGE}"))?;
    let beats_err = |e: String| format!("beats: {e}");
    if clear_hint && options.bpm_hint.is_some() {
        promo_model::beat_cache::wanted_hint(None, options.bpm_hint, true).map_err(beats_err)?;
    }
    // A song that states its own grid counts on that grid, not the measure.
    let mut stated: Option<promo_model::musical::BeatGrid> = None;
    let (analysis, cached) = if path.is_dir() {
        let text = std::fs::read_to_string(path.join("metadata.json"))
            .map_err(|e| format!("{}: {e}", path.join("metadata.json").display()))?;
        let mut meta = promo_model::ProjectMetadata::from_json(&text).map_err(|e| e.to_string())?;
        let chosen =
            promo_model::beat_cache::choose_song(&meta, resource.as_deref()).map_err(beats_err)?;
        let file = promo_model::beat_cache::song_file(&path, chosen);
        // The hint is the song's: a new one is kept on it (bpmHint), so
        // every later measure — any tool's — hears the same tempo; none
        // given, the kept one steers this measure too.
        let chosen_id = chosen.id.clone();
        let kept = chosen.bpm_hint;
        stated = chosen.beat_grid.clone();
        let wanted_hint = promo_model::beat_cache::wanted_hint(kept, options.bpm_hint, clear_hint)
            .map_err(beats_err)?;
        if wanted_hint != kept {
            if let Some(resource) = meta
                .resources
                .iter_mut()
                .flatten()
                .find(|r| r.id == chosen_id)
            {
                resource.bpm_hint = wanted_hint;
            }
            promo_author::write_metadata(&meta, &path.join("metadata.json"))?;
        }
        options.bpm_hint = wanted_hint;
        let (analysis, cached) = beats::analysis_for(&path, &file, options, refresh)?;
        (analysis, Some(cached))
    } else {
        (beats::analyse_file(&path, options)?, None)
    };
    if json {
        return Ok(
            promo_model::beat_cache::beats_answer(&analysis, cached, stated.as_ref())?.to_string(),
        );
    }
    let drift = stated
        .as_ref()
        .map(|grid| promo_model::beat_cache::hand_grid_drift(&analysis, grid));
    if let (Some(grid), Some(drift)) = (&stated, drift) {
        return Ok(format!(
            "{:.3} BPM from the song's hand beatGrid (first downbeat {:.3} s) — the grid in use; \
             the song itself measures {:.3} BPM ({} grid, confidence {:.2}), and the hand grid's \
             beats come up to {:.1} ms from the measured ones over the song",
            grid.bpm,
            grid.offset.unwrap_or(0.0),
            analysis.bpm,
            analysis.grid,
            analysis.confidence,
            drift * 1000.0
        ));
    }
    let sections: Vec<String> = analysis
        .sections
        .iter()
        .map(|s| {
            format!(
                "{} bars {}–{}",
                s.label,
                s.start_bar,
                s.start_bar + s.bars - 1
            )
        })
        .collect();
    Ok(format!(
        "{:.3} BPM{}, {}/{}, first downbeat {:.3} s, {} grid, confidence {:.2} — {} beats, {} bars{}{}",
        analysis.bpm,
        analysis
            .bpm_hint
            .map(|h| format!(" (hint {h} BPM)"))
            .unwrap_or_default(),
        analysis.time_signature[0],
        analysis.time_signature[1],
        analysis.offset,
        analysis.grid,
        analysis.confidence,
        analysis.beats.len(),
        analysis.downbeats.len(),
        if sections.is_empty() {
            String::new()
        } else {
            format!("; sections: {}", sections.join(", "))
        },
        match cached {
            Some(true) => " (from the project's cache)",
            Some(false) => " (analysed now, cached in the project)",
            None => "",
        }
    ))
}

/// `promo words`: a take's word timing (docs/SPEECH-SYNC.md §9). On a
/// project: the take's record by the cache's rules — authoritative provider
/// timing as it is, the take aligned (and stored) when it has none, an
/// older aligner's record aligned again; `--refresh` aligns again;
/// `--no-analyse` reads the cache only, else answers an estimate. On an
/// audio file: the file aligned to `--script`. `--transcribe` asks a
/// provider, on the person's key, only when asked (`--check` says the cost
/// and spends nothing). `--timeline` adds the times on the output timeline
/// through the first layer that plays the take; `--prune` removes records
/// no file in Resources/ has.
fn words(args: &[String]) -> Result<String, String> {
    use promo_model::speech::TakeTiming;
    let mut path: Option<PathBuf> = None;
    let mut resource: Option<String> = None;
    let mut script: Option<String> = None;
    let mut transcribe: Option<String> = None;
    let mut import: Option<PathBuf> = None;
    let mut provider: Option<String> = None;
    let (mut json, mut refresh, mut no_analyse, mut timeline, mut check, mut prune) =
        (false, false, false, false, false, false);
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let value = || {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{flag}: expected a value"))
        };
        match flag {
            "--json" => json = true,
            "--refresh" => refresh = true,
            "--no-analyse" => no_analyse = true,
            "--timeline" => timeline = true,
            "--check" => check = true,
            "--prune" => prune = true,
            "--resource" => {
                resource = Some(value()?);
                i += 1;
            }
            "--script" => {
                script = Some(value()?);
                i += 1;
            }
            "--script-file" => {
                let file = value()?;
                script = Some(std::fs::read_to_string(&file).map_err(|e| format!("{file}: {e}"))?);
                i += 1;
            }
            "--import" => {
                import = Some(PathBuf::from(value()?));
                i += 1;
            }
            "--provider" => {
                provider = Some(value()?);
                i += 1;
            }
            "--transcribe" => {
                let provider = value()?;
                if !promo_speech::transcribe::TRANSCRIBERS.contains(&provider.as_str()) {
                    return Err(format!("--transcribe: `{provider}` — openai or elevenlabs"));
                }
                transcribe = Some(provider);
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("words: unknown option `{other}`\n\n{USAGE}"))
            }
            other => path = Some(PathBuf::from(other)),
        }
        i += 1;
    }
    let path =
        path.ok_or_else(|| format!("words: expected an audio file or a project\n\n{USAGE}"))?;
    let words_err = |e: String| format!("words: {e}");
    if check && transcribe.is_none() {
        return Err("words: --check goes with --transcribe (it says what one would cost)".into());
    }
    if provider.is_some() && import.is_none() {
        return Err("words: --provider names who timed an --import".into());
    }
    if import.is_some() && (transcribe.is_some() || refresh || no_analyse) {
        return Err(
            "words: --import is the take's timing — not with --transcribe, --refresh or \
             --no-analyse"
                .into(),
        );
    }
    // Word timings a voice handed over (issue #16): read and matched onto
    // the script, as the provider's.
    let imported = |said: &str, seconds: f64| -> Result<TakeTiming, String> {
        let file = import.as_ref().expect("asked");
        let text =
            std::fs::read_to_string(file).map_err(|e| format!("words: {}: {e}", file.display()))?;
        promo_speech::timing::imported_record(said, &text, provider.as_deref(), seconds)
            .map_err(|e| format!("words: {}: {e}", file.display()))
    };
    let probe = |file: &Path| -> Result<f64, String> {
        promo_media::probe_stream(file)
            .map(|info| info.duration_s)
            .map_err(|e| format!("words: {}: {e}", file.display()))
    };
    // A transcription: the cost first (and only that, with --check), then
    // the person's key, the request, the record mapped onto the script.
    let transcribed =
        |file: &Path, said: &str, seconds: f64| -> Result<Result<TakeTiming, String>, String> {
            let provider = transcribe.as_deref().expect("asked");
            let (usd, how) =
                promo_speech::transcribe::cost(provider, seconds).map_err(words_err)?;
            let keys = promo_speech::SystemKeys;
            let key = promo_speech::KeyStore::key(&keys, provider);
            if check {
                return Ok(Err(format!(
                    "transcribing {} ({seconds:.1} s) with {provider} would cost about ${usd:.4} \
                 ({how}); key: {} — nothing was spent",
                    file.display(),
                    if key.is_some() { "found" } else { "none" }
                )));
            }
            let (key, _) = key.ok_or_else(|| promo_speech::keys::missing_key_message(provider))?;
            let audio = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
            let name = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let spoken =
                promo_speech::transcribe::transcribe_with_key(provider, &audio, &name, said, &key)?;
            let mut timing = promo_speech::timing::record(said, &spoken, seconds);
            timing.extra.insert(
                "cost".into(),
                serde_json::json!({"usd": (usd * 1e6).round() / 1e6, "how": how}),
            );
            Ok(Ok(timing))
        };
    // (record, fromCache, stale, notes, project)
    let mut notes: Vec<String> = Vec::new();
    let mut project: Option<(promo_model::ProjectMetadata, String)> = None;
    let (timing, cached, stale): (TakeTiming, Option<bool>, bool) = if path.is_dir() {
        if script.is_some() {
            return Err(
                "words: a project's take says its own script (speech.text or transcript) \
                 — --script is for an audio file"
                    .into(),
            );
        }
        let text = std::fs::read_to_string(path.join("metadata.json"))
            .map_err(|e| format!("{}: {e}", path.join("metadata.json").display()))?;
        let meta = promo_model::ProjectMetadata::from_json(&text).map_err(|e| e.to_string())?;
        if prune {
            return Ok(prune_words(&path, &meta));
        }
        let chosen =
            promo_model::word_cache::choose_take(&meta, resource.as_deref()).map_err(words_err)?;
        let take = promo_model::word_cache::Take {
            id: chosen.id.clone(),
            file: promo_model::beat_cache::song_file(&path, chosen),
            script: promo_model::speech::take_script(chosen).unwrap_or_default(),
        };
        let fixes = promo_model::speech::word_fixes(chosen).map_err(words_err)?;
        let length = || {
            chosen
                .duration
                .filter(|d| *d > 0.0)
                .map(Ok)
                .unwrap_or_else(|| probe(&take.file))
        };
        let language = promo_cli::project::Project::take_language(&meta, &take.id);
        let (mut timing, cached, stale) = if import.is_some() {
            let mut timing = imported(&take.script, length()?)?;
            if let Ok(source) = promo_model::media_cache::source_of(&take.file) {
                timing.audio = source;
            }
            let timing = promo_model::word_cache::store(&path, &take.file, timing)?;
            notes.push(format!(
                "imported as {} timing — it stands until the take changes",
                timing.source.as_text()
            ));
            (timing, false, false)
        } else if transcribe.is_some() {
            match transcribed(&take.file, &take.script, length()?)? {
                Err(said) => return Ok(said),
                Ok(timing) => {
                    let timing = promo_model::word_cache::store(&path, &take.file, timing)?;
                    (timing, false, false)
                }
            }
        } else if no_analyse || !take.file.is_file() {
            match take
                .file
                .is_file()
                .then(|| promo_model::word_cache::lookup(&path, &take.file, &take.script))
                .flatten()
            {
                Some(found) => (found.timing, true, found.stale),
                None => {
                    let mut timing = promo_speech::estimate_words(
                        &take.script,
                        length()?,
                        language.as_deref(),
                        1.0,
                    );
                    if let Ok(source) = promo_model::media_cache::source_of(&take.file) {
                        timing.audio = source;
                    }
                    notes.push(
                        "no word timing stored for this take yet — an estimate, not stored".into(),
                    );
                    (timing, false, false)
                }
            }
        } else {
            let options = promo_media::words::AlignOptions { language };
            let timed = promo_media::words::timing_for(&path, &take, &options, refresh)
                .map_err(words_err)?;
            if timed.realigned {
                notes.push(format!(
                    "aligned again with aligner {}",
                    promo_media::words::ALIGNER_VERSION
                ));
            }
            (timed.timing, timed.from_cache, timed.stale)
        };
        notes.extend(promo_model::speech::apply_fixes(&mut timing, &fixes));
        project = Some((meta, take.id.clone()));
        (timing, Some(cached), stale)
    } else {
        let said = script.ok_or_else(|| {
            "words: an audio file needs its script: --script <text> or --script-file <f>"
                .to_string()
        })?;
        if prune || timeline {
            return Err("words: --prune and --timeline need a project".into());
        }
        let timing = if import.is_some() {
            let mut timing = imported(&said, probe(&path)?)?;
            timing.audio = promo_model::media_cache::source_of(&path).map_err(words_err)?;
            timing
        } else if transcribe.is_some() {
            match transcribed(&path, &said, probe(&path)?)? {
                Err(said) => return Ok(said),
                Ok(timing) => timing,
            }
        } else if no_analyse {
            let mut timing = promo_speech::estimate_words(&said, probe(&path)?, None, 1.0);
            timing.audio = promo_model::media_cache::source_of(&path).map_err(words_err)?;
            timing
        } else {
            promo_media::words::align_file(&path, &said, &Default::default()).map_err(words_err)?
        };
        (timing, None, false)
    };
    let mapped = match (&project, timeline) {
        (Some((meta, id)), true) => {
            Some(promo_timeline::speech::timeline_words(meta, id, &timing).map_err(words_err)?)
        }
        _ => None,
    };
    if json {
        let mut value = promo_model::word_cache::words_answer(&timing, cached, stale)?;
        if !notes.is_empty() {
            value["notes"] = serde_json::json!(notes);
        }
        if let Some((layer, times)) = &mapped {
            value["timeline"] = promo_timeline::speech::timeline_json(layer, times);
        }
        return Ok(value.to_string());
    }
    let mut table = words_table(&timing, cached, stale, &notes);
    if let Some((layer, times)) = &mapped {
        table.push_str(&format!("\non the timeline, through layer \"{layer}\":"));
        for (k, (w, t)) in timing.words.iter().zip(times).enumerate() {
            table.push_str(&match t {
                Some((s, e)) => format!("\n{:>4}  {s:>8.3}  {e:>8.3}  {}", k + 1, w.text),
                None => format!("\n{:>4}  trimmed out  {}", k + 1, w.text),
            });
        }
    }
    Ok(table)
}

/// Removes the word records of `.words/` whose audio no file in
/// Resources/ has (stand-ins voiced since, takes deleted).
fn prune_words(dir: &Path, meta: &promo_model::ProjectMetadata) -> String {
    let resources = dir.join("Resources");
    let mut kept: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&resources) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Ok(source) = promo_model::media_cache::source_of(&path) {
                    kept.push(source.sha256);
                }
            }
        }
    }
    for resource in meta.resources.iter().flatten() {
        let file = promo_model::beat_cache::song_file(dir, resource);
        if file.is_file() {
            if let Ok(source) = promo_model::media_cache::source_of(&file) {
                kept.push(source.sha256);
            }
        }
    }
    let mut removed = 0;
    if let Ok(entries) = std::fs::read_dir(promo_model::word_cache::cache_dir(dir)) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let audio = name.split('-').next().unwrap_or_default().to_string();
            if name.ends_with(".json")
                && !kept.contains(&audio)
                && std::fs::remove_file(entry.path()).is_ok()
            {
                removed += 1;
            }
        }
    }
    format!("removed {removed} word record(s) whose audio no file has")
}

/// `promo words`' table: a line of what timed the take, the words 1-based
/// (`#  start  end  conf  word`), then the sentences.
fn words_table(
    timing: &promo_model::speech::TakeTiming,
    cached: Option<bool>,
    stale: bool,
    notes: &[String],
) -> String {
    let how = match &timing.source {
        promo_model::speech::TimingSource::Estimate => {
            "estimated from the script's syllables".to_string()
        }
        promo_model::speech::TimingSource::Provider(p) => format!("timed by {p}"),
        promo_model::speech::TimingSource::Local(v) => format!("timed by the local voice {v}"),
        promo_model::speech::TimingSource::Aligned => "aligned to the audio".to_string(),
        promo_model::speech::TimingSource::Transcribed(p) => format!("transcribed by {p}"),
        promo_model::speech::TimingSource::Other(s) => s.clone(),
    };
    let whence = match cached {
        Some(true) => " (from the project's cache)",
        Some(false) if timing.estimated => "",
        Some(false) => " (timed now, stored in the project's cache)",
        None => "",
    };
    let mut out = vec![format!(
        "{how}, confidence {:.2} — {} words, {} sentences over {:.3} s{whence}",
        timing.confidence,
        timing.words.len(),
        timing.sentences.len(),
        timing.duration
    )];
    if stale {
        out.push(
            "the take's text changed since it was voiced — these times follow the old take"
                .to_string(),
        );
    }
    out.extend(notes.iter().cloned());
    out.push(format!(
        "{:>4}  {:>8}  {:>8}  {:>4}  word",
        "#", "start", "end", "conf"
    ));
    for (i, w) in timing.words.iter().enumerate() {
        let conf = w.confidence.map(|c| format!("{c:.2}")).unwrap_or_default();
        out.push(format!(
            "{:>4}  {:>8.3}  {:>8.3}  {:>4}  {}{}",
            i + 1,
            w.start,
            w.end,
            conf,
            w.text,
            if w.interpolated {
                "  (interpolated)"
            } else {
                ""
            }
        ));
    }
    out.push("sentences".to_string());
    let chars: Vec<char> = timing.script.chars().collect();
    for (i, s) in timing.sentences.iter().enumerate() {
        let text: String = chars
            .get(s.chars[0] as usize..(s.chars[1] as usize).min(chars.len()))
            .map(|c| c.iter().collect())
            .unwrap_or_default();
        out.push(format!(
            "{:>4}  {:.3}–{:.3}  words {}–{}  {}",
            i + 1,
            s.start,
            s.end,
            s.words[0] + 1,
            s.words[1] + 1,
            text.split_whitespace().collect::<Vec<_>>().join(" ")
        ));
    }
    out.join("\n")
}

#[derive(Default)]
struct Options {
    json: bool,
    /// Read a beat clock's song from the project's cache only, never
    /// analysing it on the way in.
    no_analyse: bool,
    /// `validate`: exit 1 when anything is reported, not only on a decode
    /// failure — for a script or an agent that gates on the exit code.
    strict: bool,
    out: Option<PathBuf>,
    count: Option<u32>,
    time: Option<f64>,
    fps: Option<f64>,
    from: Option<f64>,
    to: Option<f64>,
    /// `--from 375f`: a frame number, made seconds once the rate is known
    /// ([`Options::resolve_frames`]).
    from_frames: Option<f64>,
    to_frames: Option<f64>,
    /// `video`: seconds of video per resumable segment.
    segment: Option<f64>,
    /// The exact moments to render, when the caller knows them.
    times: Vec<f64>,
    /// N moments spread evenly across the range instead of every frame —
    /// what "a contact sheet" means, and what an agent wants.
    sample: Option<usize>,
    /// Refuse rather than render more than this many frames. A person may
    /// legitimately ask for three thousand; a tool call should not.
    cap: Option<usize>,
    /// Also tile the rendered moments into one PNG here.
    sheet: Option<PathBuf>,
    size: Option<(u32, u32)>,
    proxy: render::ProxyPolicy,
    codec: promo_media::VideoCodec,
    alpha: bool,
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
                "--strict" => {
                    opts.strict = true;
                    i += 1;
                    continue;
                }
                "--no-analyse" => {
                    opts.no_analyse = true;
                    i += 1;
                    continue;
                }
                "--out" => opts.out = Some(PathBuf::from(value()?)),
                "--count" => {
                    opts.count = Some(parse_f64(&value()?, flag)?.round().clamp(1.0, 64.0) as u32)
                }
                "--time" => opts.time = Some(parse_f64(&value()?, flag)?),
                "--fps" => opts.fps = Some(parse_f64(&value()?, flag)?),
                "--proxy" => opts.proxy = render::ProxyPolicy::parse(&value()?)?,
                "--codec" => opts.codec = promo_media::VideoCodec::parse(&value()?)?,
                "--alpha" => {
                    opts.alpha = true;

                    i += 1;
                    continue;
                }
                "--times" => {
                    let raw = value()?;
                    let mut times = Vec::new();
                    for part in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                        times.push(parse_f64(part, flag)?);
                    }
                    if times.is_empty() {
                        return Err("--times: expected one or more seconds".into());
                    }
                    opts.times = times;
                }
                "--sample" => {
                    opts.sample =
                        Some(parse_f64(&value()?, flag)?.round().clamp(1.0, 4096.0) as usize)
                }
                "--cap" => {
                    opts.cap = Some(parse_f64(&value()?, flag)?.round().clamp(1.0, 1e6) as usize)
                }
                "--sheet" => opts.sheet = Some(PathBuf::from(value()?)),
                "--from" => match parse_moment(&value()?, flag)? {
                    Moment::Seconds(s) => opts.from = Some(s),
                    Moment::Frames(f) => opts.from_frames = Some(f),
                },
                "--to" => match parse_moment(&value()?, flag)? {
                    Moment::Seconds(s) => opts.to = Some(s),
                    Moment::Frames(f) => opts.to_frames = Some(f),
                },
                "--segment" => {
                    let s = parse_f64(&value()?, flag)?;
                    if !(s.is_finite() && s > 0.0) {
                        return Err("--segment: expected seconds above zero".into());
                    }
                    opts.segment = Some(s);
                }
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

impl Options {
    /// Turns `--from/--to` frame numbers into seconds at the rate `command`
    /// renders at: a GIF's own rate for `gif`, else the export rate.
    fn resolve_frames(&mut self, project: &Project, command: &str) {
        if self.from_frames.is_none() && self.to_frames.is_none() {
            return;
        }
        let fps = if command == "gif" {
            self.fps
                .unwrap_or(project.meta.composition_settings.gif_export_fps)
                .max(1.0)
        } else {
            promo_timeline::export_fps(project.meta.composition_settings.fps, self.fps)
        };
        if let Some(f) = self.from_frames.take() {
            self.from = Some(f / fps);
        }
        if let Some(f) = self.to_frames.take() {
            self.to = Some(f / fps);
        }
    }
}

/// A `--from/--to` value: seconds, or a frame number (`375f`).
#[derive(Debug, PartialEq)]
enum Moment {
    Seconds(f64),
    Frames(f64),
}

fn parse_moment(raw: &str, flag: &str) -> Result<Moment, String> {
    let text = raw.trim();
    let bad = || format!("{flag}: expected seconds (12.5, 12.5s) or frames (375f), got `{raw}`");
    if let Some(frames) = text.strip_suffix('f') {
        let n: f64 = frames.trim().parse().map_err(|_| bad())?;
        if !(n.is_finite() && n >= 0.0 && n.fract() == 0.0) {
            return Err(bad());
        }
        return Ok(Moment::Frames(n));
    }
    let seconds = text.strip_suffix('s').unwrap_or(text);
    seconds
        .trim()
        .parse()
        .map(Moment::Seconds)
        .map_err(|_| bad())
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

/// Everything `validate` reports, in one list with one headline: the
/// core's report for the file as written (the same the app's
/// `promo_validate` serves through the FFI) and each layer whose media
/// this machine cannot open.
fn validation(project: &Project) -> promo_timeline::report::Report {
    let raw = std::fs::read_to_string(project.dir.join("metadata.json")).unwrap_or_default();
    // The CLI opens every file below with a decoder instead of trusting a
    // listing, which also catches a file that is there but cannot be read.
    let mut report = promo_timeline::report::file_report_with(&raw, None, &project.grids)
        .unwrap_or_else(|_| {
            // The open decoded this very file a moment ago; if it has changed
            // under us since, report on what was opened.
            let mut report = promo_timeline::report::Report::default();
            for problem in &project.attachment_problems {
                report.warn(problem.clone());
            }
            report.findings.extend(
                promo_timeline::report::report(
                    &project.meta,
                    &promo_timeline::report::Context {
                        layout: true,
                        ..Default::default()
                    },
                )
                .findings,
            );
            report
        });
    // A beat clock whose song could not be measured lays out on a stand-in
    // tempo: nothing renders as written.
    for problem in &project.grid_problems {
        report.breaks(problem.clone());
    }
    // A take a speech value counts on that could not be timed: why.
    for (take, why) in &project.missing_words {
        if project.timing.missing_words.contains(take) {
            report.warn(format!(
                "the word timing of \"{take}\" could not be made: {why}"
            ));
        }
    }
    // A layer whose media is gone. `inspect` has always reported these and
    // `validate` did not, so a project with a hole in it came back "ok" and
    // then rendered the hole.
    for layer in promo_model::nesting::all_layers(&project.meta) {
        match project.unsupported(layer) {
            // Audio never reaches a frame, and that is not a fault.
            None | Some(crate::project::Unsupported::Audio) => {}
            Some(why) => report.breaks(format!("layer \"{}\" will not render — {why}", layer.name)),
        }
    }
    // A MIDI resource whose take is missing or older than its block.
    if let Ok(doc) = serde_json::from_str::<serde_json::Value>(&raw) {
        for finding in promo_midi::findings(&doc, &project.dir.join("Resources")) {
            report.warn(finding);
        }
    }
    // A camera that frames its own shot and comes too close cuts its
    // subject off; the engine measures it here rather than a render later
    // (3D plan §6½, R1).
    promo_cli::framing::report_into(&mut report, project);
    // An SVG picture that will not draw as written: refused hrefs, text
    // without outlines, a font the import lacked.
    promo_cli::vectors::report_into(&mut report, project);
    report
}

fn report_answer(report: &promo_timeline::report::Report, opts: &Options) -> String {
    match (opts.json, opts.strict) {
        (true, false) => report.json().to_string(),
        (true, true) => {
            let mut json = report.json();
            json["notes"] = serde_json::json!(report.notes);
            json.to_string()
        }
        (false, false) => report.text(),
        (false, true) => report.text_with_notes(),
    }
}

#[cfg(test)]
fn validate(project: &Project, opts: &Options) -> Result<String, String> {
    Ok(report_answer(&validation(project), opts))
}

fn inspect(project: &Project, opts: &Options) -> Result<String, String> {
    Ok(promo_cli::inspect::inspect(project, opts.json, true))
}

/// What a `.glb` holds, for the decision to place it: bounds, the material
/// slots a `materials` binding may name, the clips a keyframe may play.
fn model_probe(file: &Path, opts: &Options) -> Result<String, String> {
    let bytes = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let model = promo_engine::model::Model::from_glb(&bytes).map_err(|e| e.to_string())?;
    let summary = promo_cli::sheet::model_facts(file)?;
    let triangles = summary["triangles"].as_u64().unwrap_or(0);
    if opts.json {
        return Ok(summary.to_string());
    }
    let mut out = format!(
        "model: {} — {} meshes, {} triangles, bounds radius {:.3}\n",
        file.display(),
        model.meshes.len(),
        triangles,
        model.bounds_radius
    );
    for m in model.materials.iter().filter(|m| !m.name.is_empty()) {
        out.push_str(&format!(
            "  slot \"{}\"{}{}\n",
            m.name,
            if m.base_texture.is_some() {
                " (textured)"
            } else {
                ""
            },
            if m.double_sided { " double-sided" } else { "" }
        ));
    }
    for (name, duration) in model.clip_summary() {
        out.push_str(&format!("  clip \"{name}\" {duration:.2}s\n"));
    }
    Ok(out.trim_end().to_string())
}

/// `promo media activity <video>`: the activity meter the apps feed from
/// AVAssetReader, fed from ffmpeg — the way to try it on real recordings,
/// and the headless host's way to fill a resource's `activity`. Prints the
/// track as the format stores it.
fn media(rest: &[String]) -> Result<String, String> {
    use promo_media::activity::{Masks, MeterOptions};
    let usage = "media: expected `media activity <video> [--rate <n>] [--pointer <pointer.json>] \
                 [--masks auto|phone|desktop|none]`";
    let (verb, file) = match rest {
        [verb, file, ..] if !file.starts_with("--") => (verb.as_str(), file),
        _ => return Err(usage.into()),
    };
    if verb != "activity" {
        return Err(format!("media: unknown verb `{verb}`\n{usage}"));
    }
    let mut options = MeterOptions::default();
    let mut i = 2;
    while i < rest.len() {
        let flag = rest[i].as_str();
        let value = rest
            .get(i + 1)
            .ok_or_else(|| format!("{flag}: expected a value"))?;
        match flag {
            "--rate" => {
                let rate = parse_f64(value, flag)?;
                if !(rate > 0.0 && rate <= 120.0) {
                    return Err(format!(
                        "--rate: expected 0–120 frames a second, got {rate}"
                    ));
                }
                options.rate = rate;
            }
            // The recorder's track as it sits beside the file, or a whole
            // resource object that carries one under `pointer`.
            "--pointer" => {
                let text = std::fs::read_to_string(value)
                    .map_err(|e| format!("--pointer {value}: {e}"))?;
                let json: serde_json::Value =
                    serde_json::from_str(&text).map_err(|e| format!("--pointer {value}: {e}"))?;
                let track = json.get("pointer").cloned().unwrap_or(json);
                options.pointer = Some(
                    serde_json::from_value(track)
                        .map_err(|e| format!("--pointer {value}: not a pointer track: {e}"))?,
                );
            }
            "--masks" => {
                options.masks = match value.as_str() {
                    "auto" => Masks::Auto,
                    "phone" => Masks::Phone,
                    "desktop" => Masks::Desktop,
                    "none" => Masks::None,
                    other => return Err(format!("--masks: unknown `{other}`")),
                }
            }
            other => return Err(format!("media activity: unknown option `{other}`\n{usage}")),
        }
        i += 2;
    }
    let track =
        promo_media::measure_activity(Path::new(file), options).map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&track).map_err(|e| e.to_string())
}

/// A built-in device body — phone, tablet, tablet-portrait or laptop — written as a `.glb`
/// with `Body` and `Screen` slots (the laptop a `Deck` too), the same
/// bytes the app and the MCP put into a project's Resources.
fn device(kind: &str, opts: &Options) -> Result<String, String> {
    let out = opts.out()?;
    let kind = promo_engine::model::DeviceKind::parse(kind).ok_or_else(|| {
        format!("device: `{kind}` is not phone, tablet, tablet-portrait or laptop")
    })?;
    let bytes = promo_engine::model::device_glb(kind);
    std::fs::write(out, &bytes).map_err(|e| format!("{}: {e}", out.display()))?;
    let model = promo_engine::model::Model::from_glb(&bytes).map_err(|e| e.to_string())?;
    if opts.json {
        return Ok(serde_json::json!({
            "wrote": out.display().to_string(),
            "kind": kind.name(),
            "boundsRadius": model.bounds_radius,
            "slots": model.slot_names(),
            "clips": [],
        })
        .to_string());
    }
    Ok(format!(
        "wrote {} ({} — slots {})",
        out.display(),
        kind.name(),
        model.slot_names().join(", ")
    ))
}

/// The model seen from around — `promo_cli::sheet::turntable`, the one
/// implementation the headless server and the Mac app's server both run.
fn turntable(file: &Path, opts: &Options) -> Result<String, String> {
    let out = opts.out()?;
    let answer = promo_cli::sheet::turntable(
        file,
        out,
        opts.count.map(|c| c as usize),
        opts.size.map(|(w, _)| w),
    )?;
    if opts.json {
        return Ok(answer.to_string());
    }
    Ok(format!(
        "wrote {} ({} yaws, {}x{} cells of {}px)",
        out.display(),
        answer["cells"].as_array().map_or(0, Vec::len),
        answer["columns"],
        answer["rows"],
        answer["cell"]
    ))
}

fn still(project: &Project, opts: &Options) -> Result<String, String> {
    let out = opts.out()?;
    let (w, h) = opts.size(project);
    let time = opts.time.unwrap_or(0.0);
    // A moment past the end renders an empty frame; refuse it by the rule
    // a look and the app's still share.
    let whole = promo_timeline::export_plan(&project.meta, None, None, None);
    promo_timeline::look_times(&[time], None, &whole, project.duration(), None)?;
    let mut renderer = render::Renderer::with_proxy(project, w, h, opts.proxy)?;
    renderer.set_transparent_plate(opts.alpha);
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
    // The moments, picked by the rule both servers' promo_render_frames
    // share: times in order, a sample over the range, or every frame.
    let plan = promo_timeline::export_plan(&project.meta, opts.fps, opts.from, opts.to);
    let times = promo_timeline::look_times(
        &opts.times,
        opts.sample,
        &plan,
        project.duration(),
        opts.cap,
    )?;
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    // This tool's own previous frames go first. Without it a shorter run
    // leaves the tail of a longer one behind and the next reader — a person
    // scrubbing the folder, or ffmpeg globbing it — sees two renders mixed.
    let replaced = clear_frames(out);

    let mut renderer = render::Renderer::with_proxy(project, w, h, opts.proxy)?;
    renderer.set_transparent_plate(opts.alpha);
    let count = times.len();
    // A contact sheet's cells are small on purpose: it is for catching an
    // off-centre card or an empty frame, not for reading type.
    let (cell_w, cell_h) = sheet_cell(w, h);
    let mut cells: Vec<Vec<u8>> = Vec::new();
    let sheet_of = sheet_sample(count);
    for (i, &time) in times.iter().enumerate() {
        let rgba = renderer.frame_rgba(time)?;
        let path = out.join(format!("frame-{i:05}.png"));
        render::write_png(&path, &rgba, w, h)?;
        if opts.sheet.is_some() && sheet_of.contains(&i) {
            cells.push(downscale(&rgba, w, h, cell_w, cell_h));
        }
        if i % 30 == 0 || i + 1 == count {
            eprint!("\r  {}/{count} frames", i + 1);
        }
    }
    eprintln!();
    let mut sheet_note = None;
    if let Some(path) = &opts.sheet {
        let (columns, rows) = grid_for(cells.len());
        // A gutter, on a ground darker than any canvas: cells read as cells
        // rather than as one smeared picture, and an unfilled slot at the
        // end reads as empty rather than as a hole.
        const GAP: u32 = 6;
        let (sw, sh) = (
            columns as u32 * (cell_w + GAP) + GAP,
            rows as u32 * (cell_h + GAP) + GAP,
        );
        let mut sheet = Vec::with_capacity((sw * sh * 4) as usize);
        for _ in 0..(sw * sh) {
            sheet.extend_from_slice(&[18, 20, 26, 255]);
        }
        for (i, cell) in cells.iter().enumerate() {
            let (column, row) = ((i % columns) as u32, (i / columns) as u32);
            blit(
                &mut sheet,
                sw,
                cell,
                cell_w,
                cell_h,
                GAP + column * (cell_w + GAP),
                GAP + row * (cell_h + GAP),
            );
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        render::write_png(path, &sheet, sw, sh)?;
        sheet_note = Some((path.display().to_string(), columns, rows, cells.len()));
    }
    if opts.json {
        return Ok(serde_json::json!({
            "wroteDir": out.display().to_string(),
            "frames": count, "width": w, "height": h,
            "fps": plan.fps, "from": plan.start, "to": plan.end,
            "replaced": replaced,
            "times": times.iter().map(|t| (t * 1000.0).round() / 1000.0).collect::<Vec<f64>>(),
            "sheet": sheet_note.as_ref().map(|(p, c, r, n)| serde_json::json!({
                "path": p, "columns": c, "rows": r, "cells": n,
            })),
        })
        .to_string());
    }
    let mut text = format!("wrote {count} frames to {} ({w}x{h})", out.display());
    if replaced > 0 {
        text.push_str(&format!(", replacing {replaced}"));
    }
    if let Some((path, columns, rows, n)) = sheet_note {
        text.push_str(&format!(
            "\ncontact sheet {path} ({columns}x{rows}, {n} cells)"
        ));
    }
    Ok(text)
}

/// The moments to render: exactly what was asked for, N spread evenly, or
/// every frame at the rate — in that order of precedence.
/// Deletes this tool's own `frame-NNNNN.png` from a directory, and answers
/// how many. Nothing else is touched — a person's own files in the folder
/// are not this tool's to remove.
fn clear_frames(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let is_frame = name.starts_with("frame-")
            && name.ends_with(".png")
            && name["frame-".len()..name.len() - 4]
                .chars()
                .all(|c| c.is_ascii_digit());
        if is_frame && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// A sheet cell, long edge 256px, keeping the canvas's proportions.
fn sheet_cell(w: u32, h: u32) -> (u32, u32) {
    let long = w.max(h).max(1) as f64;
    let scale = (256.0 / long).min(1.0);
    (
        ((w as f64 * scale).round() as u32).max(1),
        ((h as f64 * scale).round() as u32).max(1),
    )
}

/// Which of `count` frames go on the sheet: all of them up to 24, else 24
/// spread evenly. A sheet of two hundred thumbnails shows nothing.
fn sheet_sample(count: usize) -> Vec<usize> {
    const MOST: usize = 24;
    if count <= MOST {
        return (0..count).collect();
    }
    (0..MOST).map(|i| i * (count - 1) / (MOST - 1)).collect()
}

/// Straight RGBA box-resize, through `image`'s triangle filter.
fn downscale(rgba: &[u8], w: u32, h: u32, dw: u32, dh: u32) -> Vec<u8> {
    let Some(src) = image::RgbaImage::from_raw(w, h, rgba.to_vec()) else {
        return vec![0; (dw * dh * 4) as usize];
    };
    image::imageops::resize(&src, dw, dh, image::imageops::FilterType::Triangle).into_raw()
}

/// Renders straight into ffmpeg's stdin as raw BGRA — no intermediate PNGs,
/// no temp directory the size of the uncompressed video.
///
/// ffmpeg is invoked as a separate program, not linked, so this borrows
/// nothing from its licence.
fn video(project: &Project, opts: &Options) -> Result<String, String> {
    let out = opts.out()?;
    // The project's export size unless --size says otherwise — what the
    // apps export at (a 16:9 canvas into a 1080x1920 file, with bars).
    let (w, h) = opts
        .size
        .unwrap_or_else(|| promo_timeline::export_size(&project.meta));
    let (start, end, fps) = range(project, opts);
    let count = frame_count(start, end, fps);

    // The export itself lives in the lib (render::export_video), because
    // the app exports through the same code over the FFI — the CLI only
    // narrates it.
    let settings = render::ExportSettings {
        width: w,
        height: h,
        codec: opts.codec,
        alpha: opts.alpha,
        start,
        end,
        fps,
        // The CLI is the oracle and renders clean; watermark policy is the
        // apps' concern.
        overlay: None,
    };
    let started = std::time::Instant::now();
    let mut resumed_frames = 0;
    promo_cli::resume::export_video_resumable(
        project,
        out,
        &settings,
        opts.segment
            .unwrap_or(promo_cli::resume::DEFAULT_SEGMENT_SECONDS),
        opts.proxy,
        &mut |event| {
            use promo_cli::resume::Event;
            match event {
                Event::Plan {
                    frames,
                    segments,
                    done_segments,
                    done_frames,
                    restarted,
                    work,
                } => {
                    if let Some(why) = restarted {
                        eprintln!("  starting over: {why}");
                    }
                    if done_segments > 0 {
                        resumed_frames = done_frames;
                        eprintln!(
                            "  resuming: {done_segments} of {segments} segments already \
                             rendered ({done_frames} of {frames} frames) in {}",
                            work.display()
                        );
                    } else {
                        eprintln!(
                            "  {frames} frames in {segments} segment{}; a killed render \
                             resumes from {}",
                            if segments == 1 { "" } else { "s" },
                            work.display()
                        );
                    }
                }
                Event::Frame { done, total } => {
                    if done % 30 == 0 || done == total {
                        let rendered = done - resumed_frames;
                        let secs = started.elapsed().as_secs_f64();
                        let rate = if secs > 0.0 {
                            rendered as f64 / secs
                        } else {
                            0.0
                        };
                        eprint!(
                            "\r  {done}/{total} frames ({:.0}%, {rate:.1} fps)",
                            100.0 * done as f64 / total.max(1) as f64
                        );
                    }
                }
                Event::Segment { .. } => {}
                Event::Joining { segments } => {
                    eprintln!();
                    eprintln!(
                        "  joining {segments} segment{} and the soundtrack",
                        if segments == 1 { "" } else { "s" }
                    );
                }
            }
            true
        },
    )?;

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
/// and no external tool. The project's own GIF rate (10 unless set): a GIF
/// is a preview, not a master.
fn gif(project: &Project, opts: &Options) -> Result<String, String> {
    let out = opts.out()?;
    let (w, h) = opts.size(project);
    let start = opts.from.unwrap_or(0.0).max(0.0);
    let end = opts.to.unwrap_or_else(|| project.duration()).max(start);
    // The project's own GIF rate, as the apps' GIF export reads it (a
    // fixed 12 here used to disagree with their 10).
    let fps = opts
        .fps
        .unwrap_or(project.meta.composition_settings.gif_export_fps)
        .max(1.0);
    let count = frame_count(start, end, fps);

    let file = std::fs::File::create(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut encoder = image::codecs::gif::GifEncoder::new(std::io::BufWriter::new(file));
    encoder
        .set_repeat(image::codecs::gif::Repeat::Infinite)
        .map_err(|e| e.to_string())?;
    let delay = image::Delay::from_numer_denom_ms((1000.0 / fps).round() as u32, 1);
    let mut renderer = render::Renderer::with_proxy(project, w, h, opts.proxy)?;
    renderer.set_transparent_plate(opts.alpha);
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

    /// A video's MIDI: a used take with no bank is refused with how to
    /// install one; with the bank (PROMOSHOT_SOUNDS) it is settled, the
    /// nested composition's use counting, and validate is quiet after.
    #[test]
    fn a_video_settles_the_midi_it_uses() {
        let dir = std::env::temp_dir().join(format!("promo-cli-midi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        let doc = serde_json::json!({
            "id": "P", "name": "T", "createdAt": 0, "state": "recorded", "subtitles": [],
            "trimStart": 0, "trimEnd": 2, "videoDuration": 2, "compositionSettings": {},
            "layers": [{ "id": "c", "name": "C", "sortIndex": 0, "kind": "video",
                "isEnabled": true, "startTime": 0, "duration": 2, "resourceID": "comp",
                "keyframes": [] }],
            "resources": [
                { "id": "M", "kind": "audio", "filename": "", "displayName": "Bed", "addedAt": 0,
                  "midi": { "tracks": [{ "notes": [{ "t": 0, "d": 1, "n": 60 }] }] } },
                { "id": "comp", "kind": "composition", "filename": "", "displayName": "comp",
                  "addedAt": 0, "composition": { "layers": [{ "id": "a", "name": "A",
                    "sortIndex": 0, "kind": "audio", "isEnabled": true, "startTime": 0,
                    "duration": 2, "resourceID": "M", "keyframes": [] }] } }]
        });
        std::fs::write(dir.join("metadata.json"), doc.to_string()).unwrap();
        let installed = promo_midi::sounds::installed_default();
        let empty = dir.join("no-sounds");
        promo_midi::sounds::set_sounds_dir(Some(empty));
        let refused = settle_used_midi(&dir).unwrap_err();
        promo_midi::sounds::set_sounds_dir(None);
        assert!(
            refused.contains("\"M\"") && refused.contains("sounds install generaluser"),
            "{refused}"
        );
        let Some(_) = installed else {
            eprintln!(
                "SKIPPED the render: {}",
                promo_midi::sounds::missing_bank_message()
            );
            return;
        };
        let lines = settle_used_midi(&dir).unwrap();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(dir.join("Resources/Bed.wav").is_file());
        assert!(
            settle_used_midi(&dir).unwrap().is_empty(),
            "current: nothing to do"
        );
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("metadata.json")).unwrap())
                .unwrap();
        assert!(promo_midi::findings(&doc, &dir.join("Resources")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The skill installs once for every agent tool found: the shared home
    /// always, a tool's own folder only when it reads nothing else. The
    /// app's SkillInstallerTests pin the same case.
    #[test]
    fn the_skill_installs_for_every_tool_found() {
        let home = std::env::temp_dir().join(format!("promo-skill-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        for folder in [".claude", ".codex", ".gemini/antigravity", ".cursor"] {
            std::fs::create_dir_all(home.join(folder)).unwrap();
        }
        let destinations = skill_destinations(&home);
        let labels: Vec<&str> = destinations.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            labels,
            [
                "shared, read by Codex, Gemini CLI, Cursor",
                "Claude Code",
                "Antigravity"
            ]
        );
        skill(&[
            "install".into(),
            "--home".into(),
            home.display().to_string(),
        ])
        .unwrap();
        for path in [
            ".agents/skills/promoshot/SKILL.md",
            ".claude/skills/promoshot/SKILL.md",
            ".gemini/antigravity/skills/promoshot/SKILL.md",
        ] {
            let written = std::fs::read_to_string(home.join(path)).unwrap();
            assert!(written.starts_with("---\n"), "{path}");
        }
        assert!(
            !home.join(".codex/skills").exists(),
            "Codex reads the shared home"
        );
        let bare = home.join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        assert!(
            skill_destinations(&bare).is_empty(),
            "nothing found, nothing written"
        );
        std::fs::create_dir_all(bare.join(".agents")).unwrap();
        let existing = skill_destinations(&bare);
        assert_eq!(existing.len(), 1);
        assert!(
            existing[0]
                .0
                .starts_with("shared, ~/.agents already present"),
            "{}",
            existing[0].0
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The moments a frames call renders, in precedence order: exact times,
    /// then a sample across the range, then every frame at the rate. The
    /// sample includes both ends, which is what makes a contact sheet show
    /// the first and last picture.
    #[test]
    fn frame_times_follow_what_was_asked_for() {
        let plan = |end: f64, fps: f64| promo_timeline::ExportPlan {
            start: 0.0,
            end,
            fps,
            count: frame_count(0.0, end, fps),
            width: 64,
            height: 64,
        };
        let pick = |times: &[f64], sample: Option<usize>, end: f64, fps: f64| {
            promo_timeline::look_times(times, sample, &plan(end, fps), 8.0, None).unwrap()
        };
        assert_eq!(
            pick(&[], None, 1.0, 4.0),
            vec![0.0, 0.25, 0.5, 0.75],
            "the export plan's own count"
        );
        assert_eq!(pick(&[], Some(5), 8.0, 30.0), vec![0.0, 2.0, 4.0, 6.0, 8.0]);
        assert_eq!(
            pick(&[], Some(1), 8.0, 30.0),
            vec![4.0],
            "one moment is the middle"
        );
        assert_eq!(
            pick(&[3.0, 0.5], Some(5), 8.0, 30.0),
            vec![0.5, 3.0],
            "exact times win, in order"
        );
    }

    /// A shorter run must not leave the tail of a longer one behind, and
    /// nothing else in the folder is this tool's to remove.
    #[test]
    fn clearing_takes_only_this_tools_frames() {
        let dir = std::env::temp_dir().join(format!("promo-frames-clear-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "frame-00000.png",
            "frame-00001.png",
            "frame-x.png",
            "frames.png",
            "notes.txt",
            "frame-00002.jpg",
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        assert_eq!(clear_frames(&dir), 2);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            ["frame-00002.jpg", "frame-x.png", "frames.png", "notes.txt"]
        );
        assert_eq!(clear_frames(&dir), 0, "and it is idempotent");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A sheet shows at most two dozen cells, always including the first
    /// and the last: two hundred thumbnails show nothing.
    #[test]
    fn a_sheet_samples_the_frames_it_shows() {
        assert_eq!(sheet_sample(6), vec![0, 1, 2, 3, 4, 5]);
        let many = sheet_sample(600);
        assert_eq!(many.len(), 24);
        assert_eq!(many[0], 0);
        assert_eq!(*many.last().unwrap(), 599);
        assert!(many.windows(2).all(|w| w[0] < w[1]), "{many:?}");
        // Cells keep the canvas's proportions, long edge 256.
        assert_eq!(sheet_cell(1920, 1080), (256, 144));
        assert_eq!(sheet_cell(2064, 2752), (192, 256));
        assert_eq!(sheet_cell(120, 90), (120, 90), "never upscaled");
    }

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

    /// `--from/--to` take seconds or frames; frames become seconds at the
    /// command's rate once the project is open.
    #[test]
    fn a_range_is_seconds_or_frames() {
        assert_eq!(parse_moment("12.5", "--from"), Ok(Moment::Seconds(12.5)));
        assert_eq!(parse_moment("12.5s", "--from"), Ok(Moment::Seconds(12.5)));
        assert_eq!(parse_moment("375f", "--to"), Ok(Moment::Frames(375.0)));
        assert!(parse_moment("1.5f", "--to").is_err(), "a frame is whole");
        assert!(parse_moment("-3f", "--to").is_err());
        assert!(parse_moment("soon", "--to").is_err());
        let opts = Options::parse(&[
            "--from".into(),
            "300f".into(),
            "--to".into(),
            "20".into(),
            "--segment".into(),
            "5".into(),
        ])
        .unwrap();
        assert_eq!(
            (opts.from_frames, opts.to, opts.segment),
            (Some(300.0), Some(20.0), Some(5.0))
        );
        assert!(Options::parse(&["--segment".into(), "0".into()]).is_err());
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

    /// After the app has minted a project's ids, `handles` keeps the
    /// author's spellings, and inspect shows each beside its UUID — what
    /// the person who wrote "deck" remembers, and what every tool takes.
    #[test]
    fn inspect_shows_the_authors_spelling_beside_a_minted_id() {
        let dir =
            std::env::temp_dir().join(format!("promo-handles-inspect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let uuid = "7B1C6C4E-1F8A-5C1C-9E2B-2D6F3A4B5C6D";
        std::fs::write(
            dir.join("metadata.json"),
            format!(
                r#"{{"id":"P","name":"n","createdAt":0,"state":"recorded","trimStart":0,
                "trimEnd":4,"videoDuration":4,"subtitles":[],
                "compositionSettings":{{"canvasWidth":1280,"canvasHeight":720}},
                "resources":[],"layers":[{{"id":"{uuid}","name":"Deck","sortIndex":0,
                "kind":"background","isEnabled":true,"startTime":0,"duration":4,"keyframes":[]}}],
                "handles":{{"{uuid}":"deck"}}}}"#
            ),
        )
        .unwrap();
        let project = Project::open(&dir).unwrap();
        let text = inspect(&project, &Options::default()).unwrap();
        assert!(text.contains(&format!("{uuid}  background")), "{text}");
        assert!(text.contains("← deck"), "{text}");
        assert!(text.contains("handles:   1 author ids kept"), "{text}");
        let json = inspect(
            &project,
            &Options {
                json: true,
                ..Options::default()
            },
        )
        .unwrap();
        assert!(json.contains(r#""handle":"deck""#), "{json}");
        let _ = std::fs::remove_dir_all(&dir);
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
        assert_eq!(json["compositions"][0]["boundTo"], serde_json::json!([]));
        let text = inspect(&project, &Options::parse(&[]).unwrap()).unwrap();
        assert!(
            text.contains("compositions: 1") && text.contains("placed by 1 layer\n"),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A composition on a device's Screen slot is placed — the body shows
    /// it — though no layer names it. Inspect used to say "placed by 0",
    /// which the agent authoring the three-devices demo read as an unused
    /// resource; the slot is counted, and named apart from the layers.
    #[test]
    fn inspect_counts_a_composition_bound_to_a_slot_as_placed() {
        let dir = std::env::temp_dir().join(format!("promo-slot-inspect-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Resources")).unwrap();
        std::fs::write(
            dir.join("metadata.json"),
            r#"{"id":"P","name":"Device","createdAt":0,"state":"recorded","minReaderVersion":44,
            "trimStart":0,"trimEnd":4,"videoDuration":4,"subtitles":[],
            "compositionSettings":{"canvasWidth":1280,"canvasHeight":720},
            "resources":[{"id":"cap","kind":"caption","filename":"","displayName":"Words","addedAt":0,
               "captionText":"hi","imageCuts":[]},
              {"id":"scr","kind":"composition","filename":"","displayName":"Phone screen","addedAt":0,
               "duration":4,"pixelWidth":1170,"pixelHeight":2532,"imageCuts":[],
               "composition":{"canvasWidth":1170,"canvasHeight":2532,"layers":[
                 {"id":"L","name":"inner","sortIndex":0,"kind":"caption","isEnabled":true,
                  "startTime":0,"duration":4,"resourceID":"cap","keyframes":[]}]}},
              {"id":"dev","kind":"model","filename":"","displayName":"Phone","addedAt":0,"imageCuts":[],
               "recipe":{"device":{"kind":"phone"}},
               "materials":{"Screen":{"resourceID":"scr","finish":"glass"},"Body":"2B2C30"}}],
            "layers":[{"id":"bench","name":"Bench","sortIndex":0,"kind":"stage","isEnabled":true,
              "startTime":0,"duration":4,"keyframes":[],
              "members":[{"id":"m","name":"Phone","sortIndex":0,"kind":"model","isEnabled":true,
                "startTime":0,"duration":4,"resourceID":"dev","keyframes":[]}]}]}"#,
        )
        .unwrap();
        let project = Project::open(&dir).expect("opens");
        let out = inspect(&project, &Options::parse(&["--json".into()]).unwrap()).unwrap();
        let json: serde_json::Value = serde_json::from_str(&out).unwrap();
        let composition = &json["compositions"][0];
        assert_eq!(composition["id"], "scr");
        assert_eq!(composition["placedBy"], serde_json::json!([]));
        assert_eq!(
            composition["boundTo"],
            serde_json::json!([{ "resource": "dev", "slot": "Screen" }]),
            "{out}"
        );
        let text = inspect(&project, &Options::parse(&[]).unwrap()).unwrap();
        assert!(
            text.contains("placed by 0 layers, on 1 slot (dev Screen)"),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A composition reached only by a swap keyframe — the takeover (rung
    /// 47): one video layer, a scene per swap — is placed, though no layer
    /// names it at rest. Inspect said "placed by 0 layers" of the
    /// carousel demo's scenes 2–4, and the agent that authored it read the
    /// line as understating the truth. A takeover is counted on any layer
    /// — top level, stage member or nested — and said apart from a rest
    /// placement, with the layer and the time: the handle
    /// promo_upsert_keyframe takes.
    #[test]
    fn inspect_counts_a_composition_a_swap_keyframe_takes_over_as_placed() {
        let dir =
            std::env::temp_dir().join(format!("promo-takeover-inspect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("metadata.json"),
            r#"{"id":"P","name":"Carousel","createdAt":0,"state":"recorded","minReaderVersion":47,
            "trimStart":0,"trimEnd":12,"videoDuration":12,"subtitles":[],
            "compositionSettings":{"canvasWidth":1280,"canvasHeight":720},
            "resources":[{"id":"cap","kind":"caption","filename":"","displayName":"Words","addedAt":0,
               "captionText":"hi","imageCuts":[]},
              {"id":"A","kind":"composition","filename":"","displayName":"Scene 1","addedAt":0,
               "duration":4,"pixelWidth":1280,"pixelHeight":720,"imageCuts":[],
               "composition":{"canvasWidth":1280,"canvasHeight":720,"layers":[
                 {"id":"a1","name":"words","sortIndex":0,"kind":"caption","isEnabled":true,
                  "startTime":0,"duration":4,"resourceID":"cap","keyframes":[]}]}},
              {"id":"B","kind":"composition","filename":"","displayName":"Scene 2","addedAt":0,
               "duration":4,"pixelWidth":1280,"pixelHeight":720,"imageCuts":[],
               "composition":{"canvasWidth":1280,"canvasHeight":720,"layers":[
                 {"id":"b1","name":"words","sortIndex":0,"kind":"caption","isEnabled":true,
                  "startTime":0,"duration":4,"resourceID":"cap","keyframes":[]}]}},
              {"id":"C","kind":"composition","filename":"","displayName":"Scene 3","addedAt":0,
               "duration":4,"pixelWidth":1280,"pixelHeight":720,"imageCuts":[],
               "composition":{"canvasWidth":1280,"canvasHeight":720,"layers":[
                 {"id":"c1","name":"inner card","sortIndex":0,"kind":"video","isEnabled":true,
                  "startTime":0,"duration":4,"resourceID":"A","keyframes":[
                    {"id":"ck","time":1,"resourceID":"B","transitionDuration":0}]}]}}],
            "layers":[{"id":"card","name":"Card","sortIndex":0,"kind":"video","isEnabled":true,
              "startTime":0,"duration":12,"resourceID":"A","keyframes":[
                {"id":"k1","time":4,"resourceID":"B","transitionDuration":0,"sourceTime":0},
                {"id":"k2","time":8,"resourceID":"C","transitionDuration":0,"sourceTime":0}]}]}"#,
        )
        .unwrap();
        let project = Project::open(&dir).expect("opens");
        let out = inspect(&project, &Options::parse(&["--json".into()]).unwrap()).unwrap();
        let json: serde_json::Value = serde_json::from_str(&out).unwrap();
        let scene = |id: &str| {
            json["compositions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["id"] == id)
                .cloned()
                .unwrap_or_else(|| panic!("no composition {id} in {out}"))
        };
        // Scene 1 is placed at rest twice — by the card, and by the inner
        // card nested in scene 3 — and taken over by nothing.
        assert_eq!(scene("A")["placedBy"], serde_json::json!(["card", "c1"]));
        assert_eq!(scene("A")["takenOverBy"], serde_json::json!([]));
        // Scene 2 is named by no layer at rest; two swaps reach it — the
        // card's, and the nested inner card's.
        assert_eq!(scene("B")["placedBy"], serde_json::json!([]));
        assert_eq!(
            scene("B")["takenOverBy"],
            serde_json::json!([{ "layer": "card", "time": 4.0 }, { "layer": "c1", "time": 1.0 }]),
            "{out}"
        );
        assert_eq!(
            scene("C")["takenOverBy"],
            serde_json::json!([{ "layer": "card", "time": 8.0 }])
        );
        assert_eq!(json["renderable"], 1, "a swapped card needs no file");
        let text = inspect(&project, &Options::parse(&[]).unwrap()).unwrap();
        assert!(
            text.contains("\"Scene 1\"  4.00s  1 layers  placed by 2 layers\n"),
            "{text}"
        );
        assert!(
            text.contains("placed by 0 layers, taken over by 2 keyframes (card 4.00s, c1 1.00s)\n"),
            "{text}"
        );
        assert!(
            text.contains("placed by 0 layers, taken over by 1 keyframe (card 8.00s)\n"),
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
