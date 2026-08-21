A PromoShot project is a FOLDER:

  <project>/metadata.json     the composition
  <project>/Resources/        the media — this IS the inventory

A file in Resources/ does not have to be declared to be used. Its id
is uuid5(uuid5(NAMESPACE_URL, "promoshot.resources/" + <project id>),
<filename>), uppercase: compute it, point a layer's resourceID at it,
render. The "resources" array is for editing STATE (trims, cuts,
speed, volume, caption text, a speech spec) — declare an entry only
for what you customize. A declared entry whose file is gone is
reported missing rather than rendering as nothing.

metadata.json (only the fields that matter for authoring):

{
  "id": "<uuid>", "name": "...", "createdAt": 0, "state": "recorded",
  "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
  "compositionSettings": {
    "canvasWidth": 1920, "canvasHeight": 1080, "fps": 60,
    "backgroundColorHex": "0E1726",
    "backgroundGradient": {
      "kind": "linear|radial", "start": [0, 0], "end": [1, 1],
      "repeat": "clamp|repeat|mirror",
      "stops": [ { "colorHex": "0B1026", "at": 0 },
                 { "colorHex": "1B4A8B", "at": 1 } ] },
    "videoCornerRadius": 14, "videoBorderWidth": 1,
    "videoBorderColorHex": "26364F",
    "subtitleFontSize": 54, "subtitleBold": true,
    "subtitleColorHex": "FFFFFF", "subtitleBackgroundOpacity": 0.0,
    "subtitleLeftMargin": 90, "subtitleRightMargin": 90,
    "subtitleVerticalMargin": 950
  },
  "resources": [
    { "id": "<uuid>", "kind": "video|image|audio", "filename": "clip.mp4",
      "displayName": "...", "addedAt": 0, "duration": 6.5,
      "trimStart": 0.15, "trimEnd": 6.35, "looped": false,
      "imageCuts": [], "disabledAudioTrackIndices": [],
      "mediaCuts": [
        { "id": "<uuid>", "name": "The formula bit",
          "trimStart": 12.0, "trimEnd": 18.0 }
      ] },
    { "id": "<uuid>", "kind": "path", "filename": "",
      "displayName": "Swoop", "addedAt": 0,
      "imageCuts": [], "disabledAudioTrackIndices": [],
      "path": { "start": [0, 0], "end": [100, 0],
                "controls": [[50, -60]] } },
    { "id": "<uuid>", "kind": "image", "filename": "walk.png",
      "displayName": "Walk", "addedAt": 0,
      "imageCuts": [], "disabledAudioTrackIndices": [],
      "sampling": "nearest",
      "sprite": { "columns": 4, "rows": 2, "frameCount": 7,
                  "fps": 12 } }
  ],
  "layers": [
    { "id": "<uuid>", "name": "Background", "sortIndex": 0,
      "kind": "background", "isEnabled": true, "startTime": 0,
      "keyframes": [] },
    { "id": "<uuid>", "name": "Clip", "sortIndex": 1, "kind": "video",
      "isEnabled": true, "startTime": 0, "duration": 4.5,
      "resourceID": "<the resource's uuid>", "keyframes": [ ... ] },
    { "id": "<uuid>", "name": "Headline", "sortIndex": 2, "kind": "caption",
      "isEnabled": true, "startTime": 0.45, "duration": 3.6,
      "captionText": "A fast spreadsheet for Mac",
      "captionStyle": { "alignment": "center" }, "keyframes": [ ... ] }
  ]
}

A keyframe animates a layer over its LOCAL time:

  { "id": "<uuid>", "time": 0.45, "zoom": 0.76,
    "horizontalShift": 240, "verticalShift": 150,
    "rotation": 0, "opacity": 1.0, "transitionDuration": 0.45,
    "transitionPercent": 100,
    "viewport": [0.25, 0.25, 0.5, 0.5], "easing": "easeInOut",
    "placement": { "height": 620, "anchor": "center", "offset": [0, -40] },
    "motionPath": { "pathResourceID": "<a path resource's uuid>",
                    "flipped": false, "startAt": 0, "endAt": 1 } }

EVERY id in the file is a UUID string — project, resources, layers,
keyframes, cuts. Short ids like "V0" pass the core's parser (it stores
strings) but the app cannot decode them, so the project will not open.

Semantics worth knowing:

