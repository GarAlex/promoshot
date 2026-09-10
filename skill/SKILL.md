---
name: promoshot
description: Author and render PromoShot .promo video projects (App Store shots, promo reels, slideshows) via promoshot-mcp or the promo CLI. Use when the user wants a promo video, marketing screenshot, device-framed app demo, or to edit a .promo folder — headless via promoshot-mcp/CLI, or through the PromoShot app's automation server.
---

# Authoring PromoShot projects

A PromoShot project is a **folder named `<Name>.promo`**: `metadata.json`
plus `Resources/` holding the media it names. The file is the interface —
everything below writes, checks, or renders that file, and a project you
author here opens in the PromoShot apps unchanged.

## Two modes, and they are not the same job

**Nobody watching** (the headless server, the CLI): the file is yours.
Write `metadata.json` directly, validate, look, render. That is what the
rest of this document assumes.

**The project is OPEN in the PromoShot app**: the file is SHARED, not
yours. Write through `promo_upsert_layer`, `promo_upsert_keyframe` and
`promo_apply` instead. Those three go through the person's live document:
their unsaved work is folded in first, your change lands as ONE step in
their own undo history — they can ⌘Z you — and with "Ask before applying"
on it stages as a proposal they accept, rather than a write. A whole-file
write carries only a RESULT; a command carries the INTENT, which is what
lets the editor redraw one row instead of reloading, and hand the person
one reversible step instead of an opaque adoption of everything.

How to tell which mode you are in: the app's server offers `promo_open`,
`promo_context` and `promo_list_projects`, and declares `resources`; the
headless one offers none of those. `promo_context` then says which
projects are actually open, which layer is selected, where the playhead
is, and whether Ask before applying is on. Read it before your first
write, not after. The detail is under "With the app attached" below.

## Reaching the tools

The tools are an MCP server's; a session has them or it does not. Two
servers exist, one contract: the PromoShot APP's — the person's live
document, their undo, their proposals, a window they watch — and the
HEADLESS one, `promoshot-mcp` — nobody watching, the file is yours.
Find out what is here before choosing.

1. **Already there.** Tools named `promo_*` are registered — use them.
   A tool carries its server's registered name as a prefix, so both may
   be present side by side (`promoshot` for the app, `promoshot-headless`
   for the binary); `promo_context` is answered only by the app's.
2. **What the machine has.** The app: `/Applications/PromoShot.app`, or
   `mdfind "kMDItemCFBundleIdentifier == 'com.writea.revoice'"`. The
   pair: `command -v promo promoshot-mcp`. A server joins a session at
   its start, so registering is one line and a new session — the app:
   `claude mcp add promoshot -- "/Applications/PromoShot.app/Contents/MacOS/PromoShot" --mcp-stdio`
   (the app opens itself when a session needs it, no token to paste;
   Settings ▸ Automation copies that line, and a config block for other
   clients); the pair: `claude mcp add promoshot-headless -- promoshot-mcp`,
   or the image, `claude mcp add promoshot-headless -- docker run -i --rm
   -v "<your projects>:/projects" ghcr.io/garalex/promoshot-mcp`.
3. **Which, when both are there.** The prompt says — follow it. A
   project the person has open in the app — the app's. Otherwise ASK
   the person: in the app, where they watch and can undo, or headless,
   where nothing is watching and the file is yours. That is their call,
   not a default.
4. **Mid-session, when the client cannot take a new server**, the app's
   can be spoken to directly: streamable-HTTP JSON-RPC at
   `http://127.0.0.1:8765/mcp` (the port the app wrote is
   `defaults read com.writea.revoice PromoMCPPort`), the bearer token in
   the app's own defaults — `defaults read com.writea.revoice
   PromoMCPToken`. `initialize`, then `notifications/initialized`, then
   `tools/call` — the same tools, the same arguments, one POST each:
   `curl -s http://127.0.0.1:8765/mcp -H "Authorization: Bearer $T"
   -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","id":1,
   "method":"tools/call","params":{"name":"promo_context","arguments":{}}}'`.
   The app not running? `open -b com.writea.revoice` launches it, and
   with automation once switched on the server comes up with it — poll
   the endpoint until a POST without a token answers 401 (listening),
   then call. This is the person's app and the person's token on the
   person's machine: a bridge for a session that started before the
   server was registered, not a way around asking to enable automation.
