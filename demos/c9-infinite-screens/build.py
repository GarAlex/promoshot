#!/usr/bin/env python3
"""Infinite screens: a laptop on a table whose screen shows a tablet on a
table whose screen shows a phone on a table whose screen shows a cube
turning under confetti. Four scenes, one zoom each into the device's
screen, a cut onto the next scene.

The seam is the trick: each screen shows a STILL of the next scene's
first frame, fitted; the zoom ends with that picture filling the canvas,
and the cut to the live scene is onto the same picture. So the stills
are derived — this script writes the project with blank stills, renders
each later scene's first frame in reverse order, binds it to the earlier
device's Screen, and writes reference.json.

    python3 demos/c9-infinite-screens/build.py [--render]

Needs the release CLI. --render also writes a contact sheet and the mp4
into the demo's runs/ for a look.
"""
import json, os, shutil, subprocess, sys, uuid
HERE = os.path.dirname(os.path.abspath(__file__)); CORE = os.path.abspath(os.path.join(HERE, '..', '..'))
CLI = os.path.join(CORE, 'target', 'release', 'promo'); RES = os.path.join(HERE, 'resources')
U = lambda: str(uuid.uuid4()).upper()
W, H = 1440, 900          # the cube piece's own canvas, so it plays as authored
FADE = 0.25               # the cut: two pictures moving at one rate, blended briefly
RATE = 0.143              # the flight: ln(scale) per second — doubling every 4.8 s
OVERSHOOT = 1.025         # the picture a hair larger than the canvas at the cut: perspective
                          # makes a screen a trapezoid, and a sliver of bezel in a corner is
                          # worse than a 2.5 % size step under the blend
CUBE_SKIP = 0.45          # land inside the cube piece's own fade-in, where the cube is there
CUBE_REF = os.path.join(HERE, '..', '34-cube-to-word', 'reference.json')

# --- measured at placement height 600 on a 1000-square canvas at the pose
# the zoom ends in: the Screen plate's box (the body is centred by
# construction). The fitted picture inside the screen is the next scene.
MEASURED = {
    'laptop': dict(screen=(107, 231, 892, 738), pose=dict(yaw=0, pitch=-6)),
    'tablet': dict(screen=(130, 222, 869, 779), pose=dict(yaw=0, pitch=0)),
    'phone':  dict(screen=(367, 210, 632, 789), pose=dict(yaw=0, pitch=0)),
}
ASPECT = W / H
def end_placement(name):
    sx0, sy0, sx1, sy1 = MEASURED[name]['screen']
    sw, sh = sx1 - sx0 + 1, sy1 - sy0 + 1
    if sw / sh > ASPECT: pw, ph = sh * ASPECT, sh    # bars at the sides: the picture is the screen's height
    else: pw, ph = sw, sw / ASPECT                    # bars top and bottom: the picture is the screen's width
    pcx, pcy = (sx0 + sx1) / 2, (sy0 + sy1) / 2
    s = H / ph
    s *= OVERSHOOT
    return dict(height=600 * s, offset=[-(pcx - 499.5) * s, -(pcy - 499.0) * s])

def kf(time, **f):
    d = {"id": U(), "time": time, "transitionDuration": f.pop('ramp', 0)}; d.update(f); return d
def res(kind, filename, name, **f):
    d = {"id": U(), "kind": kind, "filename": filename, "displayName": name, "addedAt": 0,
         "imageCuts": [], "disabledAudioTrackIndices": []}; d.update(f); return d
def layer(name, sort, kind, start, dur, **f):
    d = {"id": U(), "name": name, "sortIndex": sort, "kind": kind, "isEnabled": True,
         "startTime": start, "duration": dur, "keyframes": []}; d.update(f); return d

FINISH = {'laptop': ("2B2B2E", 1, 0.40), 'tablet': ("D8DADC", 1, 0.35), 'phone': ("9B978F", 1, 0.45)}
DEVICES = [('laptop', 'DeviceLaptop.glb', 1.095, dict(yaw=-30, pitch=16), 520, 40, 'bg_laptop.png'),
           ('tablet', 'DeviceTablet.glb', 0.864, dict(yaw=-26, pitch=12), 480, 30, 'bg_tablet.png'),
           ('phone',  'DevicePhone.glb',  0.395, dict(yaw=-24, pitch=10), 700, 20, 'bg_phone.png')]