- transitionDuration is the ramp INTO this keyframe's value; the value
  holds until that ramp begins. Two keyframes with a transition make a
  fade or a move; one keyframe is a constant.
- `easing` shapes the ramp INTO this keyframe: "linear" (the
  default, and what every ramp was before this existed), "easeIn"
  (slow start), "easeOut" (slow arrival) or "easeInOut" (smoothstep,
  the workhorse for a camera move). An unknown value falls back to
  linear rather than failing the file. It applies to whatever the
  keyframe carries — zoom, position, viewport, rotation, opacity,
  gradient — and to the SPEED along a motion path, so an eased move
  travels its curve on the clock it grows on. Because each track
  ramps on its own keyframe, one property can ease while another
  does not. Reach for easeInOut on anything that starts and stops:
  a linear zoom or pan reads as mechanical, and this is the single
  cheapest thing that makes a move look intentional.
- Keyframes are PER-TYPE TRACKS: a keyframe speaks only for the
  fields it carries, and each property interpolates over the
  keyframes that define it, each ramp using its own keyframe's
  transition. Zoom keyed at 0/60/120 coexists with movement keyed at
  0/0.5/1.5 on one layer. Two keyframes may share a time, one per
  type — a zoom that ramps through only the last 1s of a 30s move is
  two keyframes at t=30: one carrying the shifts with
  transitionPercent 100, one carrying zoom with transitionDuration 1.
  The two shifts are ONE track (a position is a point, and a motion
  path moves it as one); zoom, rotation, opacity, tilt, viewport,
  gradient and gain are each their own. Within one track, two
  keyframes at the same time resolve by ARRAY ORDER — the later
  wins from that instant on, the same rule layer order plays for z.
- `strokeColorHex` / `strokeWidth` put an OUTLINE round the glyphs, and
  `shadowColorHex` / `shadowOpacity` / `shadowRadius` / `shadowOffset` a
  soft shadow under them. This is what lets a caption sit straight on
  FOOTAGE with `subtitleBackgroundOpacity: 0` — plain white text over a
  bright frame is mush, and a plate reads as a subtitle bar rather than
  a caption. Both live INSIDE `subtitleBackgroundPadding`: the caption
  box is text-plus-padding, so a stroke wider than the padding is
  clipped rather than moving the caption. Give a stroked caption more
  padding than a plain one. Composition-wide defaults are
  `subtitleStrokeColorHex` / `subtitleStrokeWidth` /
  `subtitleShadowColorHex` / `subtitleShadowOpacity` /
  `subtitleShadowRadius`; both are OFF unless asked for.
- `placement` is the drawn box as a RULE instead of numbers, and it
  is the tool to reach for FIRST when sizing or positioning an image
  or video layer: one of `height`/`width` (drawn size in canvas px)
  or `mode` ("fit" contains the canvas, "fill" covers it), plus
  `anchor` (topLeft, top, topRight, left, center, right, bottomLeft,
  bottom, bottomRight — default center) and `offset` ([x, y] px from
  the anchor). It replaces the zoom/shift arithmetic entirely — no
  more `zoom = height/canvasHeight` or centering maths that needs
  the source's pixel size. The rule is stored and re-resolves at
  every read, so it survives swapping the media for another aspect.
  It wins over raw zoom/shifts on the same keyframe; a rule with no
  size is position-only and keeps the keyframe's own zoom. Rules
  ramp like everything else: two placement keyframes blend their
  resolved numbers on the ordinary transition clock, easing and
  motion paths included. Width and anchoring need the source's
  aspect, which comes from the resource's stored `pixelWidth`/
  `pixelHeight` (images; app imports stamp them — include them when
  authoring by hand) or `videoNaturalWidth`/`Height` (videos); a
  sprite resolves against one cell. Without a stored size the rule
  assumes a square source and promo_validate says so. Image and
  video layers. Projects using placement stamp
  `minReaderVersion: 7`.
- `palette` in compositionSettings names colours the project can
  reuse: [{ "name": "accent", "colorHex": "5B8CFF" }, …]. ANY colour
  field may then hold "@accent" instead of a hex value — background,
  border, caption colours, gradient stops, a background keyframe's
  colorHex. Matching ignores case. A name the palette does not
  define is not an error: it falls through to that field's own
  default and `promo_inspect` lists it. Use it when a colour appears
  more than once — re-skinning a project then means editing one
  entry rather than hunting every occurrence. Colours are stored as
  bare `RRGGBB`; a leading `#` is accepted on read.
