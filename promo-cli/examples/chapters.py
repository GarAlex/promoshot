#!/usr/bin/env python3
"""Build a chaptered promo reel: title card, then one chapter per clip.

    python3 chapters.py <clips-dir> <hero-image> <output-dir>
    promo video <output-dir> --out reel.mp4 --fps 30

The shape of a chapter, and the reason this script exists rather than reel.py:

    ┌─ title beat ─┐┌──────── clip ────────┐
    caption CENTRED  caption travels up to the headline slot, clip plays

A caption keyframe does not mean what a media-layer keyframe means. The app's
mapping, which the core now follows:

    zoom            -> font size
    verticalShift   -> vertical margin, from the TOP of the canvas
    horizontalShift -> left margin

So the chapter title is keyed straight from the centre margin to the headline
margin. Size is left alone here — animating it is supported (key `zoom`) and
re-rasterizes per size, but a constant headline size reads better across cuts.

Durations come from the clips themselves, so the reel is as long as the
material plus the beats — around 42s for the five LightCell captures.
"""

import json
import math
import uuid
import os
import shutil
import subprocess
import sys

CANVAS_W, CANVAS_H = 1920, 1080
BACKGROUND = "0E1726"
OPENING_TINT = "12305C"      # the opener lifts, then settles into BACKGROUND
HEADLINE_COLOR = "FFFFFF"
HEADLINE_SIZE = 54
HERO_SIZE = 118
SUB_SIZE = 40
PADDING = 14
LINE_HEIGHT = 1.25
HEADLINE_TOP_GAP = 34
WINDOW_TOP = 150
WINDOW_CORNER_RADIUS = 14
WINDOW_BORDER_WIDTH = 1
WINDOW_BORDER_COLOR = "26364F"

TITLE_CARD = 4.6             # the opener
OUTRO = 6.0    # long enough for the closing line, measured not guessed
TITLE_BEAT = 3.3             # centred chapter title, before its clip
# 3.3 rather than a round number: the narration was measured first and
# the beats were widened to fit it, not the other way round.
# --zoomdown: the chapter title opens BIG in the centre and shrinks to the
# headline size as it rides up. Caption keyframes animate font size via
# `zoom` (the app's mapping), and the core re-rasterizes per size, so the
# text stays crisp the whole way instead of being a scaled bitmap.
BIG_TITLE = 96
MOVE = 0.5                   # how long the title takes to travel up
# The window must not arrive while the title is still crossing it: white text
# over a white app window is invisible, which is exactly what the first render
# showed. The clip starts fading in once the title has nearly landed, so the
# two motions still overlap and the cut reads as one move.
CLIP_LEAD = 0.4
FADE = 0.45                  # clip fade in/out
HEAD_FADE = 0.35
CHAPTER_OVERLAP = 0.35       # a clip's tail overlaps the next title's head
LEAD_IN, LEAD_OUT = 0.15, 0.15

PROJECT_NAME = "LightCell Demo"
HERO_TITLE = "LightCell Demo"
HERO_SUBTITLE = "A fast spreadsheet for Mac"
OUTRO_TITLE = "LightCell"
OUTRO_SUBTITLE = "Open, edit and export — natively, offline"

CLIPS = [
    ("01", "Open any .xlsx"),
    ("02", "Built for large worksheets"),
    ("03", "Formulas without friction"),
    ("04", "Format every detail"),
    ("05", "Import CSV files with confidence"),
]


def probe_duration(path):
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "format=duration",
         "-of", "default=noprint_wrappers=1:nokey=1", path],
        capture_output=True, text=True, check=True).stdout.strip()
    return float(out)


def probe_size(path):
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0",
         "-show_entries", "stream=width,height",
         "-of", "default=noprint_wrappers=1:nokey=1", path],
        capture_output=True, text=True, check=True).stdout.split()
    return int(out[0]), int(out[1])


def box_height(font_size):
    """What promo-text lays out for one line at this size."""
    return math.ceil(font_size * LINE_HEIGHT) + PADDING * 2


VERTICAL_MARGIN = HEADLINE_TOP_GAP


