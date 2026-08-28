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
  "minReaderVersion": 17, "trimStart": 0, "trimEnd": 0,
  "videoDuration": 0, "subtitles": [],
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
    "videoShadowColorHex": "000000", "videoShadowOpacity": 0.35,
    "videoShadowRadius": 24, "videoShadowOffset": [0, 12],
    "subtitleFontFamily": "system", "subtitleFontSize": 54,
    "subtitleBold": true,
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
      "fadeIn": 0.28, "fadeOut": 0.4,
      "captionText": "A fast spreadsheet for Mac",
      "captionStyle": { "alignment": "center" }, "keyframes": [ ... ] }
  ]
}

The format is ONE version: stamp "minReaderVersion": 17 at the top
level and think no more about it. promo_validate warns when a file
claims a smaller number than its fields use.

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
  gradient, gain, shutter and the grade scalars are each their own.
  Within one track, two
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
  `subtitleShadowRadius` / `subtitleShadowOffset`; both are OFF
  unless asked for.
- A `background` RESOURCE is what a background LAYER shows when its
  `resourceID` names one: `{ "kind": "background", "filename":
  "plate.png", "background": { "fill": "stretch|fit|tile", "colorHex":
  "0E1726", "gradient": { …same shape as backgroundGradient… },
  "anchor": [0, 0] } }`. The colour/gradient are the plate's own
  ground (gradient wins); an image `filename` draws over it per
  `fill` — stretched edge to edge, aspect-FIT with the ground showing
  around it, or TILED from `anchor` (unit canvas coordinates, the
  gradient precedent) at the image's own pixel size times `scale`
  (2 draws each tile twice as large; absent is 1). The image's NATIVE
  size sets the tile — declare `pixelWidth`/`pixelHeight` on the
  resource so previews rendering from downsampled bitmaps agree with
  exports. Background
  plates are scenery, not media: never bordered, cornered or
  shadowed. The background layer's KEYFRAMES compose as everywhere
  else: `colorHex`/`gradient` keyframes override the plate's ground
  on the usual ramps, a keyframe `resourceID` REPLACES the plate (the
  swap rule now covers background layers), and the layer's
  shift keyframes scroll a tiled plate's anchor on the eased position
  track. The kind decodes strictly, so a project holding one refuses
  to open in older readers.
- `libraryID` on a resource is APP bookkeeping: the record is a link
  into that device's shared resource library, whose folder its
  `filename` resolves in. Renderers ignore it; the reading app treats
  a link it cannot resolve as ordinary missing media; the app's
  ARCHIVE step (the self-contained `.promo` interchange form) embeds
  the referenced files and strips the field. Authoring tools should
  not write it.
- Media layers (video/image) cast the same kind of shadow:
  `videoShadowColorHex` / `videoShadowOpacity` / `videoShadowRadius` /
  `videoShadowOffset` in compositionSettings put a soft drop shadow
  under EVERY media layer's drawn rect — corner radius, zoom, rotation
  and transitions included. OFF by default (opacity 0). Radius is the
  penumbra length in canvas px, offset canvas px too; both scale with
  the layer's zoom so a bigger card casts a bigger shadow, and an
  absent offset derives the caption drop: straight down by half the
  blur. A resource's `frame` may override PER FIELD with
  `shadowColorHex` / `shadowOpacity` / `shadowRadius` / `shadowOffset`
  (authored against the same 1080-wide reference as its
  `borderWidth`); each absent field inherits the composition default.
  Slab-framed (3D box) and masked layers cast nothing — their
  silhouettes are not the rect. Cosmetic like `easing`, so no rung: an
  older reader draws the same layout, minus the shadow.
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
  authoring by hand) or `videoNaturalWidth`/`videoNaturalHeight`
  (videos); a sprite resolves against one cell. A `frame` of kind
  "device" resolves against the SLAB, not the picture inside it: the
  device is baked around the image before anything lays the layer
  out, so it is wider than its screenshot and a different shape, and
  the rule sizes and centres the thing that actually lands on the
  canvas. A "border" frame does not change the size a rule resolves
  against. Without a stored size the rule assumes a square source
  and promo_validate says so. Image and video layers.
