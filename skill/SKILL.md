---
name: promoshot
description: Author and render PromoShot .promo video projects (App Store shots, promo reels, slideshows) via promoshot-mcp or the promo CLI. Use when the user wants a promo video, marketing screenshot, device-framed app demo, or to edit a .promo folder — headless via promoshot-mcp/CLI, or through the PromoShot app's automation server.
---

# Authoring PromoShot projects, headless

A PromoShot project is a **folder named `<Name>.promo`**: `metadata.json`
plus `Resources/` holding the media it names. The file is the interface —
everything below writes, checks, or renders that file, and a project you
author here opens in the PromoShot apps unchanged.

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
   - `promo_device_model` — a built-in phone, tablet or laptop body
     written into `Resources/` with `Body` and `Screen` slots (the laptop
     a `Deck`), and the resource entry to add. A screenshot or a screen
     recording bound to `Screen`, the accent on `Body`, a camera keyed
     to turn it: the device shot, as a model.
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
   (keyframe fields). Its schema is in the tool; ids are the file's own. Hand-editing the JSON
   stays first-class for anything a recipe shows. The authoring tools and validate each
   answer with a small thumbnail ATTACHED — a keyframe's glance looks
   where the motion arrives; the others sample the touched layer's
   midpoint, past its fadeIn — and write the same image to
   `Exports/preview.png`. Look at it before the next edit; it is the
   editor viewport. (`preview: false` turns it off.)
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
5. **Render** — `promo_render_still` at a few moments to LOOK (a mis-aimed
   viewport or an invisible caption costs seconds here, minutes in a
   video), `promo_render_frames` for a sheet of moments across a range,
   then `promo_render_video` for the mp4 or `promo_render_gif` for the
   looping preview. Outputs land in the project's `Exports/` and return
   paths, never bytes.

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

## The rules that are not in the schema