- backgroundGradient replaces the flat backgroundColorHex, which
  stays as the fallback. `kind` is linear (colours run from `start`
  to `end`) or radial (outward from `start`, reaching the last stop
  at the distance of `end`). `start`/`end` are in UNIT canvas
  coordinates — [0,0] top-left, [1,1] bottom-right — so one gradient
  survives being rendered at several canvas sizes. Up to 8 `stops`,
  each a colour and a position 0…1; out of order or out of range is
  sorted and clamped rather than refused. `repeat` is clamp (the
  default), repeat or mirror.
- A background LAYER's keyframes may carry a `gradient` of the same
  shape, which is how it animates — and animating the axis with a
  repeating ramp is how a gradient SCROLLS: shift `start` and `end`
  by exactly one axis length and the pattern returns to itself, so
  the loop has no seam. Under `clamp` the same animation only drags
  two flat regions across the canvas. `mirror` folds each tile, so it
  cannot band even when the end colours differ. Two gradients blend
  only when `kind`, `repeat` and stop count all match; otherwise the
  change CUTS at the later keyframe, because there is no meaningful
  halfway between a three-stop linear and a two-stop radial.
- transitionPercent is the same ramp as a share of the gap from the
  previous keyframe: 100 starts moving immediately and arrives exactly
  on time, 0 holds still and is simply at the new value when the
  keyframe lands. It wins when both are present, and unlike a duration
  in seconds it still means what it meant after the layer is
  stretched or its material replaced.
- motionPath bends the route between two keyframes without moving
  either end. It names a `path` resource, whose drawn coordinates
  never reach the canvas: the stroke's start is fitted onto the
  previous keyframe's position and its end onto this one's, absorbing
  the scale, rotation and placement it was drawn at. So one curve is a
  swoop for any pair of keyframes at any distance or angle, the bulge
  scaling with the distance, and progress is measured in DISTANCE so
  the move keeps a constant speed through the curve. Optional
  `flipped` mirrors it across the straight line between the
  keyframes; `startAt`/`endAt` are fractions of its length, so a
  partial range trims a tail and startAt above endAt runs it
  backwards. A CLOSED path — one whose ends meet — has no chord to
  aim with, so it is an ORBIT: it plays at its own drawn size around
  the previous keyframe's position and ignores the next one's, which
  means both keyframes want the SAME position or the layer circles
  and then jumps. Two keyframes at one position likewise have no
  direction to fit to, so any path there plays at its drawn size.
  Only position
  follows the path — zoom, rotation and opacity ramp as usual. Without
  a motionPath a layer moves in a straight line.
- A `path` resource is pure metadata, like a caption: no file, never
  reported missing. Its points are [x, y] PAIRS, not {"x":…} objects,
  which is how a point is encoded everywhere in this format.
  `controls` holds 0, 1 or 2 of them — a line, a quadratic curve or a
  cubic; anything past the second is ignored rather than refused.
- A keyframe may carry `resourceID`, swapping what the layer shows
  while everything else animates through it — a sequence on ONE
  layer instead of several with duplicated keyframes. It is a STEP:
  it lands at the keyframe's own time, no transition applies, and
  there is no cross-dissolve between two sources on one layer (that
  is what two overlapping layers are for). The layer's own resourceID
  shows before the first swap. Image and caption layers only — on
  video or audio a mid-layer swap would have to say where the second
  clip starts playing — and only to a resource of the layer's own
  kind; anything else is ignored. Height is preserved across a swap
  (it is canvasHeight * zoom) while width follows the new source's
  aspect, anchored top-left.