def flight(h0, h1):
    """How long a dive from h0 to h1 takes at the one rate."""
    import math
    return math.log(h1 / h0) / RATE

def exp_ramp(t_end, value0, value1, steps=12, mapper=lambda v: v):
    """Keyframes along an exponential from value0 to value1 over t_end,
    linear between — a zoom at one rate, which is what flying forward
    looks like. `mapper` turns the scalar into the keyframe's fields."""
    import math
    out = []
    for i in range(steps + 1):
        f = i / steps
        v = value0 * math.exp(math.log(value1 / value0) * f)
        out.append((t_end * f, v))
    return out

def build(stills, fades=True):
    resources, layers = [], []
    cube = json.load(open(CUBE_REF))
    settings = dict(cube['compositionSettings'])
    settings.update({"canvasWidth": W, "canvasHeight": H})
    floor = res("image", "floor.png", "Floor", pixelWidth=4096, pixelHeight=900); resources.append(floor)
    shadow = res("image", "shadow.png", "Shadow", pixelWidth=1600, pixelHeight=500); resources.append(shadow)
    sort = 0; t0 = 0.0; scenes = []
    for i, (name, glb, radius, start_pose, h0, off0, bg) in enumerate(DEVICES):
        end = end_placement(name); pose = MEASURED[name]['pose']
        dur = flight(h0, end['height']); grow = end['height'] / h0
        scenes.append((name, t0, dur))
        fade = FADE if (i > 0 and fades) else None
        bgres = res("image", bg, f"Background {i+1}", pixelWidth=4096, pixelHeight=2700); resources.append(bgres)
        still = res("image", stills[i], f"Screen {i+1}", pixelWidth=W, pixelHeight=H); resources.append(still)
        hexv, met, rough = FINISH[name]
        body = res("model", glb, name.capitalize(), boundsRadius=radius, clips=[],
                   materials={"Screen": {"resourceID": still["id"]},
                              "Body": {"colorHex": hexv, "metallic": met, "roughness": rough},
                              "Deck": {"colorHex": hexv, "metallic": met, "roughness": rough}})
        resources.append(body)
        tail = dur + (FADE if fades else 0)
        # The whole scene flies: background, floor, shadow and body all
        # grow at the one rate — exactly what the still on the previous
        # screen was doing — while the background also pans slowly.
        steps = exp_ramp(dur, 1.0, grow)
        bg_keys = []
        for j, (t, g) in enumerate(steps):
            pan = -160 + 320 * (t / dur)
            bg_keys.append(kf(t, ramp=(t - steps[j-1][0]) if j else 0, easing="linear",
                              placement={"height": 1150 * g, "anchor": "center", "offset": [pan * g, 0]}))
        layers.append(layer(f"Background {i+1}", sort, "image", t0, tail, resourceID=bgres["id"],
                            **({"fadeIn": fade} if fade else {}), keyframes=bg_keys)); sort += 1
        floor_keys = [kf(t, ramp=(t - steps[j-1][0]) if j else 0, easing="linear",
                         placement={"width": 1700 * g, "anchor": "bottom", "offset": [0, (g - 1) * 260]})
                      for j, (t, g) in enumerate(steps)]
        layers.append(layer(f"Floor {i+1}", sort, "image", t0, tail, resourceID=floor["id"],
                            **({"fadeIn": fade} if fade else {}), keyframes=floor_keys)); sort += 1
        sh_y = off0 + h0 / 2 - 16
        shadow_keys = [kf(t, ramp=(t - steps[j-1][0]) if j else 0, easing="linear",
                          placement={"height": 120 * g, "anchor": "center", "offset": [0, sh_y * g]},
                          opacity=max(0.0, 0.9 - 1.6 * (t / dur)))
                       for j, (t, g) in enumerate(steps)]
        layers.append(layer(f"Shadow {i+1}", sort, "image", t0, tail, resourceID=shadow["id"],
                            **({"fadeIn": fade} if fade else {}), keyframes=shadow_keys)); sort += 1
        light = dict(yaw=15, pitch=50, intensity=1.0)
        body_keys = []
        for j, (t, g) in enumerate(steps):
            f = t / dur
            cam = dict(yaw=start_pose['yaw'] + (pose['yaw'] - start_pose['yaw']) * f,
                       pitch=start_pose['pitch'] + (pose['pitch'] - start_pose['pitch']) * f,
                       roll=0, distance=4.2, fov=30)
            # the body's centre slides from its opening spot to where the
            # screen's picture is centred on the canvas, as it grows
            ox = end['offset'][0] * f
            oy = off0 * g * (1 - f) + end['offset'][1] * f
            body_keys.append(kf(t, ramp=(t - steps[j-1][0]) if j else 0, easing="linear",
                                placement={"height": h0 * g, "anchor": "center", "offset": [ox, oy]},
                                camera=cam, light=light))
        layers.append(layer(name.capitalize(), sort, "model", t0, tail, resourceID=body["id"],
                            **({"fadeIn": fade} if fade else {}), keyframes=body_keys)); sort += 1
        t0 += dur
    # --- the final scene: the cube piece as it was authored, joined at
    # CUBE_SKIP so the cut lands with the cube already there (its own
    # fade-in would have the flight arrive into an empty gradient). A
    # stage's members carry their own times, so they move with it.
    shift = t0 - CUBE_SKIP
    for r in cube['resources']:
        resources.append(r)
    for l in cube['layers']:
        moved = json.loads(json.dumps(l)); moved['startTime'] = l['startTime'] + shift
        moved['sortIndex'] = sort; sort += 1
        for member in moved.get('members', []):
            member['startTime'] = member.get('startTime', 0) + shift
        layers.append(moved)
    total = t0 + cube['videoDuration'] - CUBE_SKIP
    return {"id": U(), "name": "Infinite screens", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": total, "videoDuration": total, "subtitles": [],
            "minReaderVersion": 42, "compositionSettings": settings, "resources": resources, "layers": layers,
            "_scenes": scenes}