- **Ids are unique strings.** Short mnemonics — "bg", "clip", "k0" — are
  first-class here; the apps mint UUIDs when they adopt the file. The
  tools speak the same language: pass your own short ids (`id`,
  `resourceId`; init's background layer is always "bg") and only what
  you leave unnamed gets a canonical UUID. Never reuse a spelling:
  validate names the collision.
- **Stamp `"minReaderVersion": 19`** and think no more about it.
- **Measure what you place.** A placed image resource wants
  `pixelWidth`/`pixelHeight` (videos: `videoNaturalWidth`/`Height`) or
  the rule anchors a square guess — validate says so. `promo_upsert_layer`
  stamps them for you.
- **Look before you ship.** The attached thumbnails are the running
  glance, but a full-size render is the honest check of layout —
  validation cannot see that a caption sits on top of the subject, and a
  480px preview can hide small text sitting slightly wrong.
- **Two captions never cross-fade**, and cross-dissolving layers need
  OVERLAP — simultaneous end/start flashes the background.
- **The file is shared; every writer merges.** A person opening your
  project in PromoShot does not end your work: the app adopts external
  edits into the open editor — a clean document simply becomes what disk
  says, and while the person holds unsaved work their edits win
  conflicts while your ADDITIONS still land. Adoption mints short ids
  to STABLE UUIDs (the same spelling in the same project always mints
  the same one), so re-run `promo_inspect` after a person's turn and
  re-anchor; its `updated:` line is the turn signal.
- **ProRes and alpha out.** `promo_render_video` takes `codec:
  "prores422" | "prores4444"` (give it a `.mov` out path) and `alpha:
  true` — the project renders over nothing and the frames' alpha lands
  in a ProRes 4444, the honest hand-off to another editor. Alpha IN
  works too: a ProRes 4444, WebM-with-alpha or PNG-sequence source
  composes with its transparency.
- **A look from a `.cube`.** Copy the file into `Resources/`, declare
  `{ "kind": "lut", "filename": "look.cube", ... }`, and name it from a
  layer's adjustments: `"adjustments": { "lutResourceID": "<uuid>",
  "lutAmount": 0.8 }` — applied after saturation, contrast, brightness
  and tint, on every host alike. Stamp `minReaderVersion: 23`.
- **Models.** A `.glb` in `Resources/` as a resource of `"kind":
  "model"`, shown by a layer of `"kind": "model"`. The layer is a square
  picture of the model: `zoom` 1 fills the canvas height with it, and
  `placement` sizes it like an image. Keyframes take `"camera": { "yaw":
  -25, "pitch": 10, "distance": 4.2, "fov": 30 }` and `"light": { "yaw":
  40, "pitch": 50 }`, which ramp — a yaw from -60 to 20 over three
  seconds is a turntable. `"materials": { "Body": "@accent", "Screen": { "resourceID":
  "<image uuid>" } }` on the resource paints a slot by the name the file
  exports — a colour, or an image or video resource drawn on that
  surface (a screen recording plays on the screen). The object form
  also takes a FINISH over the file's own: `"Body": { "colorHex":
  "@accent", "metallic": 1, "roughness": 0.12 }` is chrome, `{
  "metallic": 0, "roughness": 0.85 }` matte (each 0…1; what is left
  out keeps the file's value), so one `.glb` serves every look. A
  picture on a slot is a SCREEN by default — unlit, fitted, a finish
  on it does nothing; `"Body": { "resourceID": "<uuid>", "mode":
  "surface", "roughness": 0.3, "repeat": [2, 1] }` WEARS it instead:
  the picture becomes the slot's colour under the light and the
  finish (a label on a vase, a print on a box, a video on a glossy
  wall), tiled by `repeat`, shifted by `offset`; where it is
  transparent the slot's own colour shows. A file with
  animations lists them under `clips` after import; `"clip":
  { "name": "Open" }` on a keyframe plays one on layer time, and a
  `time` scrubs it. Stamp
  `minReaderVersion: 29`, or `32` when a finish (or a colour in the
  object form) is written, or `38` when a picture is worn. `promo_inspect` lists the layer; a missing
  file is named like any other.
- **Particles.** Confetti, sparks, snow: a resource `{ "kind":
  "particles", "filename": "", "displayName": "Confetti", "addedAt": 0,
  "particles": { "anchor": [0.5, 0.1], "extent": [0.6, 0], "burst": 200,
  "rate": 0, "direction": 270, "spread": 40, "speed": [0.2, 0.5],
  "gravity": 0.6, "drag": 0.6, "size": [0.01, 0.02], "shape": "square",
  "colors": ["@accent", "FFFFFF"], "life": [2, 3] } }` played by a
  DRAWING layer (`"kind": "drawing"`, `resourceID` the recipe) whose
  start is the burst's instant. Lengths in canvas heights, anchor in unit
  canvas fractions, direction 0 right / 90 up / 270 down; `rate` per
  second for a stream (snow: rate 30, direction 270, spread 20, gravity
  0.2, turbulence 0.02, shape dot, colors white), `burst` for one bang.
  Deterministic — the same frame every time. Stamp `minReaderVersion:
  36`.
- **A body bursting into a word.** Particles in a STAGE are a morph:
  `{ "kind": "particles", "filename": "", "displayName": "Points",
  "addedAt": 0, "particles": { "colors": ["@accent", "FFFFFF"],
  "morph": { "from": "<cube uuid>", "to": "<word uuid>", "count":
  3000, "spread": 1.2 } } }` played by a DRAWING member of the stage
  both bodies are in, whose keyframes ramp `"progress"` 0 → 1 (with
  easing): the points sit on the first body at 0, fly out, and settle
  on the second at 1. End the first body's member as they leave and
  start the second's as they land, so the swap is seamless. Stamp
  `minReaderVersion: 39`.
- **A body from parts.** Anything a product shot needs that is not a
  device or type — a stand, a plinth, a ring, a puck — is a model
  resource with a parts recipe, written like an SVG: `{ "kind": "model",
  "filename": "", "displayName": "Stand", "addedAt": 0, "recipe": {
  "parts": [ { "slot": "Base", "shape": { "cylinder": { "radius": 0.6,
  "height": 0.08 } } }, { "slot": "Stem", "shape": { "lathe": {
  "profile": [[0.1, 0], [0.06, 0.6], [0.16, 0.8]] } } }, { "slot":
  "Plate", "shape": { "box": { "size": [1.2, 0.05, 0.8], "radius": 0.02
  } }, "position": [0, 0.82, 0] } ] } }`. Shapes: box (size [w,h,d],
  radius; `"faces": true` makes six slots, slot/front, /back, /left,
  /right, /top, /bottom, one picture per side — rung 39), sphere (radius), cylinder (radius, height), torus (radius,
  tube), lathe (profile of [radius, height] about Y), extrude (a closed
  [x,y] path pulled `depth` along Z); `position`, `rotation` (degrees
  X,Y,Z) and `scale` place each; `slot` names take colour and finish
  bindings. Units are the body's own; place it on a stage beside a
  device or a vase (positions there are in stage radii). Stamp
  `minReaderVersion: 37`.
- **Environment.** Chrome and gloss need something to mirror: put
  `"environment": { "preset": "studio" }` (or `sunset`, `night`;
  `intensity`, `rotation` in degrees) in `compositionSettings` for any
  project with a metal or a glossy finish. Without it metals mirror a
  synthetic sky and read dark on a dark theme. Stamp `minReaderVersion:
  35`.
- **Text as a body.** A title that belongs IN the scene — lit by the
  stage's light, chrome or matte, turning, standing between devices —
  is a model resource with a recipe and no file: `{ "kind": "model",
  "filename": "", "displayName": "Title", "addedAt": 0, "recipe": {
  "text": { "text": "Hello", "bold": true, "depth": 0.25 } },
  "materials": { "Face": { "colorHex": "@accent", "metallic": 1,
  "roughness": 0.15 }, "Side": "@edge" } }`, played by a model layer or a
  stage member like any body (the text is 1 em tall × `size` world units,
  default 1; `depth` in em, default 0.25; `fontFamily`, `bold`,
  `italic` as a caption's). Keep a body to one line; long text is a
  caption. Stamp `minReaderVersion: 34`. A caption's 2.5D `depth`/tilt is
  the flat trick and legacy — reach for the body. A DEVICE is a recipe
  too: `"recipe": { "device": { "kind": "phone" } }` (tablet, laptop),
  no file to copy — bind the screenshot to its `Screen` slot.
- **Stages.** Prefer the ONE-LAYER form (rung 33): a layer of `"kind":
  "stage"` whose keyframes carry the `"camera"`, `"light"` and
  `"placement"`, and whose `"members"` are the model, image, video,
  caption or drawing layers inside it, each with its own `"depth"`,
  `"stageOffset"` and turn (`"camera"` on a member turns that member).
  One depth: a member is never a stage and names no stage; the stage
  layer has no `resourceID`. Stamp `minReaderVersion: 33`. The flat form
  below still reads and draws the same picture, but it is legacy: the app
  rewrites it as a stage layer on open.
  Layers naming the same `"stage": "hero"` draw through one
  camera into one depth buffer: models at their keyframes' `"depth"`
  (in stage radii, + toward the viewer) and `"stageOffset": [right, up]`
  in the same radii (two devices side by side; rung 31), images and
  videos as billboards
  facing the camera (their `zoom` sizes them against the stage's
  radius). The first member (lowest `sortIndex`) owns the picture: its
  `camera`/`light` are the stage's and its placement, opacity and
  effects apply to the whole stage. A phone at +1, a laptop at -1 and a
  screenshot between them is the reason; caption and drawing members
  stand in the scene as billboards at their depth, and a model member
  that is not first turns in place by its own `camera` yaw/pitch. Stamp `minReaderVersion:
  30` (31 with `stageOffset`).
- **Kinetic reveals.** Beside `wipe`, `fade`, `rise` and `scale`, a
  reveal's `mode` can be `flip` (each unit turns in edge-on, in
  perspective), `tumble` (rights itself from a lean while rising) or
  `slide` (in from the right). Stamp `minReaderVersion: 28`. By word,
  with `"seconds": 1.2`, is the kinetic-type look; by character is
  busier and wants short text.
- **No 2.5D.** A caption's `tiltX`/`tiltY` and `captionStyle.depth`
  (stacked copies) and the device `frame` on a picture are the flat
  compositor's old tricks: they still render, `promo_validate` names
  them legacy, and new work does not use them. A title with a side or a
  lean is a TEXT BODY in a stage (above); a device is a device BODY with
  the picture on its Screen slot.
- **Follow the pointer.** A recording made in the Mac app carries
  `"pointer": { "samples": [[t, x, y], …], "clicks": [[t, x, y], …] }`
  (source seconds, unit coordinates). Put `"follow": { "zoom": 2,
  "smoothing": 0.35 }` on the layer that shows it and the viewport
  follows the pointer — a smooth auto-zoom with click rings — with no
  keyframes to place. `"clicks": false` drops the rings;
  `"clickColorHex"` colours them. Stamp `minReaderVersion: 26`.
- **Ten transition kinds.** `transitionIn`/`transitionOut` on a layer, or
  `transition` on a swap keyframe: fade, wipe, slide, push, scale, and —
  stamp `minReaderVersion: 25` — `blurDissolve` (a fade that sharpens
  as it lands), `zoom` (in from larger and soft; at a swap the old one is
  pushed out through the zoom), `flash` (through white), `glitch` (torn
  bands and split channels for the duration), `dip` (through black).
  `{ "kind": "blurDissolve", "duration": 0.6, "easing": "easeOut" }`.
- **Image effects on a layer.** `"effects": { "blur": 12, "glow": 0.5,
  "vignette": 0.4, "grain": 0.15, "sharpen": 0.5 }` on any layer, each
  optional; `"blurAngle": 45` makes the blur a directional smear, and
  `glowRadius` / `glowThreshold` / `vignetteSoftness` tune the rest. Put
  `blur`, `glow` or `vignette` on a KEYFRAME to ramp it — a blur-in
  headline is `"blur": 20` at 0 and `"blur": 0` a third of a second
  later. Stamp `minReaderVersion: 24`.
- **Chroma key on a layer.** `"chromaKey": { "colorHex": "00FF00",
  "tolerance": 0.3, "softness": 0.1 }` on a video or image layer makes
  the plate transparent before the grade, border and mask — footage on
  a green screen composes over anything. Stamp `minReaderVersion: 22`.
- **Audio effects on a resource.** `"audioEffects": [{ "kind": "normalize",
  "targetLufs": -16 }, { "kind": "compressor" }, { "kind": "eq",
  "frequencyHz": 1000, "gainDb": 3 }]` on a video or audio resource runs
  before the mix, in order, in every render the core makes (the apps'
  live preview plays the resource dry). Stamp `minReaderVersion: 21`.
- **Markers and chapters.** `"markers": [{ "id": "<uuid>", "time": 12.5,
  "name": "Pricing", "kind": "chapter" }]` on the project names moments;
  `chapter` markers are written into an exported mp4's chapter list,
  `marker` ones are notes the editors show. Stamp `minReaderVersion: 20`.
- **Long sources: build proxies once.** `promo_proxy {project}` makes a
  tier-1 proxy (960 px long edge, every frame a keyframe) for each video
  resource, in a cache outside the package. Stills, frames and small
  renders then read proxies by default (`proxy: "auto"`), so an hour-long
  4K source scrubs and renders like a short one; `proxy: "off"` reads the
  source, and a full-size render always does.
- **Reuse a card: make it a composition.** A resource of `kind:
  "composition"` carries its own `canvasWidth`/`canvasHeight`, an optional
  `backgroundColorHex` plate (absent = transparent) and ordinary `layers`
  that reference THIS project's resources by id; give it a `duration` and
  `pixelWidth`/`pixelHeight` equal to its canvas. Place it with a `video`
  layer, as many times as you like — placement rules, trims, speed,
  transitions and fades all apply, and its sound comes along. Edit the
  card once and every placement follows. A composition may not contain
  itself, and nests at most eight deep; validate says so.
- Colours can be palette names (`"@accent"`); an undefined name renders
  BLACK and validate names it. `@edge` is what a device frame's border
  reads by default — define it when you frame.
- For editor autocomplete in hand-written files, point `"$schema"` at
  `docs/promo.schema.json` in this repo.

## Design guidance for store work

One short headline per shot, above the device; same background and
typography across the set; the headline must describe what the picture
shows. Prefer a canvas the source drops into at native size. A set of
stills is a slideshow with hard cuts — every frame is then a finished
screenshot, and the same project doubles as a promo reel.

Pace a MOVING piece like a presentation, not a drift: one eased ramp
across a whole clip reads slow. Move a stage camera in two or three
quick eased-OUT moves of 0.8–1.3 s with holds of about 2 s between (a
hold is a keyframe repeating the values, reached by a long linear ramp),
let the key light fly AHEAD of the camera on every move and drift a
little during the holds, reveal a caption in a second or less, and cut
between beats rather than cross-fading.

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
