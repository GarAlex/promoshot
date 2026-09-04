# .promo, the short course

A project is a FOLDER: `metadata.json` + `Resources/` holding the media it
names. Write the JSON, `promo validate` it (the validator runs the
renderers' own parser — "ok" means it renders), then render or open it.
This is the authoring subset; `promo_schema_full` is the whole format.

- Stamp `"minReaderVersion": 34`. Ids are strings, unique in the file —
  short mnemonics ("bg", "clip", "k0") are fine; apps mint UUIDs on adopt.
- Boilerplate every project carries: `id`, `name`, `createdAt: 0`,
  `state: "recorded"`, `trimStart: 0`, `trimEnd: <seconds>`,
  `videoDuration: <seconds>`, `subtitles: []`.
- `compositionSettings`: `canvasWidth`/`canvasHeight` (the render size),
  `backgroundColorHex`, the `subtitle*` caption defaults, and `palette` —
  named colours any colour field references as `"@name"`.
- `layers`, drawn low `sortIndex` first. Kinds: `background`, `video`,
  `image`, `caption`, `drawing` (audio kinds add sound, not picture).
  A layer: `id`, `name`, `sortIndex`, `kind`, `isEnabled: true`,
  `startTime`, `duration` (seconds on the output timeline), usually a
  `resourceID`, and `keyframes`.
- `resources` hold per-asset state: `id`, `kind`, `filename` (in
  `Resources/`), `displayName`, `addedAt: 0`, `imageCuts: []`,
  `disabledAudioTrackIndices: []`; videos also carry `duration`,
  `trimStart`, `trimEnd`; images `pixelWidth`/`pixelHeight` — placement
  anchors against the aspect, and an unmeasured source positions as a
  SQUARE (validate names it). Declare what you use.

Keyframes animate a layer over its LOCAL time, hold-then-ease: a value
holds until `transitionDuration` seconds before the keyframe, then ramps
into it; `"easing": "easeInOut"` shapes the ramp. One keyframe is a
constant. The fields that matter first:

- `placement` — size/position as a RULE: one of `height`/`width` (canvas
  px) or `mode: "fit"|"fill"`, plus `anchor` (nine-point grid,
  `topLeft`…`bottomRight`) and `offset: [x, y]`. Reach for this before
  zoom/shift arithmetic; it re-resolves if the media changes.
- `opacity` 0..1; `rotation` degrees. A model layer's `camera` (yaw,
  pitch, distance in bounds radii) turns a body.
- `viewport: [x, y, w, h]` in UNIT source coordinates — the window of the
  source the layer shows. `[0,0,1,1]` is everything; ramping to
  `[0.25,0.25,0.5,0.5]` is the Ken Burns push. Keep `w == h` to keep the
  layer's shape; the layer's own rect never moves, only what it shows.
- `resourceID` on a keyframe SWAPS what the layer shows (image, caption,
  drawing layers only — never video): a sequence on ONE layer. Add
  `"transition": { "kind": "wipe", "from": "left", "duration": 0.6 }` and
  the cut becomes a blend. This is how a still-image slideshow crossfades
  with no second layer.

How a layer enters and leaves: `fadeIn`/`fadeOut` (seconds) are the plain
dissolve; `transitionIn`/`transitionOut`
`{ "kind": "fade|wipe|slide|push|scale", "from": "left|right|top|bottom",
"duration": s, "easing": … }` are the shaped kinds. Overlap two layers by
the transition length — simultaneous end/start flashes the background.

Captions: `captionText` on the layer; `captionStyle` overrides the
composition's `subtitle*` defaults per caption — `fontSize` (points),
`alignment`, `tracking` (letter spacing in points), `weight`
("ultraLight".."black", winning over `isBold`), `lineHeight` (a multiple
of the size, 1.25 unless said), stroke (`strokeColorHex`/`strokeWidth`)
and shadow (`shadowOpacity`/`shadowRadius`) for text on footage, and
`placement: { "anchor": "bottom", "offset": [0, -60] }` to hang the box on
the same grid media uses (anchor+offset only; margins then only set wrap
width). A keyframe `fontSize` animates the size in points. Two captions
must never cross-fade; give each a life clear of both dissolves.

A device is a BODY, and the easiest body is a RECIPE — a model resource
with no file: `"recipe": { "device": { "kind": "phone" } }` (tablet,
laptop too) is a model with `Body` and `Screen` slots the engine builds
at load — a laptop has a `Deck` as well. Or from
a file: `promo device phone --out phone.glb`. A model RESOURCE names the
recipe or the file and binds the slots — `"materials": { "Body": "@edge",
"Screen": { "resourceID": "<shot>" } }` puts the screenshot on the
screen — and a model LAYER places it; `placement` sizes the whole body,
its keyframes' `camera` turns it. The old `frame` on a picture resource
is legacy 2.5D and `promo_validate` says so. `"kind": "border"` with
`borderWidth`/`cornerRadius` is still the flat card.

Four recipes. Each is a complete `metadata.json` — copy one beside your
media, rename ids/filenames, validate.