5. **No binaries.** `promo` and `promoshot-mcp` ship together, prebuilt:
   <https://github.com/GarAlex/promoshot/releases/latest> —
   `promoshot-<tag>-macos-arm64.tar.gz` or `-linux-x64.tar.gz`; untar
   both onto PATH (rendering video also wants `ffmpeg`/`ffprobe`). The
   `promo` CLI then works from the shell in THIS session, no
   registration needed: `promo schema | validate | inspect | still |
   frames | video | gif`, the same contract as the tools, argument for
   argument — so headless work never waits on a new session.

Do not guess a port, a path or a token: the app's Automation page and
its defaults are the source of the first, the release page of the
second.

## The loop

1. **Learn the format** — `promo_schema` once: the authoring subset plus
   four complete, validated recipes (a device-framed product card, two
   clips under a wipe, a Ken Burns push with a lower caption, a 9:16
   re-stamp). `promo_schema_full` is the whole format when a field is not
   in the subset; `promo_schema_types` is a generated, types-only JSON
   Schema to fill structured output against.
2. **Look at the footage first** — the senses, before composing:
   - `promo_media_probe` — container, duration, streams, fps, display
     rotation, channels. On a `.glb`: its material slots (what a
     `materials` binding may name), clips and bounds.
   - `promo_media_turntable` — a `.glb` seen from N yaws round it, one
     PNG contact sheet with each cell's yaw. Look before choosing a
     model layer's camera.
   - A device needs no tool and no file: a model resource whose
     `recipe` is `{ "device": { "kind": "phone" } }` (tablet, laptop)
     is built at load with `Body` and `Screen` slots, and a `Deck` on
     the laptop. A screenshot or a screen recording bound to `Screen`,
     the accent on `Body`, a camera keyed to turn it: the device shot,
     as a model.
   - `promo_media_filmstrip` — a contact sheet of a SOURCE clip, with the
     time each cell samples. Read it before deciding what a clip shows.
   - `promo_media_silences` — silence spans and their inverse; cuts and
     captions land on those boundaries.
   - `promo_media_scenes` — scene-change cuts and the shots between
     them; the cut list when the footage has no silence gaps.
   - `promo_transcribe` — a transcript with timings, the draft captions
     are cut from (headless it needs whisper-cli + WHISPER_MODEL; the
     app uses Apple's recognizer; without either, captions are typed).
   The repo's `examples/media/talktrack.mp4` is a practice clip whose
   silences and cuts are both real.
3. **Author** — `promo_slideshow` is the wizard: pictures and clips in,
   a complete show out — classic, carousel, or an App Store listing
   sized by the store — a `caption` on any slide becomes a caption layer
   that lives and arrives with its picture (a headline band for the
   store, a lower third otherwise); the answer carries the glance like
   every authoring tool — then refine it with the tools below. `promo_init` lays the folder, canvas, palette,
   background; `promo_upsert_layer` adds an image/video/caption with a
   placement, a fadeIn, a device frame — media copied in, sizes and
   durations probed, the composition re-stretched every call; and
   `promo_upsert_keyframe` is the MOTION: one keyframe, created or
   merged — a second placement keyframe is a push-in, viewport
   keyframes are a Ken Burns ride, colorHex ramps a background — and a
   created keyframe's ramp defaults to span from the previous one,
   which is what a push-in means. Updating by id changes only what you
   pass; hand-added keyframes survive. Everything else the format can
   say goes through `promo_apply`: a batch of the editor's own commands
   as ONE atomic step — delete, move, retime, a wipe or any transition
   (`updateLayer` with a merge patch: only the fields you pass change,
   null removes), a swap (`upsertKeyframe` with `resourceID` and a
   `transition`), a trim (`patchResource` — the layers that played the
   whole resource follow the new length and the background stretches
   over them; a layer you shortened yourself keeps its length), a
   canvas change (`patchSettings`), the timeline's markers and chapters
   (`setMarkers`, the list replaced whole), waits and motion paths
   (keyframe fields). The tool names every command and what each takes;
   the SHAPES it refers to — a layer, a resource, a keyframe — are in
   `promo_schema_types`, their prose in `promo_schema_full`. Ids are the file's own. Hand-editing the JSON
   stays first-class for anything a recipe shows. The authoring tools and validate each
   answer with a small thumbnail ATTACHED — a keyframe's glance looks
   where the motion arrives; the others sample the touched layer's
   midpoint, past its fadeIn — and write the same image to
   `<Name> Exports/preview.png` beside the project. Look at it before
   the next edit; it is the editor viewport. (`preview: false` turns it
   off.)
4. **Check** — `promo_validate` runs the renderers' own parser, so "ok"
   means "renders"; anything else is a silent correction named before you
   see it in pixels. `promo_inspect` summarizes what is in the project —
   canvas, each layer with its ID (the handle the upsert tools take,
   spelled as the file spells it), undefined colours, missing media.
   `promo_explain` is the debugger: the renderer's OWN numbers at a
   moment — visible and why not, the rect on the canvas, the keyframes
   bracketing the time — when a layer is not where you meant. `promo_diff`
   reads the other author's turn: copy `metadata.json` aside before a
   person edits, then diff against the copy.
5. **Render** — `promo_render_frames` FIRST and often: bare, it samples
   twelve moments across the piece, tiles them into one contact sheet and
   attaches it as an image, which is the cheapest way to catch a
   mis-aimed viewport, an empty frame or an invisible caption. Name
   `times` for exact moments. `promo_render_still` when one moment is
   the question. Then `promo_render_video` for the mp4 or
   `promo_render_gif` for the looping preview. A render lands BESIDE
   the project, in `<Name> Exports/` (never inside the `.promo`), and
   returns paths; only the sheet and the authoring tools' glance return
   pixels.

Narration: `promo_voices` lists a provider's voices (pick a voiceID
from it); `promo_speak` synthesizes every resource whose `speech.text`
says something, spending the person's OWN provider key — headless from
the OS keyring, where the person registers it once with
`promoshot-mcp key set <provider>` (the key is read from stdin, never
from an argument), else — where there is no keyring, a container — from
a secrets file (`/run/secrets/OPENAI_API_KEY`, or the path in
`OPENAI_API_KEY_FILE`; likewise ELEVENLABS and GOOGLE), never from an
environment variable; in the app from the person's Keychain. No tool takes a key and none ever shows one.
Before planning a narrated piece, ask `promo_speak {"check": true}`
(with or without a project): it spends nothing and says, per provider,
whether a key is present and what a real call would synthesize —
ready, blocked, or nothing to do. A real call checks every pending
narration's key BEFORE buying anything, and writes each receipt back
the moment it is paid for. Unchanged text is reused by receipt, never
billed twice.
**Without a key an agent cannot narrate** — do not pretend: record or
obtain a voice file, drop it into `Resources/`, and reference it as an
ordinary audio resource.

