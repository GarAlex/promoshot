# promo-core

<p align="center">
  <img src="docs/rendered-on-linux.png" width="720"
       alt="A frame rendered by the engine on Linux: a bordered video card over a themed background, with a stroked, shadowed caption reading 'Rendered on Linux'.">
</p>

The rendering engine behind [PromoShot](https://apps.apple.com/app/promoshot),
and an open implementation of its project format. A `.promo` project is a
folder — `metadata.json` plus its media — and this workspace is everything
needed to validate, inspect, and render one to stills, image sequences, or
mp4 with mixed audio: no app attached, byte-for-byte the same compositor the
apps ship.

The design bet is that **the format is the interface**. An assistant, a
script, or a person writes `metadata.json`; the engine renders it the same
everywhere — the Mac and iOS apps (Metal + VideoToolbox), this repo's CLI,
or a headless Linux box with no GPU at all (wgpu on lavapipe, ffmpeg as a
subprocess). The schema is one compiled-in document (`promo_model::SCHEMA`,
served by `promo schema`), and the parser the validator runs is the parser
the renderers use, so "validates" means "renders".

## Crates

| Crate | What it owns |
|---|---|
| `promo-model` | The format: wire structs, migrations, palette roles, `schema.md` |
| `promo-timeline` | Timeline math: keyframes, trims, attachments, waits, validation |
| `promo-gpu` | wgpu compositing: quads, borders, letterbox, vectors, color conversion |
| `promo-text` | Caption shaping and effects (cosmic-text) |
| `promo-engine` | Preview/export orchestration, frame cache, memory governor, PCM mixer |
| `promo-media` | Decoder/encoder trait registry; ffmpeg-subprocess backend + conformance suite |
| `promo-editor` | Front-end-agnostic editor state: lanes, viewport, transport, selection |
| `promo-ffi` | The C ABI the Swift apps link |
| `promo-cli` | `promo` — render a project from the command line |
| `promoshot-mcp` | MCP server over stdio, for agents |

## Build and verify

```
./check-all.sh          # fmt, clippy -D warnings, all tests, release build
```

Rendering video needs `ffmpeg` (and `ffprobe`) on PATH — frames are composited
on the GPU and piped to it raw; ffmpeg only decodes and encodes. On a headless
Linux machine, `mesa-vulkan-drivers` (lavapipe) is enough of a GPU.

## The CLI

```
cargo build --release -p promo-cli     # -> target/release/promo

promo schema                            # the format, read this first
promo validate <project>                # exit 0 == this will render
promo inspect  <project>                # canvas, layers, missing media, undefined colours
promo still    <project> --out f.png --time 2.5
promo frames   <project> --out frames/ --fps 30 --from 0 --to 4
promo video    <project> --out out.mp4 --fps 30
```

## The MCP server

`promoshot-mcp` speaks Model Context Protocol over stdio, so any MCP client
can validate and render projects. It owns no rendering code — every render
shells to `promo` (found next to the executable, or on PATH, or via
`--promo`), so the CLI stays the single contract.

```
cargo build --release -p promoshot-mcp -p promo-cli
```

Client configuration (Claude Code, or any MCP client):

```json
{
  "mcpServers": {
    "promoshot": {
      "command": "/path/to/target/release/promoshot-mcp"
    }
  }
}
```

Tools: `promo_schema`, `promo_validate`, `promo_inspect`, `promo_render_still`,
`promo_render_frames`, `promo_render_video`, `promo_workspace`. Renders
default their output into the project's `Exports/` folder and return the path
written, never the bytes.

Flags, all optional: `--workspace <dir>` (where `promo_workspace` points;
else `$PROMOSHOT_WORKSPACE`, else the XDG data dir), `--root <dir>` (refuse
projects outside this tree), `--promo <path>`.

The Mac app carries its own MCP server (Settings → Automation) with the same
tool names plus app-only abilities — opening the editor, speech synthesis.
One skill drives both.

What a session looks like — three requests in, a validated project and a
rendered frame out (the frame at the top of this page was made exactly this
way):

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"promo_validate","arguments":{"project":"examples/LinuxSmoke.promo"}}}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"promo_render_still","arguments":{"project":"examples/LinuxSmoke.promo","time":5.5}}}
```

```json
{"id":2,"result":{"content":[{"type":"text","text":"ok — nothing the renderer would quietly correct"}]}}
{"id":3,"result":{"content":[{"type":"text","text":"wrote examples/LinuxSmoke.promo/Exports/still-5.5s.png (1280x720 at 5.50s)"}]}}
```

## One engine, every platform

The same project rendered on macOS (Metal, VideoToolbox) and on a bare
Linux container (lavapipe software Vulkan, no GPU; ffmpeg) — SSIM 0.983
over the full 240-frame video. The visible difference is the font: the
caption asks the question, the two frames answer it.

<p align="center">
  <img src="docs/mac-vs-linux.png"
       alt="The same frame rendered on macOS (left) and Linux (right), near-identical: a bordered gradient card over a dark background, captioned 'Same pixels as the Mac?'.">
</p>

Try it yourself — [examples/LinuxSmoke.promo](examples/LinuxSmoke.promo)
is a complete project (synthetic media, ~450 KB): a clip with audio, an
image, palette-named colours, placement rules, easing, and a keyframe
that waits for another layer:

```
promo video examples/LinuxSmoke.promo --out smoke.mp4
```

## Authoring a project

Start with `promo schema`. The short version: a project folder holds
`metadata.json` and `Resources/`; every id is a UUID; layers place resources
on a timeline with keyframes (hold-then-ease), placement rules, transitions
and palette-named colours (`@accent`). Validate before rendering — the
validator names what the renderer would silently correct, undefined colour
names included.

```
mkdir -p Demo.promo/Resources
# write Demo.promo/metadata.json, copy media into Resources/
promo validate Demo.promo && promo still Demo.promo --out look.png --time 1
```

## Invariants and plans

- `SPECS.md` — the invariants the tests pin.
- `LINUX-READY-PLAN.md` — how the engine became portable, measured; the
  first real-Linux run's results are in its Status section.
- `EDITOR-PLAN.md` — where `promo-editor` is heading: the core owns the
  document, front ends send commands.

## License

Apache-2.0. The PromoShot applications built on this engine are separate,
proprietary products.