**16:9 product card** — a phone body with the screenshot on its screen
pushes in over a palette ground, title above, 6 seconds:

```json
{"id":"card","name":"Product Card","createdAt":0,"state":"recorded",
 "minReaderVersion":34,"trimStart":0,"trimEnd":6,"videoDuration":6,
 "subtitles":[],
 "compositionSettings":{"canvasWidth":1920,"canvasHeight":1080,
   "palette":[{"name":"canvas","colorHex":"10182B"},
              {"name":"text","colorHex":"F3F5FF"},
              {"name":"edge","colorHex":"26364F"}],
   "backgroundColorHex":"@canvas","subtitleColorHex":"@text",
   "subtitleFontSize":72,"subtitleBold":true},
 "resources":[{"id":"shot","kind":"image","filename":"shot.png",
   "displayName":"Screenshot","addedAt":0,"imageCuts":[],
   "disabledAudioTrackIndices":[],
   "pixelWidth":1290,"pixelHeight":2796},
  {"id":"phone","kind":"model","filename":"phone.glb","displayName":"Phone",
   "addedAt":0,"imageCuts":[],"disabledAudioTrackIndices":[],
   "clips":[],"boundsRadius":0.4097,
   "materials":{"Body":"@edge","Screen":{"resourceID":"shot"}}}],
 "layers":[
  {"id":"bg","name":"Ground","sortIndex":0,"kind":"background",
   "isEnabled":true,"startTime":0,"duration":6,"keyframes":[]},
  {"id":"card1","name":"Phone","sortIndex":1,"kind":"model",
   "isEnabled":true,"startTime":0,"duration":6,"resourceID":"phone",
   "fadeIn":0.4,
   "keyframes":[
    {"id":"k0","time":0,"transitionDuration":0,
     "placement":{"height":640,"anchor":"center","offset":[0,40]},
     "camera":{"yaw":-14,"pitch":6,"distance":4.6}},
    {"id":"k1","time":5.5,"transitionDuration":5.0,"easing":"easeInOut",
     "placement":{"height":720,"anchor":"center","offset":[0,40]},
     "camera":{"yaw":-6,"pitch":6,"distance":4.4}}]},
  {"id":"title","name":"Title","sortIndex":2,"kind":"caption",
   "isEnabled":true,"startTime":0.4,"duration":5.6,
   "captionText":"Meet the new dashboard",
   "captionStyle":{"placement":{"anchor":"top","offset":[0,72]}},
   "keyframes":[
    {"id":"t0","time":0,"opacity":0,"transitionDuration":0},
    {"id":"t1","time":0.3,"opacity":1,"transitionDuration":0.3}]}]}
```

**Two-clip sequence with a wipe** — video layers cannot swap, so two
clips are two layers, overlapped by the wipe (for STILLS, do this on one
layer with a swap keyframe carrying the same `transition` object):

```json
{"id":"seq","name":"Two Clips","createdAt":0,"state":"recorded",
 "minReaderVersion":34,"trimStart":0,"trimEnd":9.4,"videoDuration":9.4,
 "subtitles":[],
 "compositionSettings":{"canvasWidth":1920,"canvasHeight":1080,
   "backgroundColorHex":"0E1726"},
 "resources":[
  {"id":"clipA","kind":"video","filename":"a.mp4","displayName":"A",
   "addedAt":0,"duration":5,"trimStart":0,"trimEnd":5,"imageCuts":[],
   "disabledAudioTrackIndices":[]},
  {"id":"clipB","kind":"video","filename":"b.mp4","displayName":"B",
   "addedAt":0,"duration":5,"trimStart":0,"trimEnd":5,"imageCuts":[],
   "disabledAudioTrackIndices":[]}],
 "layers":[
  {"id":"bg","name":"Ground","sortIndex":0,"kind":"background",
   "isEnabled":true,"startTime":0,"duration":9.4,"keyframes":[]},
  {"id":"first","name":"Clip A","sortIndex":1,"kind":"video",
   "isEnabled":true,"startTime":0,"duration":5,"resourceID":"clipA",
   "fadeIn":0.3,"keyframes":[]},
  {"id":"second","name":"Clip B","sortIndex":2,"kind":"video",
   "isEnabled":true,"startTime":4.4,"duration":5,"resourceID":"clipB",
   "transitionIn":{"kind":"wipe","from":"left","duration":0.6},
   "keyframes":[]}]}
```

**Recording, Ken Burns viewport, lower caption** — the frame stays put;
what it SHOWS pushes in to the top-left quarter:

```json
{"id":"kb","name":"Focus Push","createdAt":0,"state":"recorded",
 "minReaderVersion":34,"trimStart":0,"trimEnd":8,"videoDuration":8,
 "subtitles":[],
 "compositionSettings":{"canvasWidth":1920,"canvasHeight":1080,
   "palette":[{"name":"text","colorHex":"FFFFFF"}],
   "backgroundColorHex":"0B1020","subtitleColorHex":"@text",
   "subtitleFontSize":54,"subtitleBold":true},
 "resources":[{"id":"rec","kind":"video","filename":"recording.mp4",
   "displayName":"Recording","addedAt":0,"duration":8,"trimStart":0,
   "trimEnd":8,"imageCuts":[],"disabledAudioTrackIndices":[]}],
 "layers":[
  {"id":"bg","name":"Ground","sortIndex":0,"kind":"background",
   "isEnabled":true,"startTime":0,"duration":8,"keyframes":[]},
  {"id":"screen","name":"Recording","sortIndex":1,"kind":"video",
   "isEnabled":true,"startTime":0,"duration":8,"resourceID":"rec",
   "fadeIn":0.3,
   "keyframes":[
    {"id":"v0","time":1,"viewport":[0,0,1,1],"transitionDuration":0},
    {"id":"v1","time":6,"viewport":[0.02,0.02,0.5,0.5],
     "transitionDuration":4.0,"easing":"easeInOut"}]},
  {"id":"cap","name":"Caption","sortIndex":2,"kind":"caption",
   "isEnabled":true,"startTime":1,"duration":6.5,
   "captionText":"Everything lives in one panel",
   "captionStyle":{"placement":{"anchor":"bottom","offset":[0,-48]},
                    "strokeColorHex":"0A0A0A","strokeWidth":5,
                    "shadowOpacity":0.5,"shadowRadius":8},
   "keyframes":[
    {"id":"c0","time":0,"opacity":0,"transitionDuration":0},
    {"id":"c1","time":0.3,"opacity":1,"transitionDuration":0.3}]}]}
```

**9:16 story** — the SAME layers as the card; only the canvas and the
placements change. Placement rules are why this is a re-stamp, not a
redesign:

```json
{"id":"story","name":"Story","createdAt":0,"state":"recorded",
 "minReaderVersion":34,"trimStart":0,"trimEnd":6,"videoDuration":6,
 "subtitles":[],
 "compositionSettings":{"canvasWidth":1080,"canvasHeight":1920,
   "palette":[{"name":"canvas","colorHex":"10182B"},
              {"name":"text","colorHex":"F3F5FF"},
              {"name":"edge","colorHex":"26364F"}],
   "backgroundColorHex":"@canvas","subtitleColorHex":"@text",
   "subtitleFontSize":64,"subtitleBold":true},
 "resources":[{"id":"shot","kind":"image","filename":"shot.png",
   "displayName":"Screenshot","addedAt":0,"imageCuts":[],
   "disabledAudioTrackIndices":[],
   "pixelWidth":1290,"pixelHeight":2796},
  {"id":"phone","kind":"model","filename":"phone.glb","displayName":"Phone",
   "addedAt":0,"imageCuts":[],"disabledAudioTrackIndices":[],
   "clips":[],"boundsRadius":0.4097,
   "materials":{"Body":"@edge","Screen":{"resourceID":"shot"}}}],
 "layers":[
  {"id":"bg","name":"Ground","sortIndex":0,"kind":"background",
   "isEnabled":true,"startTime":0,"duration":6,"keyframes":[]},
  {"id":"card1","name":"Phone","sortIndex":1,"kind":"model",
   "isEnabled":true,"startTime":0,"duration":6,"resourceID":"phone",
   "fadeIn":0.4,
   "keyframes":[
    {"id":"k0","time":0,"transitionDuration":0,
     "placement":{"height":900,"anchor":"center","offset":[0,60]},
     "camera":{"yaw":-14,"pitch":6,"distance":4.6}},
    {"id":"k1","time":5.5,"transitionDuration":5.0,"easing":"easeInOut",
     "placement":{"height":1000,"anchor":"center","offset":[0,60]},
     "camera":{"yaw":-6,"pitch":6,"distance":4.4}}]},
  {"id":"title","name":"Title","sortIndex":2,"kind":"caption",
   "isEnabled":true,"startTime":0.4,"duration":5.6,
   "captionText":"Meet the new dashboard",
   "captionStyle":{"placement":{"anchor":"top","offset":[0,160]}},
   "keyframes":[
    {"id":"t0","time":0,"opacity":0,"transitionDuration":0},
    {"id":"t1","time":0.3,"opacity":1,"transitionDuration":0.3}]}]}
```

Everything else — sprites, masks, motion paths, drawings, narration,
duration rules, waits, gradients, tiled plates, palette roles — is in
`promo_schema_full`, with the same one-file authority behind both. And
`promo_schema_types` is the format as a JSON Schema, generated from the
parser's own structs — fill a structured object against it instead of
freehanding, and put
`"$schema": "https://raw.githubusercontent.com/GarAlex/promoshot/main/docs/promo.schema.json"`

The rest of the format — nested compositions, markers, audio effects,
chroma key, pointer follow, image effects, LUTs, models, particles,
routes, morphs, parts, recipes, environments, stages — is
`promo_schema_full`. It answers whole (67 KB) or by topic: pass
`{"topics": ["particles", "route"]}` and get those sections alone,
`"core"` for the format proper.
in a metadata.json for editor autocomplete.
