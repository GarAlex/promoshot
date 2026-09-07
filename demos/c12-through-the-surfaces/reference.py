#!/usr/bin/env python3
"""Through the surfaces, the simple way: a composition is a texture.

Four films, each as long as the whole piece and each with a camera of
its own. The deepest — a chrome word on a mirror floor, the camera
drifting round it — is bound to a phone's Screen; the phone's film is
bound to a tablet's Screen; the tablet's to a laptop's; the laptop's is
the MAIN film at the start. Nothing is computed between the films: each
outer camera flies to a maximum close-up on its screen — the orbit's
distance at its floor, the field widened until the screen covers the
frame, looking straight at it — and the main switches to the film that
screen was showing, which has been playing since frame one on the same
clock. `placement: fill` is a centred cover crop and every close-up
covers by width, centred, so the bands nest on their own; a 0.3 s
dissolve hides what is left.

    python3 demos/c12-through-the-surfaces/reference.py [--stills] [--video]

Writes reference.json beside this file and a .promo under runs/reference/.
"""
import json, math, os, shutil, subprocess, sys, uuid
HERE = os.path.dirname(os.path.abspath(__file__)); CORE = os.path.abspath(os.path.join(HERE, '..', '..'))
CLI = os.path.join(CORE, 'target', 'release', 'promo')
U = lambda: str(uuid.uuid4()).upper()
PLACE = os.environ.get('PLACE', 'fill')      # fill | height: how a film's stage sits on its canvas
DIP = float(os.environ.get('DIP', '15'))     # how far the key light dips through a flight (degrees)
ENV_ROT = float(os.environ.get('ENV_ROT', '180'))          # the studio turned: keeps its key box out of a tilted lid's mirror
LAPTOP_FOV = float(os.environ.get('LAPTOP_FOV', '28'))   # the laptop close-up's field: narrower covers more of the lid
PROBE = os.environ.get('PROBE', '')                      # a film's name (tablet|phone|words) on a magenta canvas: its extent on the screen showing it
LAPTOP_LOOK = float(os.environ.get('LAPTOP_LOOK', '-0.05'))  # where the laptop close-up looks: the lid's centre, above the body's

W, H = 1440, 900          # the main canvas
T = 26.0                  # every film is this long
FADE = 0.3                # the switch: a short dissolve onto the film already playing
# The screens' shapes (the library bodies): each film is built to the
# shape of the screen that will show it, so the fitted picture IS the screen.
SHAPE = {'laptop': 1.65, 'tablet': 1.463, 'phone': 0.456}
# Each level: the body, its rest pose, when its flight runs, and the
# close-up — the orbit's floor (1.05 radii) and the field that makes the
# screen cover the frame, looking straight at it. The numbers were set by
# eye from stills, not measured off anything.
LEVELS = [
    dict(name='laptop', rest=dict(yaw=-30, pitch=16, distance=4.2, fov=30), fly=(1.5, 7.5),
         close=dict(yaw=0, pitch=12, distance=1.05, fov=LAPTOP_FOV), look=[0, LAPTOP_LOOK, -0.53], body="2B2B2E", floor='glossy',
         bg=("6B5237", "0B0806")),
    dict(name='tablet', rest=dict(yaw=-26, pitch=12, distance=4.2, fov=30), fly=(7.5, 13.5),
         close=dict(yaw=0, pitch=0, distance=1.05, fov=50), look=[0, 0, 0], body="D8DADC", floor='satin',
         bg=("3E5570", "05070C")),
    dict(name='phone', rest=dict(yaw=-24, pitch=10, distance=4.2, fov=30), fly=(13.5, 19.5),
         close=dict(yaw=0, pitch=0, distance=1.05, fov=29.5), look=[0, 0, 0], body="9B978F", floor='glossy',
         bg=("2F5A4B", "05100C")),
]

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

def light(f=0.0):
    # High on the front-right at rest, low and to the front through the
    # middle of a flight (where the glass mirrors it into the camera), up
    # and away before the switch. Only the light moves.
    return dict(yaw=35 - 70 * f, pitch=45 - DIP * math.sin(math.pi * f), intensity=1.05)

