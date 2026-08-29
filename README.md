# promo-core

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