def shift_to_top(font_size, wanted_top):
    """The margin that puts a caption's top edge at `wanted_top`.

    Measured from the TOP, so it is the wanted position itself — the font size
    does not enter into it. It stays a named function because the box height
    still matters for centring.
    """
    del font_size
    return round(wanted_top, 2)


def centre_shift(font_size):
    return round((CANVAS_H - box_height(font_size)) / 2, 2)


def window_geometry(src_w, src_h, top=WINDOW_TOP, scale=None):
    """Native size if it fits, else the largest that does."""
    if scale is None:
        available_h = CANVAS_H - top - 30
        scale = min(1.0, available_h / src_h, (CANVAS_W - 80) / src_w)
    width, height = src_w * scale, src_h * scale
    return {
        # media_rect scales by (canvas_height / source_height) * zoom.
        "zoom": round(scale * src_h / CANVAS_H, 6),
        "horizontalShift": round((CANVAS_W - width) / 2, 2),
        "verticalShift": top,
    }


def settings():
    return {
        "canvasWidth": CANVAS_W,
        "canvasHeight": CANVAS_H,
        "backgroundColorHex": BACKGROUND,
        "videoCornerRadius": WINDOW_CORNER_RADIUS,
        "videoBorderWidth": WINDOW_BORDER_WIDTH,
        "videoBorderColorHex": WINDOW_BORDER_COLOR,
        "subtitleFontFamily": "system",
        "subtitleFontSize": HEADLINE_SIZE,
        "subtitleBold": True,
        "subtitleItalic": False,
        "subtitleColorHex": HEADLINE_COLOR,
        "subtitleBackgroundOpacity": 0.0,
        "subtitleBackgroundColorHex": BACKGROUND,
        "subtitleBackgroundPadding": PADDING,
        "subtitleBackgroundCornerRadius": 0,
        "subtitleLeftMargin": 90,
        "subtitleRightMargin": 90,
        "subtitleVerticalMargin": VERTICAL_MARGIN,
        # 60000/1001 matches what ScreenCaptureKit actually delivers, so the
        # scroll stays as smooth as it was captured.
        "fps": 60000 / 1001,
    }


def caption_layer(lid, name, text, sort, start, life, keys, style=None):
    return {
        "id": lid, "name": name, "sortIndex": sort, "kind": "caption",
        "isEnabled": True, "startTime": round(start, 3),
        "duration": round(life, 3), "captionText": text,
        "captionStyle": dict({"alignment": "center"}, **(style or {})),
        "keyframes": keys,
    }


def held_caption(lid, dy, life, size_prefix):
    """A caption that fades in, holds at `dy`, and fades out.

    Every keyframe carries BOTH opacity and verticalShift: each field is
    interpolated over only the keyframes that define it, so a keyframe that
    omits one silently drops out of that field's timeline.
    """
    return [
        {"id": f"{size_prefix}A", "time": 0, "opacity": 0.0,
         "verticalShift": dy, "transitionDuration": 0},
        {"id": f"{size_prefix}B", "time": HEAD_FADE, "opacity": 1.0,
         "verticalShift": dy, "transitionDuration": HEAD_FADE},
        {"id": f"{size_prefix}C", "time": life - HEAD_FADE, "opacity": 1.0,
         "verticalShift": dy, "transitionDuration": 0},
        {"id": f"{size_prefix}D", "time": life, "opacity": 0.0,
         "verticalShift": dy, "transitionDuration": HEAD_FADE},
    ]


def attach(start=None, end=None):
    """A `timing` block: where this layer begins and ends, in terms of the one
    above it.

    Anchors reach only one layer back, which is why the chapter's clip is
    keyed off the PREVIOUS chapter's caption, and its own caption is keyed off
    the clip that follows it in z-order. Nothing here computes a cursor.
    """
    timing = {}
    if start is not None:
        timing["start"] = {"from": start[0], "offset": round(start[1], 3)}
    if end is not None:
        timing["end"] = {"from": end[0], "offset": round(end[1], 3)}
    return timing


