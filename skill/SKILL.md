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
     rotation, channels.
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
   `transition`), a trim (`patchResource`), a canvas change
   (`patchSettings`), waits and motion paths (keyframe fields). Its
   schema is in the tool; ids are the file's own. Hand-editing the JSON
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
- **Stamp `"minReaderVersion": 18`** and think no more about it.
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