- `palette` in compositionSettings names colours the project can
  reuse: [{ "name": "accent", "colorHex": "5B8CFF" }, …]. ANY colour
  field may then hold "@accent" instead of a hex value — background,
  border, caption colours, gradient stops, a background keyframe's
  colorHex. Matching ignores case. A name the palette does not
  define does NOT fall through to that field's own default: the
  reference is handed on unchanged, fails to parse as hex, and
  renders BLACK. `promo_inspect` lists undefined names, and it is
  worth reading its output, because the app's editing canvas draws
  unresolved caption text WHITE — so an undefined name can look
  right while you are working and ship invisible. Use it when a
  colour appears more than once — re-skinning a project then means
  editing one entry rather than hunting every occurrence. Colours
  are stored as
  bare `RRGGBB`; a leading `#` is accepted on read.
- A `palette` RESOURCE carries the same entries as a reusable
  definition: `{ "kind": "palette", "filename": "", "displayName":
  "Studio Dark", "palette": [{ "name": "canvas", "colorHex":
  "101014" }, …] }`. `compositionSettings.paletteResourceID` records
  which one the project follows; `compositionSettings.palette` is its
  MATERIALIZED copy — the app rewrites it from the resource on open
  and save, and every resolver keeps reading `settings.palette`, so a
  hand-authored document may simply write `palette` and skip the
  resource entirely. When authoring both, keep them consistent: the
  resource wins on the next open.
- Four entry names are ROLES — reserved names with a stated job, so
  a palette describes a look rather than a bag of colours: `canvas`
  (the ground behind everything), `text` (caption type), `text-bg`
  (the plate behind caption type), and `edge` (borders and rules
  around media). A role is only a NAME; there is no role field, and
  nothing resolves differently. Write a palette that states all four
  and point the matching settings fields at them —
  `backgroundColorHex: "@canvas"`, `subtitleColorHex: "@text"`,
  `subtitleBackgroundColorHex: "@text-bg"`, `videoBorderColorHex:
  "@edge"` — and re-skinning the whole project is one palette swap.
  State ALL FOUR in any palette meant to be swapped in: a palette
  missing a role leaves documents that followed a previous one
  pointing at a name nobody defines, which renders black. Any other
  name is freeform and nothing is wired to it; `accent`, and ramps
  like `accent1`…`accent4` for gradient stops, are the conventional
  ones.
- A gradient's `start`/`end` (and `repeat`) may be OMITTED on a
  background LAYER keyframe's gradient: absent geometry is pulled
  from the PLATE's gradient at every read — so a keyframe that only
  recolours follows the plate's later angle/width edits live, and
  only a keyframe that states geometry freezes it. A plate (or
  settings gradient) with absent geometry uses the canonical default
  for its kind: linear top→bottom, radial centre→corner.
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
  layer instead of several with duplicated keyframes. By itself it
  is a STEP landing at the keyframe's own time: there is no halfway
  between two images, and on a keyframe that only swaps,
  transitionDuration has nothing to ramp and does nothing. The
  layer's own resourceID shows before the first swap. Image, caption
  and drawing layers only — on video or audio a mid-layer swap would
  have to say where the second clip starts playing — and only to a
  resource of the layer's own kind; anything else is ignored. A
  caption swap replaces the WORDS (each caption resource carries its
  own text and style), a drawing swap the marks. Height is preserved
  across a swap (it is canvasHeight * zoom) while width follows the
  new source's aspect, anchored top-left.
- Give a swap keyframe a `transition` and it stops being a cut:
  { "time": 4, "resourceID": "<the next image>", "transition":
  { "kind": "wipe", "from": "left", "duration": 0.6 } } draws BOTH
  resources for those 0.6s — the outgoing one whole, the incoming
  one wiping, sliding, pushing or fading in over it. That is the
  crossfade between clips, and it needs no second layer. Same shape
  as transitionIn, and the one place `push` has old material to push
  out. A swap naming a missing or wrong-kind resource is skipped,
  transition included, so a deleted image degrades to a cut rather
  than cross-fading a picture with itself. Distinct from
  transitionDuration on the same keyframe: that ramps VALUES (zoom,
  position, opacity), this blends MATERIAL — a keyframe can carry
  both and they do not interact.
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
- `adjustments` on a layer is its own colour grade — its pixels and
  nobody else's. NOT an adjustment layer: nothing beneath is
  touched, so the screenshot goes black-and-white while the canvas
  around it keeps its colour.
  { "saturation": 0, "tintHex": "@accent", "tintAmount": 0.4 }.
  `saturation` 1 is untouched, 0 grey; `contrast` 1 is untouched;
  `brightness` is additive around 0; `tintHex` + `tintAmount`
  multiply a gel in (1 is fully gelled) — both halves or it does
  nothing and promo_validate says so. Applied in that order, so
  saturation 0 plus a warm tint reads as a duotone: mono is
  saturation 0 alone, sepia is saturation 0 with tintHex E8B380 at
  0.4. The scalars are keyframe tracks too — `saturation`,
  `contrast`, `brightness` and `tintAmount` on keyframes hold and
  ramp like any other value ("fade to grey"), and a keyframed field
  beats the layer constant of the same name. tintHex itself does not
  animate; a keyframed tintAmount ramps the one gel.
