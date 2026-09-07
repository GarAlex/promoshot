#!/usr/bin/env python3
"""A carousel of films, the simple way: compositions as cards.

Four films, each a composition with its own canvas, its own clock and
its own motion — a title that types itself, a list that arrives a line
at a time, three numbers that count in, a sign-off that glows. On the
main canvas ONE video layer holds a card-shaped rectangle (a placement:
height, anchor, offset) and a swap keyframe brings each film in through
a different cut — a push, a wipe, a blur dissolve, a zoom — with
`sourceTime: 0`, so every film starts as it arrives (rung 47, the
takeover). A caption above the card swaps its words at the same
instants. Nothing is computed between the films; the card's rectangle
is the only thing they share.

    python3 demos/c13-carousel-of-films/reference.py [--stills] [--video]

Writes reference.json beside this file and a .promo under runs/reference/.
"""
import json, os, subprocess, sys, uuid
HERE = os.path.dirname(os.path.abspath(__file__)); CORE = os.path.abspath(os.path.join(HERE, '..', '..'))
CLI = os.path.join(CORE, 'target', 'release', 'promo')
U = lambda: str(uuid.uuid4()).upper()

W, H = 1440, 900          # the main canvas
CW, CH = 1200, 750        # every card's own canvas, 16:10 like the main
T = 22.0                  # the piece
BEAT = 5.0                # each card's tenure
CARD = {"height": 640, "anchor": "center", "offset": [0, 40]}   # the rectangle the films share

PALETTE = [{"name": "ink", "colorHex": "0C0E14"}, {"name": "paper", "colorHex": "F4F1EA"},
           {"name": "accent", "colorHex": "5B8CFF"}, {"name": "accent2", "colorHex": "FF9A5B"},
           {"name": "mint", "colorHex": "3FD2A0"}, {"name": "plum", "colorHex": "3A2740"}]

def res(kind, name, **f):
    d = {"id": U(), "kind": kind, "filename": "", "displayName": name, "addedAt": 0,
         "imageCuts": [], "disabledAudioTrackIndices": []}
    d.update(f); return d

def layer(name, sort, kind, start, dur, **f):
    d = {"id": U(), "name": name, "sortIndex": sort, "kind": kind, "isEnabled": True,
         "startTime": start, "duration": dur, "keyframes": []}
    d.update(f); return d

def kf(t, ramp=0.0, **f):
    d = {"id": U(), "time": round(t, 4), "transitionDuration": round(ramp, 4)}
    d.update(f); return d

def gradient(a, b, drift=0.0):
    return {"kind": "radial", "start": [0.5 + drift, 0.45], "end": [1.15 + drift, 0.45], "repeat": "clamp",
            "stops": [{"colorHex": a, "at": 0.0}, {"colorHex": a, "at": 0.1},
                      {"colorHex": b, "at": 0.9}, {"colorHex": b, "at": 1.0}]}

def room(a, b, sort=0):
    """A background whose light pool drifts for the film's whole length."""
    return layer("Room", sort, "background", 0, T, keyframes=[
        kf(0, gradient=gradient(a, b, -0.12)),
        kf(T, ramp=T, easing="linear", gradient=gradient(a, b, 0.12))])

def caption(name, sort, start, dur, text, size, color="@paper", anchor="center", offset=(0, 0), **style):
    st = {"alignment": "center", "fontSize": size, "isBold": True, "textColorHex": color,
          "backgroundOpacity": 0, "placement": {"anchor": anchor, "offset": list(offset)}}
    st.update(style)
    return layer(name, sort, "caption", start, dur, captionText=text, captionStyle=st,
                 keyframes=[kf(0, opacity=1)])

def film(name, layers):
    return res("composition", name, duration=T, pixelWidth=CW, pixelHeight=CH,
               composition={"canvasWidth": CW, "canvasHeight": CH, "backgroundColorHex": "@ink", "layers": layers})

def card_title():
    # A title that types itself, word by word, and a line under it.
    return film("Film: the title", [
        room("@plum", "@ink"),
        caption("Title", 1, 0.3, T, "Every card is its own film", 76, offset=(0, -40),
                reveal={"by": "word", "seconds": 1.4}),
        caption("Line", 2, 1.9, T, "One layer holds the frame", 40, color="@accent", offset=(0, 70),
                reveal={"by": "character", "seconds": 1.0}),
    ])

def card_list():
    # A list that arrives a line at a time, each a chip on a plate.
    lines = ["Pause it", "Seek it", "Swap it"]
    layers = [room("@accent", "@ink")]
    for i, text in enumerate(lines):
        layers.append(caption(f"Item {i + 1}", 1 + i, 0.4 + i * 0.7, T, f"0{i + 1}   {text}", 54,
                              anchor="left", offset=(160, -130 + i * 130),
                              backgroundOpacity=0.9, backgroundColorHex="@paper",
                              textColorHex="@ink", cornerRadius=22, padding=22,
                              reveal={"by": "word", "seconds": 0.5}))
    return film("Film: the list", layers)

