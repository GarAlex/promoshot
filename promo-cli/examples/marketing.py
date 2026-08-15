#!/usr/bin/env python3
"""Generate PromoShot projects for App Store marketing screenshots.

Writes one project per screenshot (stills) plus one slideshow project (video),
then leaves the rendering to `promo`. Everything about the design — background,
headline typography, window placement, corner radius, timing — is expressed in
metadata.json; this script only computes numbers.

    python3 marketing.py <screenshots-dir> <output-dir> [--theme dark|brand|light]
    promo still <output-dir>/01 --out shot-01.png
    promo video <output-dir>/slideshow --out promo.mp4 --fps 30

Design rules encoded here (App Store guidance):
  - a short headline above each window
  - the same background, typography and window size throughout
  - nothing implied that the app does not do

Slides overlap by FADE so each cross-dissolves into the next; without the
overlap both slides sit at zero opacity on the boundary and the background
flashes through.
"""

import json
import os
import shutil
import struct
import sys

# --- design constants -------------------------------------------------------
CANVAS_W, CANVAS_H = 1440, 900          # a valid Mac App Store screenshot size

# Themes, chosen with --theme. The window art is identical in all three; only
# the surround changes, which is the point of keeping design in the JSON.
THEMES = {
    # Deep navy. Maximum separation from a white app window, reads premium,
    # stands out against the App Store's light gallery.
    "dark":  {"bg": "0E1726", "headline": "FFFFFF", "border": "26364F"},
    # The app's own accent (#2673E7) taken darker: branded rather than
    # generic, still ample contrast for white bold text.
    "brand": {"bg": "123A7A", "headline": "FFFFFF", "border": "2F5AA8"},
    # Light. Matches the app's own surface, calmer — but the white window
    # needs a real border or it dissolves into the background.
    "light": {"bg": "F3F6FB", "headline": "0E1726", "border": "C2D2E8"},
}
THEME = THEMES["dark"]
BACKGROUND = THEME["bg"]
HEADLINE_COLOR = THEME["headline"]
HEADLINE_SIZE = 52
HEADLINE_PADDING = 14
HEADLINE_TOP_GAP = 46                    # canvas top -> headline box top
LINE_HEIGHT = 1.25                       # must match promo-text's default
WINDOW_TOP = 178                         # below the headline
WINDOW_BOTTOM_GAP = 54
WINDOW_CORNER_RADIUS = 18                # in canvas px, before zoom
WINDOW_BORDER_WIDTH = 1
WINDOW_BORDER_COLOR = THEME["border"]

SLIDE_SECONDS = 4.5
FADE = 0.45
# Headlines must never cross-fade with each other — two dissolving headlines
# on top of one another are unreadable. Each one lives strictly inside its
# slide, starting after the incoming dissolve and ending before the outgoing
# one, so they hand over instead of blending.
HEAD_FADE = 0.28

SHOTS = [
    ("01", "A faster spreadsheet for Mac"),
    ("02", "Built for large worksheets"),
    ("03", "Formulas without friction"),
    ("04", "Format every detail"),
    ("05", "Open, edit, and export"),
    ("06", "Focused, native, and offline"),
]


def png_size(path):
    with open(path, "rb") as f:
        head = f.read(24)
    if head[:8] != b"\x89PNG\r\n\x1a\n":
        raise SystemExit(f"{path}: not a PNG")
    return struct.unpack(">II", head[16:24])


def headline_box_height():
    """Mirrors promo-text: one line of text, plus padding above and below."""
    import math
    return math.ceil(HEADLINE_SIZE * LINE_HEIGHT) + HEADLINE_PADDING * 2


def headline_vertical_margin():
    """promo-text measures the vertical margin from the BOTTOM of the canvas,
    matching the app. Convert the top gap we actually care about."""
    return CANVAS_H - HEADLINE_TOP_GAP - headline_box_height()


def window_geometry(src_w, src_h):
    """Zoom and shift that centre the window under the headline.

    media_rect scales by canvas_height / source_height * zoom and places the
    rect's TOP-LEFT at (horizontalShift, verticalShift) — so the shifts are
    absolute canvas coordinates, not offsets from centre.
    """
    target_h = CANVAS_H - WINDOW_TOP - WINDOW_BOTTOM_GAP
    zoom = target_h / src_h
    width = src_w * zoom
    return {
        "zoom": round(zoom, 6),
        "horizontalShift": round((CANVAS_W - width) / 2, 2),
        "verticalShift": WINDOW_TOP,
        "width": round(width, 2),
        "height": round(target_h, 2),
    }