- `blendMode` says how a layer's pixels COMBINE with what is beneath
  them: "multiply" darkens (white drops out — vignettes, shadows,
  paper grain), "screen" lightens (black drops out — glows, flares
  and light leaks ship on black, and this is what makes them usable),
  "add" is pure light, hotter than screen and clipping sooner.
  Absent means ordinary source-over ("normal"). Static, not
  keyframable — nothing interpolates between two blend functions;
  animate the layer's opacity or its grade instead. Only layers that
  draw pixels combine; promo_validate names a blend on a background
  or audio layer.
- `motionBlur` gives a layer its own camera shutter:
  { "shutter": 0.5 }. `shutter` is the fraction of one frame
  interval the shutter stays open — 0.5 is the classic 180 degrees,
  1.0 a full 360; above 1 is clamped, zero or less does nothing, and
  promo_validate names both. What smears is the EDITOR's motion —
  position and zoom ramps, viewport pans, motion paths, rotation, a
  caption's travel, a swap transition's slide — never the footage's
  interior motion, which carries its own camera blur: each source
  frame is decoded once, and which resource shows never smears (a
  cut inside the shutter stays a cut). Per LAYER, absent means
  sharp, and there is deliberately no composition-wide default — a
  composite never shared one exposure, and the usual mistake is a
  smeared caption over sharp footage. The sample count is derived
  from how far things actually move, so a still moment costs nothing
  and renders bit-exact sharp. For a blur that RAMPS, put `shutter`
  on keyframes instead: it holds and eases like every other scalar
  track (the whip-pan idiom — blur arriving with the speed and
  leaving with it), and when any keyframe carries one the keyframes
  WIN over the layer constant, which promo_validate names if both
  are present.
- `maskResourceID` on a video or image layer windows it by a
  DRAWING: the drawing's ink is the mask, and the layer only shows
  where that drawing has ink — a filled oval for a porthole, a
  pen-tool star, an imported SVG shape. The mask keeps its OWN
  proportions: it is aspect-fitted into the layer's rect and
  centred, so a circle drawn round renders round whatever shape the
  layer is. It does NOT move with the content: a keyframe viewport
  pans and zooms the footage BEHIND the window while the window
  holds still — unless keyframes fly the window itself.
  `maskOffsetX` / `maskOffsetY` (canvas px), `maskZoom` (scale
  about the window's centre, 1 = as fitted), `maskZoomY` (the
  vertical scale when it should differ — absent it follows
  `maskZoom`, which is what keeps the shape honest) and
  `maskRotation` (clockwise degrees) on keyframes move
  the MASK while the footage stays put — the roaming-spotlight shot
  the viewport alone cannot make (paired keyframes can counter-pan
  a translation, never a rotation). Each rides the same eased
  scalar clock as every keyframe track, holds then ramps, and
  composes with the layer's own motion: the layer's rotation tilts
  the window too, the mask fields tilt it alone. Ink is
  ink — fills and strokes both count, and the ink's own opacity
  carries through: 50%-opacity ink shows the layer at 50%; a shape
  with `evenOddFill` makes a ring or a donut hole. `maskInverted:
  true` flips it — the ink becomes the HOLE (a cut-out) instead of
  the window. WHICH drawing is the mask (and the invert flag) is
  static per layer, like blendMode — the placement is what the
  keyframes fly; swaps, transitions, grades, blends and motion blur
  all happen INSIDE the window.
  promo_validate names a mask on any other layer kind, one pointing
  at nothing or at a non-drawing, and an inkless mask drawing. Known
  limit: `imageBorderWidth` / `imageBorderColorHex` still trace the
  rounded rect, not the mask outline, so a border on a masked layer
  is clipped by the window rather than following it. A mask is an
  ordinary drawing resource:
    { "id": "<uuid>", "kind": "drawing", "filename": "m.json",
      "displayName": "Oval mask", "addedAt": 0, "imageCuts": [],
      "disabledAudioTrackIndices": [],
      "drawing": { "shapes": [ { "id": "<uuid>", "kind": "oval",
        "points": [[0, 0], [100, 100]], "strokeColorHex": "FFFFFF",
        "strokeWidth": 1, "fillColorHex": "FFFFFF",
        "arrowStart": false, "arrowEnd": false } ] } }
  Shape `kind` is pen, line or oval; `fillOpacity` / `strokeOpacity`
  are optional 0..1.