- `viewport` is the WINDOW a layer shows of its source: exactly
  [x, y, w, h] in UNIT source coordinates — [0,0,1,1] is the whole
  frame, [0.25,0.25,0.5,0.5] the middle at 2x. Image and video
  layers only. Unlike a swap it RAMPS: the four numbers interpolate
  like zoom, honouring transitionDuration/transitionPercent, and the
  ramp IS the visible zoom-and-pan — the way to follow an app's
  focus through a high-resolution screen recording. The layer's OWN
  rect on the canvas (position, zoom, corner radius, border) does
  not move; only what it shows does. The layer lays out as what it
  shows: drawn height stays canvasHeight * zoom, width follows the
  window's aspect, so keep w == h to preserve the layer's shape
  (unit coordinates make equal shares of width and height match the
  source's own aspect). Keep windows inside 0..1 — the renderer
  clamps a window that hangs outside back in, size first. Zoom past
  sourceHeight/canvasHeight upscales the source. A motion path never
  bends a viewport move: the window always travels straight. On a
  sprite the window is INSIDE the current cell.
- `sprite` on an IMAGE reads that file as a grid of frames instead of
  one picture, cycling over the layer's local time. It is not a
  separate layer or resource kind: a sprite layer is an image layer,
  so it moves, zooms, rotates, fades and follows a motion path exactly
  as any image does — the frame is chosen when sampling and the
  movement happens in the geometry, and the two never interact.
  Frames run left to right, top to bottom. `frameCount` is for a sheet
  whose last row is short (10 frames in a 4x3 grid) and stops the
  cycle stepping through the empty cells; absent means the whole grid.
  `fps` is 12 when absent. `frameDurations` is an array of seconds for
  a source that holds some frames longer than others — it must have
  exactly `frameCount` entries, all positive, or it is ignored whole.
  The layer LAYS OUT at one frame's size, not the sheet's: a 256x128
  sheet of 4x2 frames places as 64x64.
- A sprite REPEATS by default, which is the opposite of a video layer:
  its material is a cycle rather than a recording. `beyondEnd: "hold"`
  freezes it on the last frame once the cycle is spent and `"hide"`
  stops drawing.
- `sampling` is "smooth" (the default, bilinear) or "nearest". Pixel
  art needs "nearest" to survive being scaled up, and a sprite sheet
  needs it for CORRECTNESS — smoothing samples across a cell's edge
  and blends in the frame beside it.
- opacity is 0..1 and defaults to 1. Cross-dissolve by overlapping two
  layers in time and fading one down as the other comes up.
- Layer placement: a media layer's rect has its TOP-LEFT at
  (horizontalShift, verticalShift) in canvas coordinates, and is scaled
  by (canvasHeight / sourceHeight) * zoom. So zoom = scale *
  sourceHeight / canvasHeight.
- Captions are placed by the subtitle margins, and
  subtitleVerticalMargin is measured from the TOP of the canvas.
- A caption layer reuses the keyframe fields for its STYLE, with
  absolute values: zoom = font size, verticalShift = vertical margin,
  horizontalShift = left margin. They interpolate like any other
  value, so a title can start centred and animate into place. An
  omitted field falls back to the base style, not to the previous
  keyframe.
- captionStyle may carry fontSize, so one caption can be far larger
  than the composition default.
- speed on a resource or a cut is the playback rate; 1.5 plays half
  again as fast, so the material occupies two thirds of the timeline.
  Audio keeps its pitch, which makes it the cheap way to fit a
  narration line to its beat — better than re-synthesizing, since TTS
  returns a different duration every time it is asked.
- mediaCuts are named sub-ranges of ONE video or audio file, the same
  idea as imageCuts but in time. A layer plays one by naming it in
  `mediaCutID`; without that it plays the resource's own trim. A cut
  may carry its own trimKeyframes, so a single cut can skip a dull
  stretch in the middle. Use cuts instead of importing the same
  recording several times.
- beyondEnd on a LAYER says what happens once it outlives its source:
  "hold" freezes the last frame (the default), "loop" starts over,
  "hide" stops drawing. Without it a layer may not be given a duration
  longer than its material. Looping belongs to the layer, not the
  file — the same recording can loop under one layer and freeze under
  another. `looped` on the RESOURCE is the blunter form of the same
  idea, for material that is inherently a loop: it repeats for every
  layer that plays it.
- fps is optional and renders at 30 when absent. Screen recordings are
  captured at up to 60, so a scrolling UI demo wants 60 (or 59.94, the
  rate a Mac capture actually reports); 24 makes scrolling judder.
- Layer kinds: background, video, image, drawing, caption, audio.
  sortIndex is z-order, low to high.
- An audio resource may carry `speech` instead of an existing file:
  { "text": ..., "provider": "openai", "voiceID": "alloy" }. Call
  promo_speak to synthesize it; the app fills in filename and duration.
  `renderedHash` is written by the app — do not set it by hand, it is
  what stops unchanged text from being paid for twice.