The `promo` CLI is the same contract (`promo schema | validate | inspect |
still | frames | video | gif`), and `promo_workspace` names a folder for
new projects.

## The rules, and where the rest lives

The schema is the reference; this is the index and the judgment. Ask for
a feature's own section rather than the whole 67 KB —
`promo_schema_full {"topics": ["particles", "route"]}` is about 2 KB,
`"core"` is the format proper.

**The rules that are only here:**

- **Ids are unique strings.** Short mnemonics — "bg", "clip", "k0" — are
  fine and are the handles the tools take; the app mints a UUID for each
  on adoption and keeps the mapping, so re-anchor on what `promo_inspect`
  lists after a person has opened the project.
- **Never write `minReaderVersion` by hand.** The tools compute it from
  what the file uses, and `promo_validate` names the number when a
  hand-written file declares one that is too low. A literal is a guess
  that goes stale.
- **Measure what you place.** A placed image resource wants
  `pixelWidth`/`pixelHeight` (a video, `videoNaturalWidth`/`Height`), or
  a `placement` rule resolves against a SQUARE and lands wrong.
  `promo_media_probe` answers for one file or several at once
  (`{"files": ["a.png", "b.png"]}`, keyed by path), and each answer
  carries the `resource` entry that file becomes — paste it into
  `resources` rather than assembling one and forgetting the size.
- **Look before you ship.** `promo_render_frames` samples the piece and
  answers with one contact sheet as an image; the authoring tools and
  `promo_validate` attach a glance of the moment they touched. A
  mis-aimed viewport or an invisible caption costs seconds here and
  minutes in a video.