def settings():
    return {
        "canvasWidth": CANVAS_W,
        "canvasHeight": CANVAS_H,
        "backgroundColorHex": BACKGROUND,
        "videoCornerRadius": WINDOW_CORNER_RADIUS,
        "videoBorderWidth": WINDOW_BORDER_WIDTH,
        "videoBorderColorHex": WINDOW_BORDER_COLOR,
        # Headline typography, shared by every caption unless it overrides.
        "subtitleFontFamily": "system",
        "subtitleFontSize": HEADLINE_SIZE,
        "subtitleBold": True,
        "subtitleItalic": False,
        "subtitleColorHex": HEADLINE_COLOR,
        "subtitleBackgroundOpacity": 0.0,      # no plate: the navy is the plate
        "subtitleBackgroundColorHex": BACKGROUND,
        "subtitleBackgroundPadding": HEADLINE_PADDING,
        "subtitleBackgroundCornerRadius": 0,
        "subtitleLeftMargin": 80,
        "subtitleRightMargin": 80,
        "subtitleVerticalMargin": headline_vertical_margin(),
    }


def background_layer(sort_index=0):
    return {
        "id": "BG", "name": "Background", "sortIndex": sort_index,
        "kind": "background", "isEnabled": True, "startTime": 0, "keyframes": [],
    }


def window_layer(layer_id, name, resource_id, geo, sort_index,
                 start=0, duration=None, keyframes=None):
    layer = {
        "id": layer_id, "name": name, "sortIndex": sort_index,
        "kind": "image", "isEnabled": True, "startTime": start,
        "resourceID": resource_id,
        "keyframes": keyframes or [{
            "id": f"K-{layer_id}", "time": 0,
            "zoom": geo["zoom"],
            "horizontalShift": geo["horizontalShift"],
            "verticalShift": geo["verticalShift"],
            "transitionDuration": 0,
        }],
    }
    if duration is not None:
        layer["duration"] = duration
    return layer


def headline_layer(layer_id, text, sort_index, start=0, duration=None,
                   keyframes=None):
    layer = {
        "id": layer_id, "name": text, "sortIndex": sort_index,
        "kind": "caption", "isEnabled": True, "startTime": start,
        "captionText": text,
        "captionStyle": {"alignment": "center"},
        "keyframes": keyframes or [],
    }
    if duration is not None:
        layer["duration"] = duration
    return layer


def resource(rid, filename, name):
    return {
        "id": rid, "kind": "image", "filename": filename,
        "displayName": name, "addedAt": 0,
        "imageCuts": [], "disabledAudioTrackIndices": [],
    }


def base_project(name):
    return {
        "id": "AAAAAAAA-0000-0000-0000-0000000000" + name[-2:].zfill(2),
        "name": name, "createdAt": 0, "state": "recorded",
        "trimStart": 0, "trimEnd": 0, "videoDuration": 0,
        "subtitles": [], "sourceType": "slideshow",
        "compositionSettings": settings(),
    }


def write_project(out_dir, meta, assets):
    os.makedirs(os.path.join(out_dir, "Resources"), exist_ok=True)
    for src, filename in assets:
        shutil.copyfile(src, os.path.join(out_dir, "Resources", filename))
    with open(os.path.join(out_dir, "metadata.json"), "w") as f:
        json.dump(meta, f, indent=2)