def materialize(meta, pkg):
    shutil.rmtree(pkg, ignore_errors=True); os.makedirs(pkg + '/Resources')
    for f in os.listdir(RES): shutil.copy(os.path.join(RES, f), pkg + '/Resources/' + f)
    json.dump(meta, open(pkg + '/metadata.json', 'w'), indent=1)

def still(pkg, t, out):
    r = subprocess.run([CLI, 'still', pkg, '--out', out, '--time', str(t), '--size', f'{W}x{H}'], capture_output=True, text=True)
    if r.returncode: sys.exit(f'still failed at {t}: {r.stderr[-400:]}')

if __name__ == '__main__':
    from PIL import Image
    work = os.path.join(HERE, 'runs', 'build'); os.makedirs(work, exist_ok=True)
    pkg = os.path.join(work, 'Infinite screens.promo')
    names = ['still_s2.png', 'still_s3.png', 'still_s4.png']
    for n in names:  # blank stand-ins so the first pass decodes
        Image.new('RGB', (W, H), (11, 13, 18)).save(os.path.join(RES, n))
    # The stills come from a build WITHOUT the cut fades — a scene's true
    # first frame — in reverse order, so the tablet's screen already shows
    # the phone when the laptop's still is taken.
    for scene, name in ((4, 'still_s4.png'), (3, 'still_s3.png'), (2, 'still_s2.png')):
        meta = build(names, fades=False); scenes = meta.pop('_scenes')
        materialize(meta, pkg)
        at = scenes[scene - 1][1] if scene - 1 < len(scenes) else scenes[-1][1] + scenes[-1][2]
        still(pkg, at + 0.02, os.path.join(RES, name))
        print('rendered', name, 'at', round(at + 0.02, 2))
    meta = build(names); scenes = meta.pop('_scenes'); materialize(meta, pkg)
    json.dump(meta, open(os.path.join(HERE, 'reference.json'), 'w'), indent=1)
    print('scenes:', [(n, round(t, 2), round(d, 2)) for n, t, d in scenes], 'total', round(meta['videoDuration'], 2))
    v = subprocess.run([CLI, 'validate', pkg], capture_output=True, text=True); print(v.stdout.strip()[:600])
    if '--render' in sys.argv:
        cuts = [t for _, t, _ in scenes[1:]] + [scenes[-1][1] + scenes[-1][2]]
        times = [1.0] + [x for c in cuts for x in (c - 0.1, c + 0.3)] + [meta['videoDuration'] - 5, meta['videoDuration'] - 1.5]
        subprocess.run([CLI, 'frames', pkg, '--out', os.path.join(work, 'frames'), '--times',
                        ','.join(f'{t:.2f}' for t in times), '--size', '720x450',
                        '--sheet', os.path.join(work, 'sheet.png')], check=False)
        subprocess.run([CLI, 'video', pkg, '--out', os.path.join(work, 'infinite-screens.mp4')], check=False)