- **Two captions never cross-fade**, and cross-dissolving layers need
  overlap: two clips that end and begin at the same instant both sit at
  zero opacity there and the background flashes through.
- **The file is shared when a person has it open** — see "Two modes"
  above. Headless, it is yours.
- **No 2.5D.** A caption's `tiltX`/`tiltY`, `captionStyle.depth` and the
  device `frame` on a picture are the flat compositor's old tricks: they
  still render, `promo_validate` names them legacy, and new work does
  not use them. A title with a side or a lean is a text body in a stage;
  a device is a device body with the picture on its Screen slot.
- **Colours can be palette names** (`"@accent"`); an undefined name
  renders as the field's default and `promo_inspect` lists it. Name a
  colour that appears more than once.
- **For autocomplete in hand-written files**, point `"$schema"` at
  `docs/promo.schema.json` in this repo.

**Two tool arguments worth knowing:** `promo_render_video` takes
`codec: "prores"` and `alpha: true` for an edit-ready master with
transparency (h264/hevc cannot carry alpha); `promo_proxy {project}`
builds a tier-1 proxy per video resource once, and every later render
reads it — do it before working with long sources.

**The features, by the topic word that fetches them.** Each says what it
is for; the schema section says how to write it.

- **Models** (`model`) — a `.glb` in `Resources/`, or a recipe with no
  file at all, shown by a layer of `"kind": "model"`. Camera and light
  are keyframed and ramp: a yaw from −60 to 20 over three seconds is a
  turntable. `materials` paints a slot by the name the file exports, and
  ANY slot can show a picture — an image, a video, or a whole
  COMPOSITION playing in place on the layer's clock — as a screen, or
  wear it as the slot's own colour under the light with `"mode":
  "surface"`. A surface is whatever document you bind to it: a device's
  screen, a cube's face, a label on a vase. Say what a surface IS
  with a `finish` word — `chrome`, `brushed`, `anodized`, `gloss`,
  `satin`, `matte`, `rubber`, `ceramic`, `lacquer`, `paper`, `glass`,
  `frosted` — and the engine owns the physics; `glass` on a `Screen`
  slot is the coat that makes a phone read as a phone. Reach for
  `metallic`/`roughness` numbers only to tune a word.
- **Devices** (`model`) — no file and no tool: a model resource whose
  recipe is `{ "device": { "kind": "phone" } }` (tablet, laptop) is
  built at load with `Body` and `Screen` slots, and a `Deck` on the
  laptop. The picture goes on `Screen` — an image, a video or a
  composition — and the accent on `Body`. Build a screen's picture to
  its shape: the phone's is 0.46 wide for its height, the tablet's
  1.46, the laptop's 1.65. A MAXIMUM CLOSE-UP on a screen — the frame
  covered by the picture, looking straight at it — is one pose per
  body, no measuring: the orbit's `distance` at its floor of 1.05 and
  the field that covers, `"camera": { "yaw": 0, "pitch": 0, "distance":
  1.05, "fov": 50, "target": { "point": [0, 0, 0] } }` for the tablet,
  `"fov": 29.5` for the phone, and for the laptop's leaning lid
  `"pitch": 12, "fov": 28, "target": { "point": [0, -0.05, -0.53] }`,
  on a canvas 1.6 wide for its height. Any other body: look along the
  surface's normal at its centre, distance at the floor, and widen the
  field until a still shows the picture in all four corners.
- **Text as a body** (`recipe`) — real type standing in the scene, lit
  and turning. Reach for it when a title must catch the light or show a
  side; a flat title is a caption.
- **Parts** (`parts`) — anything a product shot needs that is not a
  device or type: a stand, a plinth, a ring, a puck. Box, sphere,
  cylinder, torus, lathe and extrude, assembled like an SVG, each part a
  named slot that takes a colour and a finish word.
- **Stages** (`stage`) — several bodies under ONE camera and ONE light.
  Prefer the one-layer form: a layer of `"kind": "stage"` with `members`,
  the camera and light on its own keyframes. Give it a `floor` word —
  `matte` for a shadow on the table, `glossy` or `mirror` for a polished
  one — and the bodies stand on something; the project's background is
  the table, and a keyed light swings the shadow. The flat form (layers
  sharing a stage name) is read forever and rewritten on open.
- **Routes** (`route`) — the 3D twin of a motion path, for a member's
  move or the camera's flight, with `target` saying where the camera
  looks on the way. A spiral in on the vase: a helix route, one camera
  keyframe carrying that `motionPath`, and `"target": { "member":
  "<vase>" }`.
- **Particles** (`particles`) — confetti, sparks, snow, played by a
  DRAWING layer whose start is the burst's instant. Snow is `rate` 30,
  direction 270, spread 20, gravity 0.2, turbulence 0.02, shape dot,
  colours white; a bang is `burst` with no rate. Deterministic: the same
  frame every time, on every host.
- **Morph** (`morph`) — a body bursting into a word. Particles in a
  stage with `morph: { from, to }`, ramped by `progress` 0 → 1 on a
  drawing member; the first body dissolves as they leave and the second
  assembles as they land. For an explosion: 0 → 0.45 in ~0.7 s
  (`easeOut`), → 0.6 over ~1 s, → 1 in ~1.5 s, the last two keyframes
  `"easing": "smooth"` so the points never stop between.
- **Environment** (`environment`) — what chrome and gloss mirror. A
  metal with nothing to reflect reads as flat grey. A preset (studio,
  sunset, night) or a PICTURE of the world: `{ "resourceID": "<uuid>" }`
  naming an image in the project, a panorama twice as wide as tall, for
  a room the bodies really stand in.
- **Kinetic reveals** (`reveal`) — a caption arriving a piece at a time.
  By word with `"seconds": 1.2` is the kinetic-type look; by character
  is busier and wants short text.
- **Designed type** (`tracking`) — a caption's `tracking`, `weight` and
  `lineHeight`. The numbers that work are under "Design guidance" below.
- **A rect** (`rect`) — a drawing shape with an optional `cornerRadius`:
  the accent bar under a headline, the plate behind it, the rounded
  window a screenshot sits in, or a mask.
- **Transitions** (`transition`) — ten kinds, on a layer's own edges or
  at a swap between two resources.
- **Image effects** (`effects`) — blur, glow, vignette, grain, sharpen,
  per layer and keyframable.
- **Chroma key** (`chroma`) — key a green screen on a video or image
  layer, tolerance and softness per layer.
- **Audio effects** (`audio`) — normalize, compress, EQ on a resource.
- **Markers and chapters** (`markers`) — named times that become the
  mp4's chapter list.
- **A look from a `.cube`** (`lut`) — a LUT resource, applied per layer
  with an amount.
- **Compositions** (`composition`) — a whole document inside the
  project: a resource of `"kind": "composition"` with its own canvas
  and `layers`. Place it by a layer like any picture — build a card
  once and place it three times; editing it changes all three — or
  bind it to any surface's slot, where it plays in place on the
  layer's clock: a reel on a screen, an app flow, a film on a wall, the
  next scene of the piece on a device. The composition never knows it
  is a texture: build it as its own film, as long as the whole piece,
  with a camera of its own, to the shape of the surface that will show
  it. THROUGH THE SURFACES, three or four levels deep, is then one
  feature used over and over — switching which composition is MAIN: ONE
  video layer from frame one, a swap keyframe (`resourceID`) at each
  switch naming the next film with a `transition` (a fade, a wipe), no
  `sourceTime` so the film arrives where its clock already is — the
  same instant the screen was showing — and the outer camera flying to
  the maximum close-up on its screen (above) just before. A film that
  should start fresh at its takeover says `"sourceTime": 0`; one that
  should hold still until then says `"playback": "pause"` on its first
  keyframe and `"play"` at the switch. `"placement": { "mode": "fill" }`
  is a centred cover crop and a close-up covers by width, centred, so
  the bands nest on their own; nothing is measured between the films. A
  CAROUSEL of films on a flat canvas is the same feature without the
  close-up: one video layer holding a card's rectangle (`placement`:
  height, anchor, offset), a swap keyframe per card naming the next
  composition with its own `transition` — push, wipe, blurDissolve,
  zoom, flash, glitch, dip — and `"sourceTime": 0` so each film starts
  as it arrives; a caption layer above swaps its words on the same
  instants. Every film is built to the card's shape and knows nothing
  of the others. Keep
  the studio's key box out of a leaning lid's mirror
  (`"environment": { "rotation": 180 }`) and bring the key light away
  from the screen before the switch: the glass, and its glance, go
  with the fade.
- **Follow the pointer** (`pointer`) — a Mac recording carries the
  cursor track, and `follow` on the layer keeps the viewport on it.

## Design guidance for store work

One short headline per shot, above the device; same background and
typography across the set; the headline must describe what the picture
shows. Type carries the set: an eyebrow line in small tracked-out caps
(+6), the headline heavy and slightly tightened (−1.4) at a 1.05 line
height, a medium subtitle under it, and a short `rect` accent bar
between the words and the device. A glow round the device is its
frame's own shadow in the accent colour — `shadowColorHex` the accent,
`shadowOpacity` about 0.34, `shadowRadius` wide (~54 at 1080) — with
`borderColorHex` the same accent at a `borderWidth` of a few px.
Prefer a canvas the source drops into at native size. A set of
stills is a slideshow with hard cuts — every frame is then a finished
screenshot, and the same project doubles as a promo reel.

Pace a MOVING piece like a presentation, not a drift: one eased ramp
across a whole clip reads slow. Move a stage camera in two or three
quick eased-OUT moves of 0.8–1.3 s with holds of about 2 s between (a
hold is a keyframe repeating the values, reached by a long linear ramp),
let the key light fly AHEAD of the camera on every move and drift a
little during the holds, reveal a caption in a second or less, and cut
between beats rather than cross-fading. A move that passes THROUGH
three or more keyframes takes `"easing": "smooth"` on every one of
them: per-ramp easing (easeIn/easeOut/easeInOut) stops at each
keyframe and the piece hitches there; smooth is a cubic through the
neighbouring keyframes that keeps the speed, never leaving their two
values, and holding when they are equal. It shapes keyframed NUMBERS;
a gradient, a transition or a reveal still ramps linearly under it, and
on a motion path it means even speed along the curve. Ease only the two
ends. A body that TURNS is the
exception: spin it in one continuous linear turn (a camera or a member
yaw of 0 → 360 over the shot) under a light that stays put, so each
side passes the light; do not chop a spin into moves.

A leading or trailing caption still needs room: `promo_validate` lays
every caption out for real and names one that runs past the canvas, one
that sits within a safe band of the LEFT or RIGHT edge, 5% of the
canvas's shorter side ("sits 12 px from the canvas's left edge (safe ≥
54 px)"), and one substantially overlapped
by a picture, body or stage that draws above it. Sitting close to the
BOTTOM is not flagged: that is where a lower third belongs. The same
lines ride the replies of `promo_upsert_layer` and `promo_apply`, so
fix them in the same turn — with margins, a placement offset, a smaller
size or a higher sortIndex, never by centring everything.

## With the app attached

The Mac app runs the same tool contract from Settings → Automation, plus
what only an app can do:

- `promo_open` puts the project in front of the person — and from then
  on the file is SHARED, not surrendered. Edits you write land in their
  open editor: additions always; the person's unsaved work wins any
  conflict. `promo_upsert_layer` through the app is the best write
  while they watch — it arrives as ONE step in their own undo history,
  so the person can ⌘Z you. Re-inspect at their turns (`updated:`
  changes when anyone saves) and re-anchor on the minted ids.
- `promo_context` is the person's gaze: which projects are open (and
  unsaved), the selected layer by the id the FILE spells, the playhead,
  the open section, their standing note to you, and whether Ask before
  applying is on. Read it before "make this one bigger".
- The app pushes the person's turn: open a server→client stream (GET
  /mcp, Accept: text/event-stream) and the open project is a resource
  (`resources/list`); `notifications/resources/updated` arrives when
  they save — the turn signal without polling.
- Ask before applying: when it is on, a write on the open project answers
  "proposed" and waits in the Agent panel until the person applies or
  discards it — do not retry; read `promo_context` and continue.
- `promo_speak` there uses the provider key in the person's Keychain, so
  no environment variable is needed.
- Access is per-folder: a tool answering `access_required: <path>` means
  the person approves that folder in PromoShot once, then retry. The
  app's `promo_workspace` names a pre-approved folder.
- Free-tier renders through the app carry the PromoShot watermark,
  exactly as in the app.
