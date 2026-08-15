#!/usr/bin/env python3
"""Build a promo reel project from a folder of screen recordings.

    python3 reel.py <clips-dir> <output-dir> [--size 1920x1080]
    promo video <output-dir> --out reel.mp4 --fps 30

Each clip becomes a video layer with a headline over it; clips overlap by
FADE so one cross-dissolves into the next, and headlines live strictly inside
their own clip so two never blend into each other.

Why 1920x1080 with the window at NATIVE size: the recordings are 1440x900, so
they drop into a 1080p canvas untouched, with 480x180 left over for the
headline. Rendering the reel at the recording's own size and upscaling later
would soften exactly the fine grid text these clips exist to show.
"""

import json
import math
import os
import shutil
import subprocess
import sys

CANVAS_W, CANVAS_H = 1920, 1080
BACKGROUND = "0E1726"
HEADLINE_COLOR = "FFFFFF"
HEADLINE_SIZE = 54
HEADLINE_PADDING = 14
HEADLINE_TOP_GAP = 34
LINE_HEIGHT = 1.25
WINDOW_TOP = 150
WINDOW_CORNER_RADIUS = 14
WINDOW_BORDER_WIDTH = 1
WINDOW_BORDER_COLOR = "26364F"

FADE = 0.45          # cross-dissolve between clips
HEAD_FADE = 0.28     # headline fade, inside its clip
# A clip's first and last moments are usually the window settling or the
# cursor leaving; trim a little off each end rather than showing it.
LEAD_IN = 0.15
LEAD_OUT = 0.15

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


def headline_box_height():
    return math.ceil(HEADLINE_SIZE * LINE_HEIGHT) + HEADLINE_PADDING * 2


def headline_vertical_margin():
    """promo-text measures from the BOTTOM, matching the app."""
    return CANVAS_H - HEADLINE_TOP_GAP - headline_box_height()


