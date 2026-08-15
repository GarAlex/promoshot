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
OUTRO = 3.2
TITLE_BEAT = 2.5             # centred chapter title, before its clip
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


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
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
        layers.append({
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
        })

        life = TITLE_BEAT + CLIP_LEAD + usable
        layers.append(caption_layer(
            f"H{index}", headline, headline, base + 1, cursor, life,
            [
                {"id": f"HA{index}", "time": 0, "opacity": 0.0,
                 "verticalShift": dy_centre, "transitionDuration": 0},
                {"id": f"HB{index}", "time": HEAD_FADE, "opacity": 1.0,
                 "verticalShift": dy_centre, "transitionDuration": HEAD_FADE},
                # Hold dead centre for the whole title beat...
                {"id": f"HC{index}", "time": TITLE_BEAT, "opacity": 1.0,
                 "verticalShift": dy_centre, "transitionDuration": 0},
                # ...then ride up to the headline slot while the clip plays.
                {"id": f"HD{index}", "time": TITLE_BEAT + MOVE, "opacity": 1.0,
                 "verticalShift": 0, "transitionDuration": MOVE},
                {"id": f"HE{index}", "time": life - HEAD_FADE, "opacity": 1.0,
                 "verticalShift": 0, "transitionDuration": 0},
                {"id": f"HF{index}", "time": life, "opacity": 0.0,
                 "verticalShift": 0, "transitionDuration": HEAD_FADE},
            ]))

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
        "name": "LightCell Demo", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
        "sourceType": "video", "compositionSettings": settings(),
        "resources": resources, "layers": layers,
    }
    with open(os.path.join(out_dir, "metadata.json"), "w") as handle:
        json.dump(meta, handle, indent=2)

    print(f"{out_dir}  {len(found)} chapters, {total:.1f}s")


if __name__ == "__main__":
    main()