def chapter_title_keys(index, life, zoomdown):
    """The chapter title's ride from centre stage to the headline slot.

    With --zoomdown it also opens at BIG_TITLE and shrinks to HEADLINE_SIZE
    during the move. Every keyframe carries every animated field: caption
    values interpolate over the keyframes that define ANY of them, and an
    omitted field falls back to the BASE style — a keyframe missing `zoom`
    would yank the size back to 54 for its stretch of the timeline.
    """
    size_big = BIG_TITLE if zoomdown else HEADLINE_SIZE
    centre = centre_shift(size_big)
    top = VERTICAL_MARGIN
    return [
        {"id": f"HA{index}", "time": 0, "opacity": 0.0,
         "verticalShift": centre, "zoom": size_big, "transitionDuration": 0},
        {"id": f"HB{index}", "time": HEAD_FADE, "opacity": 1.0,
         "verticalShift": centre, "zoom": size_big,
         "transitionDuration": HEAD_FADE},
        # Hold dead centre for the whole title beat...
        {"id": f"HC{index}", "time": TITLE_BEAT, "opacity": 1.0,
         "verticalShift": centre, "zoom": size_big, "transitionDuration": 0},
        # ...then ride up (and shrink) while the clip fades in.
        {"id": f"HD{index}", "time": TITLE_BEAT + MOVE, "opacity": 1.0,
         "verticalShift": top, "zoom": HEADLINE_SIZE,
         "transitionDuration": MOVE},
        {"id": f"HE{index}", "time": life - HEAD_FADE, "opacity": 1.0,
         "verticalShift": top, "zoom": HEADLINE_SIZE, "transitionDuration": 0},
        {"id": f"HF{index}", "time": life, "opacity": 0.0,
         "verticalShift": top, "zoom": HEADLINE_SIZE,
         "transitionDuration": HEAD_FADE},
    ]


def merge_into_existing(out_dir, fresh):
    """Update the project in place, the way a person editing it would.

    Rewriting metadata.json wholesale destroys everything the script does not
    know about — narration text, its `renderedHash`, a speed someone set, a
    layer added by hand. That is not a theoretical loss: regenerating this
    project once wiped seven `renderedHash` values, re-synthesized every line,
    and came back with DIFFERENT durations, because text-to-speech is not
    reproducible. The edit re-timed itself for no reason.

    So each object the script owns is merged over the stored one by id, and
    anything else in the file is left exactly as it was. Deterministic ids are
    what make that possible — they are the identity the merge matches on, not
    just a way to keep diffs clean.
    """
    path = os.path.join(out_dir, "metadata.json")
    if not os.path.exists(path):
        return fresh
    with open(path) as handle:
        stored = json.load(handle)

    def merge(kind):
        by_id = {item["id"]: item for item in stored.get(kind, [])}
        out = []
        for item in fresh.get(kind, []):
            existing = by_id.pop(item["id"], None)
            out.append({**existing, **item} if existing else item)
        # Anything the script did not produce stays — narration resources and
        # their layers live here.
        out.extend(by_id.values())
        return out

    merged = {**stored, **fresh}
    merged["resources"] = merge("resources")
    merged["layers"] = merge("layers")
    return merged


def uuidify(meta):
    """Rewrite every id as a deterministic UUID.

    The core stores ids as strings, but the APP decodes them as UUIDs — a
    project with ids like "V0" validates in the core and then cannot be opened
    in PromoShot at all. uuid5 keeps them stable across regenerations, so
    reruns diff cleanly.
    """
    # Namespaced by project name: two reels generated by this script must
    # not share ids, or they are the same project wearing two names.
    ns = uuid.uuid5(uuid.NAMESPACE_URL, "promoshot.chapters/" + PROJECT_NAME)
    def m(value):
        return str(uuid.uuid5(ns, str(value))).upper()
    meta["id"] = m(meta["id"])
    for resource in meta["resources"]:
        resource["id"] = m(resource["id"])
    for layer in meta["layers"]:
        layer["id"] = m(layer["id"])
        if "resourceID" in layer:
            layer["resourceID"] = m(layer["resourceID"])
        for key in layer.get("keyframes", []):
            key["id"] = m(key["id"])