def window_geometry(src_w, src_h):
    """Native size if it fits, else the largest that does.

    media_rect scales by canvas_height / source_height * zoom and places the
    rect's top-left at (horizontalShift, verticalShift).
    """
    available_h = CANVAS_H - WINDOW_TOP - 30
    scale = min(1.0, available_h / src_h, (CANVAS_W - 80) / src_w)
    width, height = src_w * scale, src_h * scale
    # media_rect's scale is (canvas_height / source_height) * zoom, so the
    # zoom that yields `scale` is scale * source_height / canvas_height.
    return {
        "zoom": round(scale * src_h / CANVAS_H, 6),
        "horizontalShift": round((CANVAS_W - width) / 2, 2),
        "verticalShift": WINDOW_TOP,
        "width": round(width, 2),
        "height": round(height, 2),
        "native": abs(scale - 1.0) < 1e-9,
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
        "subtitleBackgroundPadding": HEADLINE_PADDING,
        "subtitleBackgroundCornerRadius": 0,
        "subtitleLeftMargin": 90,
        "subtitleRightMargin": 90,
        "subtitleVerticalMargin": headline_vertical_margin(),
    }


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    if len(args) < 2:
        raise SystemExit(__doc__)
    src_dir, out_dir = args[0], args[1]

    found = []
    for prefix, headline in CLIPS:
        matches = sorted(f for f in os.listdir(src_dir)
                         if f.startswith(prefix) and f.endswith(".mp4"))
        if not matches:
            raise SystemExit(f"no clip starting with {prefix} in {src_dir}")
        path = os.path.join(src_dir, matches[0])
        found.append((path, matches[0], headline, probe_duration(path),
                      probe_size(path)))

    os.makedirs(os.path.join(out_dir, "Resources"), exist_ok=True)
    meta = {
        "id": "AAAAAAAA-0000-0000-0000-00000000REEL"[:36],
        "name": "LightCell Reel", "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 0, "videoDuration": 0, "subtitles": [],
        "sourceType": "video", "compositionSettings": settings(),
        "resources": [], "layers": [
            {"id": "BG", "name": "Background", "sortIndex": 0,
             "kind": "background", "isEnabled": True, "startTime": 0,
             "keyframes": []},
        ],
    }

    cursor = 0.0
    for index, (path, filename, headline, duration, size) in enumerate(found):
        shutil.copyfile(path, os.path.join(out_dir, "Resources", filename))
        rid = f"R{index + 1}"
        # The layer plays [LEAD_IN, duration - LEAD_OUT] of the clip; the
        # resource's trim is what maps composition time onto source time.
        usable = max(0.6, duration - LEAD_IN - LEAD_OUT)
        meta["resources"].append({
            "id": rid, "kind": "video", "filename": filename,
            "displayName": headline, "addedAt": 0, "duration": duration,
            "trimStart": LEAD_IN, "trimEnd": duration - LEAD_OUT,
            "imageCuts": [], "disabledAudioTrackIndices": [],
        })

        geo = window_geometry(*size)
        start = cursor
        keys = [
            {"id": f"A{index}", "time": 0, "zoom": geo["zoom"],
             "horizontalShift": geo["horizontalShift"],
             "verticalShift": geo["verticalShift"],
             "opacity": 0.0, "transitionDuration": 0},
            {"id": f"B{index}", "time": FADE, "zoom": geo["zoom"],
             "horizontalShift": geo["horizontalShift"],
             "verticalShift": geo["verticalShift"],
             "opacity": 1.0, "transitionDuration": FADE},
            {"id": f"C{index}", "time": usable - FADE, "zoom": geo["zoom"],
             "horizontalShift": geo["horizontalShift"],
             "verticalShift": geo["verticalShift"],
             "opacity": 1.0, "transitionDuration": usable - FADE * 2},
            {"id": f"D{index}", "time": usable, "zoom": geo["zoom"],
             "horizontalShift": geo["horizontalShift"],
             "verticalShift": geo["verticalShift"],
             "opacity": 0.0, "transitionDuration": FADE},
        ]
        meta["layers"].append({
            "id": f"V{index}", "name": f"Clip {index + 1}",
            "sortIndex": 1 + index * 2, "kind": "video", "isEnabled": True,
            "startTime": round(start, 3), "duration": round(usable, 3),
            "resourceID": rid, "keyframes": keys,
        })

        head_life = max(0.8, usable - FADE * 2)
        meta["layers"].append({
            "id": f"H{index}", "name": headline, "sortIndex": 2 + index * 2,
            "kind": "caption", "isEnabled": True,
            "startTime": round(start + FADE, 3), "duration": round(head_life, 3),
            "captionText": headline, "captionStyle": {"alignment": "center"},
            "keyframes": [
                {"id": f"HA{index}", "time": 0, "opacity": 0.0,
                 "transitionDuration": 0},
                {"id": f"HB{index}", "time": HEAD_FADE, "opacity": 1.0,
                 "transitionDuration": HEAD_FADE},
                {"id": f"HC{index}", "time": head_life - HEAD_FADE,
                 "opacity": 1.0,
                 "transitionDuration": max(0.01, head_life - HEAD_FADE * 2)},
                {"id": f"HD{index}", "time": head_life, "opacity": 0.0,
                 "transitionDuration": HEAD_FADE},
            ],
        })
        cursor = start + usable - FADE

    with open(os.path.join(out_dir, "metadata.json"), "w") as f:
        json.dump(meta, f, indent=2)

    total = cursor + FADE
    native = all(window_geometry(*size)["native"] for *_, size in found)
    print(f"wrote {out_dir}: {len(found)} clips, {total:.1f}s at "
          f"{CANVAS_W}x{CANVAS_H}")
    print(f"  window: {'native size, no resampling' if native else 'scaled to fit'}")
    print(f"  headline box {headline_box_height()}px, "
          f"verticalMargin {headline_vertical_margin()}")


if __name__ == "__main__":
    main()