def gradient(top, bottom):
    return {"kind": "radial", "start": [0.5, 0.42], "end": [1.2, 0.42], "repeat": "clamp",
            "stops": [{"colorHex": top, "at": 0.0}, {"colorHex": top, "at": 0.12},
                      {"colorHex": bottom, "at": 0.9}, {"colorHex": bottom, "at": 1.0}]}

def film(cw, ch, name, layers):
    probed = PROBE and name == f"Film: the {PROBE}"
    if probed:
        layers = [l for l in layers if l['kind'] != 'background']
    return res("composition", name, duration=T, pixelWidth=cw, pixelHeight=ch,
               composition={"canvasWidth": cw, "canvasHeight": ch,
                            "backgroundColorHex": "FF00FF" if probed else "070707", "layers": layers})

def placed(ch):
    return {"mode": "fill"} if PLACE == 'fill' else {"height": ch, "anchor": "center"}

def stage_keys(level, ch):
    rest, close, look = level['rest'], level['close'], level['look']
    t0, t1 = level['fly']
    cam = lambda c: dict(yaw=c['yaw'], pitch=c['pitch'], roll=0, distance=c['distance'], fov=c['fov'],
                         target={"point": look})
    # Never static: before its flight the film drifts — the camera turns
    # a few degrees — so what plays on a screen is always moving.
    drift = dict(rest, yaw=rest['yaw'] - 7)
    keys = [kf(0, placement=placed(ch), camera=cam(drift), light=light()),
            kf(t0, ramp=t0, easing="linear", placement=placed(ch), camera=cam(rest), light=light())]
    n = 12
    for i in range(1, n + 1):
        f = i / n; t = t0 + (t1 - t0) * f
        # a constant-rate zoom: the distance falls exponentially, the field opens with it
        d = rest['distance'] * (close['distance'] / rest['distance']) ** f
        c = dict(yaw=rest['yaw'] + (close['yaw'] - rest['yaw']) * f,
                 pitch=rest['pitch'] + (close['pitch'] - rest['pitch']) * f,
                 distance=round(d, 4), fov=rest['fov'] + (close['fov'] - rest['fov']) * f)
        keys.append(kf(t, ramp=(t1 - t0) / n, easing="linear", placement=placed(ch), camera=cam(c), light=light(f)))
    # No pause at the switch: the next film's flight starts ON the switch
    # at the same rate, so the motion carries through the fade in the
    # film that is arriving; this one holds its close-up, exactly where
    # the next film picks up — zooming on through the fade would make the
    # picture step down by that much when the fade lands.
    keys.append(kf(T, ramp=T - t1, easing="linear", placement=placed(ch), camera=cam(close), light=light(1.0)))
    return keys

def scene(level, shown, cw, ch):
    """One level's film: a background, and the body on its floor with the
    next film on its glass screen."""
    body = res("model", level['name'].capitalize(), clips=[], recipe={"device": {"kind": level['name']}},
               materials={"Body": {"colorHex": level['body'], "finish": "anodized"},
                          "Deck": {"colorHex": level['body'], "finish": "anodized"},
                          "Screen": {"resourceID": shown["id"], "finish": "glass"}})
    top, bottom = level['bg']
    layers = [layer("Room", 0, "background", 0, T, keyframes=[kf(0, gradient=gradient(top, bottom))]),
              layer(f"{level['name'].capitalize()} on the table", 1, "stage", 0, T, floor=level['floor'],
                    keyframes=stage_keys(level, ch),
                    members=[layer(level['name'].capitalize(), 0, "model", 0, T, resourceID=body["id"],
                                   keyframes=[kf(0, depth=0, stageOffset=[0, 0])])])]
    return film(cw, ch, f"Film: the {level['name']}", layers), [body]