def main():
    global THEME, BACKGROUND, HEADLINE_COLOR, WINDOW_BORDER_COLOR, WINDOW_BORDER_WIDTH
    args = [a for a in sys.argv[1:]]
    theme_name = "dark"
    if "--theme" in args:
        i = args.index("--theme")
        theme_name = args[i + 1]
        del args[i:i + 2]
    if theme_name not in THEMES:
        raise SystemExit(f"--theme must be one of {', '.join(THEMES)}")
    THEME = THEMES[theme_name]
    BACKGROUND = THEME["bg"]
    HEADLINE_COLOR = THEME["headline"]
    WINDOW_BORDER_COLOR = THEME["border"]
    # A light surround needs a heavier edge or the white window has no shape.
    WINDOW_BORDER_WIDTH = 2 if theme_name == "light" else 1
    if len(args) < 2:
        raise SystemExit(__doc__)
    src_dir, out_dir = args[0], args[1]

    found = []
    for prefix, headline in SHOTS:
        matches = sorted(f for f in os.listdir(src_dir)
                         if f.startswith(prefix) and f.lower().endswith(".png"))
        if not matches:
            raise SystemExit(f"no screenshot starting with {prefix} in {src_dir}")
        found.append((os.path.join(src_dir, matches[0]), matches[0], headline))

    sizes = {path: png_size(path) for path, _, _ in found}
    widths = {s for s in sizes.values()}
    if len(widths) > 1:
        print(f"note: screenshots are not all the same size: {widths}")

    # --- one project per screenshot -----------------------------------------
    for index, (path, filename, headline) in enumerate(found, start=1):
        geo = window_geometry(*sizes[path])
        meta = base_project(f"LightCell {index:02d}")
        meta["resources"] = [resource("R1", filename, headline)]
        meta["layers"] = [
            background_layer(0),
            window_layer("WIN", "Window", "R1", geo, 1),
            headline_layer("HEAD", headline, 2),
        ]
        write_project(os.path.join(out_dir, f"{index:02d}"), meta, [(path, filename)])

    # --- one slideshow for the video ----------------------------------------
    meta = base_project("LightCell Slideshow")
    meta["resources"] = []
    layers = [background_layer(0)]
    assets = []
    for index, (path, filename, headline) in enumerate(found):
        rid = f"R{index + 1}"
        meta["resources"].append(resource(rid, filename, headline))
        assets.append((path, filename))
        start = index * (SLIDE_SECONDS - FADE)
        geo = window_geometry(*sizes[path])
        # A slow push-in gives the still some life; opacity fades cover the cut.
        drift = 0.012
        window_keys = [
            {"id": f"KA{index}", "time": 0, "zoom": geo["zoom"],
             "horizontalShift": geo["horizontalShift"],
             "verticalShift": geo["verticalShift"],
             "opacity": 0.0, "transitionDuration": 0},
            {"id": f"KB{index}", "time": FADE, "zoom": geo["zoom"],
             "horizontalShift": geo["horizontalShift"],
             "verticalShift": geo["verticalShift"],
             "opacity": 1.0, "transitionDuration": FADE},
            {"id": f"KC{index}", "time": SLIDE_SECONDS - FADE,
             "zoom": round(geo["zoom"] * (1 + drift), 6),
             "horizontalShift": round(geo["horizontalShift"] - geo["width"] * drift / 2, 2),
             "verticalShift": round(geo["verticalShift"] - geo["height"] * drift / 2, 2),
             "opacity": 1.0, "transitionDuration": SLIDE_SECONDS - FADE * 2},
            {"id": f"KD{index}", "time": SLIDE_SECONDS,
             "zoom": round(geo["zoom"] * (1 + drift), 6),
             "horizontalShift": round(geo["horizontalShift"] - geo["width"] * drift / 2, 2),
             "verticalShift": round(geo["verticalShift"] - geo["height"] * drift / 2, 2),
             "opacity": 0.0, "transitionDuration": FADE},
        ]
        layers.append(window_layer(f"W{index}", f"Window {index + 1}", rid, geo,
                                   1 + index * 2, start=start,
                                   duration=SLIDE_SECONDS, keyframes=window_keys))
        # The headline lives inside the slide, clear of both dissolves.
        head_life = SLIDE_SECONDS - FADE * 2
        head_keys = [
            {"id": f"HA{index}", "time": 0, "opacity": 0.0, "transitionDuration": 0},
            {"id": f"HB{index}", "time": HEAD_FADE, "opacity": 1.0,
             "transitionDuration": HEAD_FADE},
            {"id": f"HC{index}", "time": head_life - HEAD_FADE, "opacity": 1.0,
             "transitionDuration": head_life - HEAD_FADE * 2},
            {"id": f"HD{index}", "time": head_life, "opacity": 0.0,
             "transitionDuration": HEAD_FADE},
        ]
        layers.append(headline_layer(f"H{index}", headline, 2 + index * 2,
                                     start=start + FADE, duration=head_life,
                                     keyframes=head_keys))
    meta["layers"] = layers
    write_project(os.path.join(out_dir, "slideshow"), meta, assets)

    print(f"wrote {len(found)} still projects + 1 slideshow to {out_dir}")
    print(f"  headline box: {headline_box_height()}px, "
          f"verticalMargin {headline_vertical_margin()} (from the bottom)")
    total = len(found) * (SLIDE_SECONDS - FADE) + FADE
    print(f"  slideshow:    {total:.1f}s "
          f"({SLIDE_SECONDS}s slides overlapping by {FADE}s)")


if __name__ == "__main__":
    main()