- `tiltX` / `tiltY` (degrees) on an image layer's keyframes tilt a
  device-framed screenshot in 2.5D. They animate like any other
  track; leaving them out keeps the frame's own static tilt.
- opacity is 0..1 and defaults to 1. Cross-dissolve by overlapping two
  layers in time and fading one down as the other comes up.
- `fadeIn` / `fadeOut` on a LAYER, in seconds, are the shorthand for
  the four opacity keyframes every fading layer otherwise repeats.
  They are an ENVELOPE, not a replacement: the fade multiplies
  whatever the opacity keyframes resolve to, so a layer can fade in
  AND dip to 50% in the middle, and neither has to know about the
  other. A fadeOut counts back from the layer's end, so it does
  nothing on a layer with no `duration` — that layer runs to the end
  of the project, which the layer itself cannot see.
- `transitionIn` / `transitionOut` are how a layer ENTERS and LEAVES
  when a plain fade is not it: { "kind": "wipe", "from": "left",
  "duration": 0.5, "easing": "easeOut" }. Five kinds. `wipe` reveals
  the picture from an edge without moving it — the picture stays put
  and its edge travels. `slide` brings it in from beyond that edge
  of the FRAME, so a layer already near an edge still starts fully
  outside. `push` slides the new material in and shoves the old out
  the opposite side — it only has something to push at a resource
  swap, so at a layer's own edge it behaves as a slide. `scale`
  grows the layer into place, with a short fade so it does not pop.
  `fade` is fadeIn/fadeOut as an object. `from` is left / right /
  top / bottom, the edge the motion starts at; on the way OUT what
  remains collapses towards it. Absent, a wipe comes from the left,
  a slide from the bottom, a push from the right; fade and scale
  ignore it. `easing` shapes the ramp — the fadeIn/fadeOut shorthand
  is linear by definition, so a fade with a curve is written as the
  full object. fadeIn: 0.3 and transitionIn: { "kind": "fade",
  "duration": 0.3 } say the same thing — prefer the shorthand; a
  layer carrying both renders the transitionIn and promo_validate
  says so. Transitions MULTIPLY with opacity keyframes rather than
  replacing them; a transition longer than the layer is clamped to
  it; a transitionOut, like a fadeOut, needs the layer to have a
  `duration`. Background layers are the frame itself and do not
  transition.