def deepest(cw, ch):
    """The deepest film: a chrome word on a mirror floor, the camera
    drifting round it for the whole length — it never knows it is a texture."""
    def word(text):
        return res("model", text, clips=[], recipe={"text": {"text": text, "bold": True, "depth": 0.25, "size": 0.3}},
                   materials={"Face": {"colorHex": "E4E8EC", "finish": "chrome"}, "Side": {"colorHex": "3A3F46", "finish": "brushed"}})
    promo, shot = word("PROMO"), word("SHOT")
    keys = [kf(0, placement=placed(ch), camera=dict(yaw=-32, pitch=12, roll=0, distance=7.0, fov=30, target={"point": [0, 0.15, 0]}),
               light=dict(yaw=40, pitch=42, intensity=1.05)),
            kf(T, ramp=T, easing="linear", placement=placed(ch), camera=dict(yaw=32, pitch=8, roll=0, distance=6.2, fov=30, target={"point": [0, 0.15, 0]}),
               light=dict(yaw=-30, pitch=30, intensity=1.05))]
    layers = [layer("Room", 0, "background", 0, T, keyframes=[kf(0, gradient=gradient("3A2740", "07050A"))]),
              layer("The words on the floor", 1, "stage", 0, T, floor='mirror', keyframes=keys,
                    members=[layer("PROMO", 0, "model", 0, T, resourceID=promo["id"], keyframes=[kf(0, depth=0, stageOffset=[0, 0.36])]),
                             layer("SHOT", 1, "model", 0, T, resourceID=shot["id"], keyframes=[kf(0, depth=0.02, stageOffset=[0, 0])])])]
    return film(cw, ch, "Film: the words", layers), [promo, shot]

def build():
    resources = []
    # canvases: each film takes the shape of the screen that shows it
    shapes = [(W, H)] + [(round(H * SHAPE[l['name']]), H) for l in LEVELS[:2]] + [(720, round(720 / SHAPE['phone']))]
    films = [None] * 4
    films[3], extra = deepest(*shapes[3]); resources += extra
    for i in (2, 1, 0):
        films[i], extra = scene(LEVELS[i], films[i + 1], *shapes[i]); resources += extra
    resources += films
    # the MAIN: every film from frame one on one clock, the outer ones on
    # top, each dissolving away at its switch onto the one it was showing
    layers = []
    ends = [LEVELS[0]['fly'][1], LEVELS[1]['fly'][1], LEVELS[2]['fly'][1], T]
    for i, f in enumerate(films):
        dur = T if i == 3 else ends[i] + FADE
        layers.append(layer(f["displayName"], 3 - i, "video", 0, dur, resourceID=f["id"],
                            **({"transitionOut": {"kind": "fade", "duration": FADE}} if i < 3 else {}),
                            keyframes=[kf(0, placement={"mode": "fill"})]))
    return {"id": U(), "name": "Through the surfaces", "createdAt": 0, "state": "recorded", "minReaderVersion": 45,
            "trimStart": 0, "trimEnd": T, "videoDuration": T, "subtitles": [],
            "compositionSettings": {"canvasWidth": W, "canvasHeight": H, "backgroundColorHex": "070707",
                                    "environment": {"preset": "studio", "intensity": 1.05, "rotation": ENV_ROT}},
            "resources": resources, "layers": layers}

if __name__ == '__main__':
    doc = build()
    json.dump(doc, open(os.path.join(HERE, 'reference.json'), 'w'), indent=1)
    pkg = os.path.join(HERE, 'runs', 'reference', 'Through the surfaces.promo')
    os.makedirs(os.path.join(pkg, 'Resources'), exist_ok=True)
    json.dump(doc, open(os.path.join(pkg, 'metadata.json'), 'w'), indent=1)
    v = subprocess.run([CLI, 'validate', pkg], capture_output=True, text=True); print(v.stdout.strip()[:600] or v.stderr[-400:])
    work = os.path.dirname(pkg)
    if '--stills' in sys.argv:
        times = "1.0,4.5,7.4,7.9,10.5,13.4,13.9,16.5,19.4,19.9,22.5,25.5"
        subprocess.run([CLI, 'frames', pkg, '--out', os.path.join(work, 'frames'), '--times', times,
                        '--sheet', os.path.join(work, 'sheet.png')], check=False)
    if '--video' in sys.argv:
        subprocess.run([CLI, 'video', pkg, '--out', os.path.join(work, 'through-the-surfaces.mp4')], check=False)