def card_numbers():
    # Three numbers, each scaling in on its own beat.
    layers = [room("@mint", "@ink")]
    for i, (n, label) in enumerate([("47", "rungs"), ("4", "films"), ("1", "layer")]):
        x = -360 + i * 360
        layers.append(layer(f"Number {i + 1}", 1 + i, "caption", 0.3 + i * 0.6, T, captionText=n,
                            captionStyle={"alignment": "center", "fontSize": 200, "isBold": True, "textColorHex": "@paper",
                                          "backgroundOpacity": 0, "placement": {"anchor": "center", "offset": [x, -50]}},
                            keyframes=[kf(0, fontSize=40, opacity=0), kf(0.5, ramp=0.5, easing="easeOut", fontSize=200, opacity=1)]))
        layers.append(caption(f"Label {i + 1}", 4 + i, 0.8 + i * 0.6, T, label, 40, color="@paper", offset=(x, 110)))
    return film("Film: the numbers", layers)

def card_signoff():
    # A sign-off that glows once, over a warm pool.
    return film("Film: the sign-off", [
        room("@accent2", "@ink"),
        layer("Word", 1, "caption", 0.2, T, captionText="PROMOSHOT",
              captionStyle={"alignment": "center", "fontSize": 112, "isBold": True, "textColorHex": "@paper",
                            "backgroundOpacity": 0, "placement": {"anchor": "center", "offset": [0, -20]}},
              keyframes=[kf(0, glow=0, opacity=1), kf(1.2, ramp=1.0, easing="easeInOut", glow=18),
                         kf(2.6, ramp=1.4, easing="easeInOut", glow=4)]),
        caption("Line", 2, 1.4, T, "compositions, all the way down", 36, color="@accent2", offset=(0, 110)),
    ])

def build():
    films = [card_title(), card_list(), card_numbers(), card_signoff()]
    cuts = [None,
            {"kind": "push", "from": "right", "duration": 0.7, "easing": "easeInOut"},
            {"kind": "wipe", "from": "left", "duration": 0.6, "easing": "easeOut"},
            {"kind": "blurDissolve", "duration": 0.8}]
    # The card: one layer, the rectangle the films share, a takeover per
    # card — each film fresh as it arrives — and a last one back to the
    # title through a zoom, so the carousel closes its ring.
    keys = [kf(0, placement=CARD)]
    for i in (1, 2, 3):
        keys.append(kf(i * BEAT, placement=CARD, resourceID=films[i]["id"], sourceTime=0, transition=cuts[i]))
    keys.append(kf(4 * BEAT, placement=CARD, resourceID=films[0]["id"], sourceTime=0,
                   transition={"kind": "zoom", "duration": 0.7}))
    card = layer("The card", 1, "video", 0, T, resourceID=films[0]["id"], keyframes=keys)
    # The words above the card swap on the same instants: a caption layer
    # swapping its WORDS, each a caption resource.
    words = ["The title", "The list", "The numbers", "The sign-off", "The title"]
    heads = [res("caption", w, captionText=w, captionStyle={"alignment": "center", "fontSize": 44, "isBold": True,
                                                                "textColorHex": "@paper", "backgroundOpacity": 0,
                                                                "placement": {"anchor": "top", "offset": [0, 60]}})
             for w in words]
    # A hard cut: two captions on one spot must never cross-fade.
    head_keys = [kf(0, opacity=1)] + [kf(i * BEAT, resourceID=heads[i]["id"]) for i in (1, 2, 3, 4)]
    head = layer("Which card", 2, "caption", 0, T, resourceID=heads[0]["id"], keyframes=head_keys)
    backdrop = layer("Backdrop", 0, "background", 0, T, keyframes=[kf(0, colorHex="@ink")])
    return {"id": U(), "name": "A carousel of films", "createdAt": 0, "state": "recorded", "minReaderVersion": 47,
            "trimStart": 0, "trimEnd": T, "videoDuration": T, "subtitles": [],
            "compositionSettings": {"canvasWidth": W, "canvasHeight": H, "backgroundColorHex": "@ink",
                                    "palette": PALETTE, "videoCornerRadius": 28, "videoBorderWidth": 0},
            "resources": films + heads, "layers": [backdrop, card, head]}

if __name__ == '__main__':
    doc = build()
    json.dump(doc, open(os.path.join(HERE, 'reference.json'), 'w'), indent=1)
    pkg = os.path.join(HERE, 'runs', 'reference', 'A carousel of films.promo')
    os.makedirs(os.path.join(pkg, 'Resources'), exist_ok=True)
    json.dump(doc, open(os.path.join(pkg, 'metadata.json'), 'w'), indent=1)
    v = subprocess.run([CLI, 'validate', pkg], capture_output=True, text=True); print(v.stdout.strip()[:1200] or v.stderr[-400:])
    work = os.path.dirname(pkg)
    if '--stills' in sys.argv:
        times = "0.8,2.5,5.35,6.5,10.3,11.5,15.4,17.0,20.35,21.5"
        subprocess.run([CLI, 'frames', pkg, '--out', os.path.join(work, 'frames'), '--times', times,
                        '--sheet', os.path.join(work, 'sheet.png')], check=False)
    if '--video' in sys.argv:
        subprocess.run([CLI, 'video', pkg, '--out', os.path.join(work, 'carousel-of-films.mp4')], check=False)