def main():
    attached = "--attached" in sys.argv
    zoomdown = "--zoomdown" in sys.argv
    # --config <json>: retarget the reel at another app without forking the
    # script — clips, titles, hero and outro come from the file.
    global CLIPS, HERO_TITLE, HERO_SUBTITLE, OUTRO_TITLE, OUTRO_SUBTITLE
    if "--config" in sys.argv:
        path = sys.argv[sys.argv.index("--config") + 1]
        with open(path) as handle:
            cfg = json.load(handle)
        CLIPS = [tuple(c) for c in cfg.get("clips", CLIPS)]
        global PROJECT_NAME
        PROJECT_NAME = cfg.get("name", cfg.get("heroTitle", "Demo"))
        HERO_TITLE = cfg.get("heroTitle", HERO_TITLE)
        HERO_SUBTITLE = cfg.get("heroSubtitle", HERO_SUBTITLE)
        OUTRO_TITLE = cfg.get("outroTitle", OUTRO_TITLE)
        OUTRO_SUBTITLE = cfg.get("outroSubtitle", OUTRO_SUBTITLE)
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    if "--config" in sys.argv:
        args.remove(sys.argv[sys.argv.index("--config") + 1])
    if len(args) < 3:
        raise SystemExit(__doc__)
    src_dir, hero_image, out_dir = args[0], args[1], args[2]

    found = []
    for prefix, headline in CLIPS:
        matches = sorted(f for f in os.listdir(src_dir)
                         if f.startswith(prefix) and f.endswith(".mp4"))
        if not matches:
            raise SystemExit(f"no clip starting with {prefix} in {src_dir}")
        path = os.path.join(src_dir, matches[0])
        found.append((path, matches[0], headline, probe_duration(path),
                      probe_size(path)))

    resources_dir = os.path.join(out_dir, "Resources")
    os.makedirs(resources_dir, exist_ok=True)

    layers = []
    resources = []

    # ---- Title card -------------------------------------------------------
    hero_name = os.path.basename(hero_image)
    shutil.copyfile(hero_image, os.path.join(resources_dir, hero_name))
    hero_w, hero_h = probe_size(hero_image)
    hero_geo = window_geometry(hero_w, hero_h, top=470, scale=0.52)
    resources.append({
        "id": "RH", "kind": "image", "filename": hero_name,
        "displayName": "Hero", "addedAt": 0, "duration": 0,
        "trimStart": 0, "trimEnd": 0,
        "imageCuts": [], "disabledAudioTrackIndices": [],
    })
    layers.append({
        "id": "BG", "name": "Background", "sortIndex": 0, "kind": "background",
        "isEnabled": True, "startTime": 0,
        # The opener sits in a lighter blue and settles into the body colour,
        # so the title card reads as its own moment.
        "keyframes": [
            {"id": "BG0", "time": 0, "colorHex": OPENING_TINT,
             "transitionDuration": 0},
            {"id": "BG1", "time": TITLE_CARD, "colorHex": BACKGROUND,
             "transitionDuration": 2.2},
        ],
    })
    layers.append({
        "id": "IH", "name": "Hero shot", "sortIndex": 1, "kind": "image",
        "isEnabled": True, "startTime": 0.5,
        "duration": round(TITLE_CARD - 0.5, 3), "resourceID": "RH",
        "keyframes": [
            {"id": "IH0", "time": 0, "opacity": 0.0,
             "transitionDuration": 0, **hero_geo},
            {"id": "IH1", "time": 0.8, "opacity": 1.0,
             "transitionDuration": 0.8, **hero_geo},
            {"id": "IH2", "time": TITLE_CARD - 0.5 - FADE, "opacity": 1.0,
             "transitionDuration": 0, **hero_geo},
            {"id": "IH3", "time": TITLE_CARD - 0.5, "opacity": 0.0,
             "transitionDuration": FADE, **hero_geo},
        ],
    })
    layers.append(caption_layer(
        "TT", "Title", HERO_TITLE, 4, 0.15, TITLE_CARD - 0.3,
        held_caption("TT", shift_to_top(HERO_SIZE, 210), TITLE_CARD - 0.3, "T"),
        style={"fontSize": HERO_SIZE}))
    layers.append(caption_layer(
        "TS", "Subtitle", HERO_SUBTITLE, 5, 0.75, TITLE_CARD - 0.9,
        held_caption("TS", shift_to_top(SUB_SIZE, 400), TITLE_CARD - 0.9, "S"),
        style={"fontSize": SUB_SIZE}))

    # ---- Chapters ---------------------------------------------------------
    cursor = TITLE_CARD - CHAPTER_OVERLAP
    # The first chapter's clip hangs off the last title-card layer in z-order,
    # which is the subtitle caption.
    previous_end = 0.75 + (TITLE_CARD - 0.9)
    dy_centre = centre_shift(HEADLINE_SIZE)

    for index, (path, filename, headline, duration, size) in enumerate(found):
        shutil.copyfile(path, os.path.join(resources_dir, filename))
        rid = f"R{index + 1}"
        usable = max(0.6, duration - LEAD_IN - LEAD_OUT)
        resources.append({
            "id": rid, "kind": "video", "filename": filename,
            "displayName": headline, "addedAt": 0, "duration": duration,
            "trimStart": LEAD_IN, "trimEnd": duration - LEAD_OUT,
            "imageCuts": [], "disabledAudioTrackIndices": [],
        })

        geo = window_geometry(*size)
        clip_start = cursor + TITLE_BEAT + CLIP_LEAD
        base = 10 + index * 3
        clip_layer = {
            "id": f"V{index}", "name": f"Clip {index + 1}",
            "sortIndex": base, "kind": "video", "isEnabled": True,
            "startTime": round(clip_start, 3), "duration": round(usable, 3),
            "resourceID": rid,
            "keyframes": [
                {"id": f"A{index}", "time": 0, "opacity": 0.0,
                 "transitionDuration": 0, **geo},
                {"id": f"B{index}", "time": FADE, "opacity": 1.0,
                 "transitionDuration": FADE, **geo},
                {"id": f"C{index}", "time": usable - FADE, "opacity": 1.0,
                 "transitionDuration": 0, **geo},
                {"id": f"D{index}", "time": usable, "opacity": 0.0,
                 "transitionDuration": FADE, **geo},
            ],
        }
        if attached:
            # The previous chapter's caption ends with its clip, so "that end,
            # less the overlap, plus this chapter's title beat" is the whole
            # rule — the cursor arithmetic disappears.
            lead = TITLE_BEAT + CLIP_LEAD - CHAPTER_OVERLAP
            clip_layer["timing"] = attach(
                start=("previousEnd", lead if index else clip_start - previous_end))
        layers.append(clip_layer)

        life = TITLE_BEAT + CLIP_LEAD + usable
        chapter_caption = caption_layer(
            f"H{index}", headline, headline, base + 1, cursor, life,
            chapter_title_keys(index, life, zoomdown))
        if attached:
            # Its own clip is the layer directly above: begin a title beat
            # before it starts, and finish exactly when it does.
            chapter_caption["timing"] = attach(
                start=("previousStart", -(TITLE_BEAT + CLIP_LEAD)),
                end=("previousEnd", 0.0))
        layers.append(chapter_caption)
        previous_end = clip_start + usable

        cursor += life - CHAPTER_OVERLAP

    # ---- Outro ------------------------------------------------------------
    layers.append(caption_layer(
        "OT", "Outro", OUTRO_TITLE, 90, cursor + 0.2, OUTRO - 0.4,
        held_caption("OT", shift_to_top(HERO_SIZE, 380), OUTRO - 0.4, "O"),
        style={"fontSize": HERO_SIZE}))
    layers.append(caption_layer(
        "OS", "Outro line", OUTRO_SUBTITLE, 91, cursor + 0.8, OUTRO - 1.0,
        held_caption("OS", shift_to_top(SUB_SIZE, 570), OUTRO - 1.0, "P"),
        style={"fontSize": SUB_SIZE}))
    total = cursor + OUTRO

    meta = {
        "id": "AAAAAAAA-0000-0000-0000-0000000CHAPT"[:36],
        "name": PROJECT_NAME, "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
        "sourceType": "video", "compositionSettings": settings(),
        "resources": resources, "layers": layers,
    }
    uuidify(meta)
    meta = merge_into_existing(out_dir, meta)
    with open(os.path.join(out_dir, "metadata.json"), "w") as handle:
        json.dump(meta, handle, indent=2)

    print(f"{out_dir}  {len(found)} chapters, {total:.1f}s")


if __name__ == "__main__":
    main()
