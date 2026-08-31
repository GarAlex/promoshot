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
3. **Author** — tools scaffold; motion is JSON. `promo_init` lays the
   folder, canvas, palette, background; `promo_upsert_layer` adds an
   image/video/caption with a placement, a fadeIn, a device frame —
   media copied in, sizes and durations probed, the composition
   re-stretched every call. Updating by id changes only what you pass
   (placement merges into the first keyframe; hand-added keyframes
   survive). Everything beyond the scaffold — a second placement
   keyframe for a push-in, a viewport ride, a wipe — is an ordinary
   JSON edit; start from the recipe that matches.
4. **Check** — `promo_validate` runs the renderers' own parser, so "ok"
   means "renders"; anything else is a silent correction named before you
   see it in pixels. `promo_inspect` summarizes what is in the project —
   canvas, layers by kind, undefined colours, missing media.
5. **Render** — `promo_render_still` at a few moments to LOOK (a mis-aimed
   viewport or an invisible caption costs seconds here, minutes in a
   video), `promo_render_frames` for a sheet of moments across a range,
   then `promo_render_video` for the mp4. Outputs land in the project's
   `Exports/` and return paths, never bytes.

The `promo` CLI is the same contract (`promo schema | validate | inspect |
still | frames | video`), and `promo_workspace` names a folder for new
projects.

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
- **Look before you ship.** A render is the only honest check of layout;
  validation cannot see that a caption sits on top of the subject.
- **Two captions never cross-fade**, and cross-dissolving layers need
  OVERLAP — simultaneous end/start flashes the background.
- **The app owns the file once it opens it.** If a person opens your
  project in PromoShot, stop editing the JSON by hand; further changes
  race the app.
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

- `promo_open` puts the project in front of the person — and adopting it
  is when the app takes ownership of the file (stop hand-editing then).
- `promo_speak` writes and synthesizes narration with the person's own
  provider key (never yours, never shipped).
- `promo_render_gif` renders the looping GIF the app's Export tab makes.
- Access is per-folder: a tool answering `access_required: <path>` means
  the person approves that folder in PromoShot once, then retry. The
  app's `promo_workspace` names a pre-approved folder.
- Free-tier renders through the app carry the PromoShot watermark,
  exactly as in the app.