- `timing` puts a layer on the timeline as a RULE instead of numbers:
  its start and end anchor to a NEIGHBOUR in the stack, plus an
  offset in seconds. { "timing": { "start": { "from":
  "previousStart", "offset": 0.45 }, "end": { "from": "previousEnd",
  "offset": 0 } } } on a caption is "enter half a beat after my
  clip, leave with it", and it stays true when the clip is
  retrimmed. `from` is previousStart / previousEnd (the neighbour
  one sortIndex step DOWN — the caption's clip) or nextStart /
  nextEnd (one step up); `offset` defaults to 0 and may be negative.
  The PEER forms — previousPeerStart / previousPeerEnd /
  nextPeerStart / nextPeerEnd — walk past layers of OTHER kinds to
  the nearest layer of the SAME kind, which is what lets a slide
  chain to the previous slide across the narration between them: no
  slide's start ever depends on how long a sound turned out to be.
  Half a spec is fine: no `start` keeps the layer's own startTime,
  no `end` keeps its own duration, so a start anchor alone slides
  the layer whole. Anchors reach exactly ONE layer either way, so
  chained layers are always a contiguous run — a slideshow is each
  clip's start anchored to previousEnd, and inserting a clip
  re-times everything after it. startTime stays REQUIRED on the
  wire: every open (app, CLI, render) resolves the spec and
  OVERWRITES startTime/duration with the answer, so on an anchored
  layer the stored numbers are only a cache — write 0 and let
  resolution fill them in. An `end` anchored to a layer that never
  ends (a background with no duration) means "run to the end of the
  composition". Resolution never refuses: what cannot be honoured
  is named by promo_validate and the layer keeps its stored
  numbers — an anchor with no neighbour on that side, a START
  anchored to the end of an open-ended layer (it would begin as the
  composition finishes), two neighbours each waiting on the other,
  and offsets that put the end at or before the start, which clamp
  to zero length and render NO frames, not one. In the app the
  attachment holds until the person drags the layer to numbers of
  their own; then their edit stands and the attachment is dropped,
  rather than snapping back on the next open.
- `durationRule` derives a layer's DURATION by rule — the time-twin
  of `placement`: stored, never baked, re-resolved on every open,
  with startTime/duration remaining the resolved answer every
  renderer reads. { "durationRule": { "kind": "fitContent" } } on an
  audio or video layer plays its RESOURCE out, however long the file
  turns out to be (a speech draft with no file yet keeps its stored
  duration). { "durationRule": { "kind": "fitDependents",
  "tail": 2.5 } } holds the layer AT LEAST its stored duration,
  extended so every layer whose start is anchored to it (its
  DEPENDENTS — containment via previousStart / nextStart, never the
  sequencing or peer forms) finishes inside it, plus `tail` seconds
  of air. That is "the slide stays N seconds or waits for its
  narration" as a rule instead of an app policy: synthesize a longer
  take, and the next resolve re-times the whole show — headless
  included. A rule and an `end` anchor on the same layer are two
  producers for one number; the anchor wins and promo_validate says
  so, as it also names a fitContent with no resource and a
  fitDependents nothing is anchored to.
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
- Every caption look field follows one rule: what THIS caption's
  captionStyle says, then what the composition says. Leaving a field
  out is how a caption follows the project; there is no separate
  "inherit" value. The pairs, per-caption first: `fontSize`
  (`subtitleFontSize` — one caption can be far larger than the
  composition default), `fontFamily` (`subtitleFontFamily`),
  `isBold` / `isItalic` (`subtitleBold` / `subtitleItalic`),
  `textColorHex` (`subtitleColorHex`), `alignment`
  (`subtitleAlignment`) — "leading" / "center" / "trailing",
  defaulting to center; `backgroundColorHex` / `backgroundOpacity` /
  `padding` / `cornerRadius` (`subtitleBackgroundColorHex`,
  `subtitleBackgroundOpacity`, `subtitleBackgroundPadding`,
  `subtitleBackgroundCornerRadius`), `leftMargin` / `rightMargin` /
  `verticalMargin` (the subtitle margins), the stroke and shadow
  fields above, and `reveal` (`subtitleReveal`). Font families:
  "system", "rounded", "serif", "monospaced", "helveticaNeue",
  "avenirNext", "gillSans", "futura", "trebuchetMS", "georgia",
  "palatino", "timesNewRoman", "americanTypewriter", "courierNew",
  "chalkboard", "markerFelt", "snellRoundhand".
- `reveal` in a caption's style makes the text arrive a piece at a
  time — a typewriter, word-by-word kinetic type, a karaoke
  highlight. A RULE, not keyframes, so it survives editing the words
  or the font: { "by": "word", "mode": "wipe" }. `by` is "character"
  (grapheme clusters — an emoji is one keystroke), "word" or "line".
  `mode` is one walk across the units, differing in what a unit does
  as its turn comes: "wipe" types it on (the unit is simply there);
  "fade", "rise" and "scale" give each unit its own little arrival,
  several in flight at once, so the caption assembles itself;
  "highlight" shows the whole caption and tints the current unit —
  karaoke — and wants a `highlightColorHex`, or the tint is the text
  colour and invisible. `unitSeconds` is how long ONE unit's arrival
  takes, and is the only difference between kinetic type and a
  typewriter: wipe is unitSeconds at zero. Absent, each unit
  overlaps its neighbour by half an arrival, which is what makes a
  stagger read as one motion rather than a queue. `rise` is how far
  a rise travels, in line heights (0.5 when absent — a proportion,
  so it survives any rendering density). Pace is `secondsPer` (per
  unit) or `seconds` (the whole caption), one or the other —
  promo_validate names the conflict; with NEITHER, the reveal
  spreads across the layer's own duration and lands with the
  caption. `easing` here shapes the walk across units, not each
  unit's own arrival. The caption is laid out WHOLE and then
  revealed: it never re-flows as it types, a wrapped caption
  finishes one line before starting the next, and a staggered word
  arrives in the place the layout gave it. A wipe crops the plate
  along with the text, so a caption with a background appears to
  grow its box — set subtitleBackgroundOpacity: 0 for the plate-less
  look most typewriters want. `subtitleReveal` in
  compositionSettings is the default every caption falls back to; a
  caption's own reveal overrides it.
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
