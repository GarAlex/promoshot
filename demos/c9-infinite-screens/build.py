#!/usr/bin/env python3
"""Infinite screens: a laptop on a table whose screen shows a tablet on a
table whose screen shows a phone on a table whose screen shows a cube
turning under confetti. Four scenes, one zoom each into the device's
screen, a cut onto the next scene.

Every scene is a COMPOSITION, and each device's Screen slot binds the
next scene's composition (rung 43) — the screen is the live next scene,
its background already scrolling long before the flight reaches it.
All four compositions start on the film's first frame: the one on a
screen and the one that later fills the canvas are the same document on
the same clock, so the cut changes nothing. The top-level layers show
the four compositions stacked, each revealed as the one above ends.

    python3 demos/c9-infinite-screens/build.py [laptop-first|phone-first] [--render]

Two films from the same scenes. `laptop-first` (the default) is the one
above. `phone-first` runs the other way — a phone, a tablet, a laptop,
the cube — so the sizes climb towards the finale, and carries one
headline caption per device scene. Needs the release CLI. --render also
writes a contact sheet and the mp4 into the demo's runs/ for a look.
"""
import json, os, shutil, subprocess, sys, uuid
HERE = os.path.dirname(os.path.abspath(__file__)); CORE = os.path.abspath(os.path.join(HERE, '..', '..'))
CLI = os.path.join(CORE, 'target', 'release', 'promo'); RES = os.path.join(HERE, 'resources')
U = lambda: str(uuid.uuid4()).upper()
W, H = 1440, 900          # the cube piece's own canvas, so it plays as authored
FADE = 0.25               # the cut: the same document seen through the screen, then whole
RATE = 0.143              # the flight: ln(scale) per second — doubling every 4.8 s
OVERSHOOT = 1.025         # the picture a hair larger than the canvas at the cut
CUBE_SKIP = 0.45          # land inside the cube piece's own fade-in, where the cube is there
PAN_SPEED = 120           # the background's scroll, px/s at its own scale
BG_WIDTH = 5600           # the background placed far wider than the canvas: room to scroll
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
    if sw / sh > ASPECT: pw, ph = sh * ASPECT, sh
    else: pw, ph = sw, sw / ASPECT
    pcx, pcy = (sx0 + sx1) / 2, (sy0 + sy1) / 2
    s = H / ph * OVERSHOOT
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
DEVICE = {d[0]: d for d in DEVICES}
FILMS = {
    'laptop-first': dict(name='Infinite screens', order=('laptop', 'tablet', 'phone'), captions=None,
                         out='build', reference='reference.json', video='infinite-screens.mp4'),
    'phone-first': dict(name='Infinite screens, phone first', order=('phone', 'tablet', 'laptop'),
                        captions=('One project, four compositions', 'Every screen plays the next scene',
                                  'Zoom all the way in'),
                        out='phone-first', reference='reference-phone-first.json',
                        video='infinite-screens-phone-first.mp4'),
}
CAPTION_STYLE = {"alignment": "center", "fontSize": 52, "isBold": True, "textColorHex": "FFFFFF",
                 "placement": {"anchor": "top", "offset": [0, 40]}, "padding": 16,
                 "shadowColorHex": "000000", "shadowOpacity": 0.6, "shadowRadius": 12}

def flight(h0, h1):
    import math
    return math.log(h1 / h0) / RATE

def exp_steps(t0, t1, grow, steps=12):
    """(time, growth) along an exponential from 1 to grow over t0..t1."""
    import math
    return [(t0 + (t1 - t0) * i / steps, math.exp(math.log(grow) * i / steps)) for i in range(steps + 1)]

def scene(name, glb, radius, start_pose, h0, off0, bg, floor, shadow, next_comp, rest, total):
    """One scene's composition: at rest (background panning) until `rest`,
    then the flight to the screen, then holding for whatever is left."""
    end = end_placement(name); pose = MEASURED[name]['pose']
    dur = flight(h0, end['height']); grow = end['height'] / h0
    hexv, met, rough = FINISH[name]
    layers = []
    bgres = res("image", bg, f"Background {name}", pixelWidth=4096, pixelHeight=2700)
    screen = {"resourceID": next_comp["id"]}
    body = res("model", glb, name.capitalize(), boundsRadius=radius, clips=[],
               materials={"Screen": screen,
                          "Body": {"colorHex": hexv, "metallic": met, "roughness": rough},
                          "Deck": {"colorHex": hexv, "metallic": met, "roughness": rough}})
    resources = [bgres, body]
    steps = exp_steps(rest, rest + dur, grow)
    # The background SCROLLS, plainly: a placement far wider than the canvas
    # panned at about PAN_SPEED px/s of its own scale for the whole film,
    # alternating direction scene to scene. It has to read through a
    # screen, and through a screen on a screen — a drift of a few px/s
    # did not. The pan runs from the film's first frame; the growth from
    # `rest`, and the pan scales with it.
    direction = 1 if name in ('laptop', 'phone') else -1
    pan = lambda t: direction * (PAN_SPEED * (rest + dur) / 2 - PAN_SPEED * t)
    bg_keys = [kf(0, placement={"width": BG_WIDTH, "anchor": "center", "offset": [pan(0), 0]})]
    bg_keys += [kf(t, ramp=(t - (steps[j-1][0] if j else 0)), easing="linear",
                   placement={"width": BG_WIDTH * g, "anchor": "center", "offset": [pan(t) * g, 0]})
                for j, (t, g) in enumerate(steps)]
    layers.append(layer(f"Background {name}", 0, "image", 0, total, resourceID=bgres["id"], keyframes=bg_keys))
    floor_keys = [kf(0, placement={"width": 1700, "anchor": "bottom"})]
    floor_keys += [kf(t, ramp=(t - (steps[j-1][0] if j else 0)), easing="linear",
                      placement={"width": 1700 * g, "anchor": "bottom", "offset": [0, (g - 1) * 260]})
                   for j, (t, g) in enumerate(steps)]
    layers.append(layer(f"Floor {name}", 1, "image", 0, total, resourceID=floor["id"], keyframes=floor_keys))
    sh_y = off0 + h0 / 2 - 16
    shadow_keys = [kf(0, placement={"height": 120, "anchor": "center", "offset": [0, sh_y]}, opacity=0.9)]
    shadow_keys += [kf(t, ramp=(t - (steps[j-1][0] if j else 0)), easing="linear",
                       placement={"height": 120 * g, "anchor": "center", "offset": [0, sh_y * g]},
                       opacity=max(0.0, 0.9 - 1.6 * ((t - rest) / dur)))
                    for j, (t, g) in enumerate(steps)]
    layers.append(layer(f"Shadow {name}", 2, "image", 0, total, resourceID=shadow["id"], keyframes=shadow_keys))
    light = dict(yaw=15, pitch=50, intensity=1.0)
    cam0 = dict(yaw=start_pose['yaw'], pitch=start_pose['pitch'], roll=0, distance=4.2, fov=30)
    body_keys = [kf(0, placement={"height": h0, "anchor": "center", "offset": [0, off0]}, camera=cam0, light=light)]
    for j, (t, g) in enumerate(steps):
        f = (t - rest) / dur
        cam = dict(yaw=start_pose['yaw'] + (pose['yaw'] - start_pose['yaw']) * f,
                   pitch=start_pose['pitch'] + (pose['pitch'] - start_pose['pitch']) * f,
                   roll=0, distance=4.2, fov=30)
        body_keys.append(kf(t, ramp=(t - (steps[j-1][0] if j else 0)), easing="linear",
                            placement={"height": h0 * g, "anchor": "center",
                                       "offset": [end['offset'][0] * f, off0 * g * (1 - f) + end['offset'][1] * f]},
                            camera=cam, light=light))
    layers.append(layer(name.capitalize(), 3, "model", 0, total, resourceID=body["id"], keyframes=body_keys))
    comp = res("composition", "", f"Scene {name}", duration=total, pixelWidth=W, pixelHeight=H,
               composition={"canvasWidth": W, "canvasHeight": H, "backgroundColorHex": "0B0D12", "layers": layers})
    return comp, resources, dur

def cube_scene(cube, pre, total):
    """The cube piece as a composition that starts `pre` seconds before the
    piece does: the cube already turning at its own rate, everything else
    holding its first pose, so the phone's screen shows a living cube and
    the cut lands inside the piece as authored."""
    import copy
    layers = []
    for l in cube['layers']:
        moved = copy.deepcopy(l); moved['startTime'] = l['startTime'] + pre
        if l['kind'] in ('background', 'stage'):
            moved['startTime'] = 0; moved['duration'] = l['duration'] + pre
            for k in moved['keyframes']: k['time'] += pre
            first = copy.deepcopy(moved['keyframes'][0]); first['time'] = 0; first['transitionDuration'] = 0
            first.pop('easing', None); first['id'] = U()
            moved['keyframes'][0]['transitionDuration'] = 0
            moved['keyframes'].insert(0, first)
            for member in moved.get('members', []):
                member['startTime'] = 0; member['duration'] = member['duration'] + pre
                for k in member['keyframes']: k['time'] += pre
                first = copy.deepcopy(member['keyframes'][0]); first['time'] = 0; first['transitionDuration'] = 0
                first.pop('easing', None); first['id'] = U()
                if member['name'] == 'Cube':
                    # the turn continues backwards at the piece's own rate
                    rate = 380.0 / 5.2
                    first['camera'] = {"yaw": -rate * pre}
                    member['keyframes'][0]['transitionDuration'] = pre
                    member['keyframes'][0]['easing'] = 'linear'
                else:
                    member['keyframes'][0]['transitionDuration'] = 0
                member['keyframes'].insert(0, first)
        layers.append(moved)
    return res("composition", "", "Scene cube", duration=total, pixelWidth=W, pixelHeight=H,
               composition={"canvasWidth": W, "canvasHeight": H, "backgroundColorHex": "0B0D12", "layers": layers})

def build(film):
    order, captions = film['order'], film['captions']
    devices = [DEVICE[n] for n in order]
    cube = json.load(open(CUBE_REF))
    settings = dict(cube['compositionSettings']); settings.update({"canvasWidth": W, "canvasHeight": H})
    resources = list(cube['resources'])
    floor = res("image", "floor.png", "Floor", pixelWidth=4096, pixelHeight=900); resources.append(floor)
    shadow = res("image", "shadow.png", "Shadow", pixelWidth=1600, pixelHeight=500); resources.append(shadow)
    # the flight's timing first, so every composition knows the film's length
    durs = [flight(h0, end_placement(n)['height']) for (n, _, _, _, h0, _, _) in devices]
    starts = [sum(durs[:i]) for i in range(3)]
    t3 = sum(durs); total = t3 + cube['videoDuration'] - CUBE_SKIP
    comps = [None] * 4
    comps[3] = cube_scene(cube, t3 - CUBE_SKIP, total)
    for i in (2, 1, 0):
        name, glb, radius, pose0, h0, off0, bg = devices[i]
        comp, extra, _ = scene(name, glb, radius, pose0, h0, off0, bg, floor, shadow, comps[i + 1], starts[i], total)
        comps[i] = comp; resources += extra
    resources += comps
    layers = []
    ends = starts[1:] + [t3]
    for i, comp in enumerate(comps):
        dur = total if i == 3 else ends[i] + FADE
        layers.append(layer(comp["displayName"], 3 - i, "video", 0, dur, resourceID=comp["id"],
                            **({"transitionOut": {"kind": "fade", "duration": FADE}} if i < 3 else {}),
                            keyframes=[kf(0, placement={"mode": "fill", "anchor": "center"})]))
    # One headline per device scene, at the top, over the flight's first
    # part: in after the cut has settled, out well before the next one,
    # so no two captions ever share a frame — and the cube piece keeps
    # its own title.
    for i, text in enumerate(captions or ()):
        start = starts[i] + (0.3 if i == 0 else FADE + 0.15); end = starts[i] + 0.62 * durs[i]
        layers.append(layer(f"Caption {order[i]}", 10 + i, "caption", start, end - start, captionText=text,
                            fadeIn=0.35, fadeOut=0.35, captionStyle=dict(CAPTION_STYLE)))
    return {"id": U(), "name": film['name'], "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": total, "videoDuration": total, "subtitles": [],
            "minReaderVersion": 43, "compositionSettings": settings, "resources": resources, "layers": layers,
            "_scenes": list(zip(order, starts, durs))}

def materialize(meta, pkg):
    shutil.rmtree(pkg, ignore_errors=True); os.makedirs(pkg + '/Resources')
    for f in os.listdir(RES):
        if not f.startswith('still_'): shutil.copy(os.path.join(RES, f), pkg + '/Resources/' + f)
    json.dump(meta, open(pkg + '/metadata.json', 'w'), indent=1)

if __name__ == '__main__':
    film = FILMS[next((a for a in sys.argv[1:] if a in FILMS), 'laptop-first')]
    work = os.path.join(HERE, 'runs', film['out']); os.makedirs(work, exist_ok=True)
    pkg = os.path.join(work, film['name'] + '.promo')
    meta = build(film); scenes = meta.pop('_scenes'); materialize(meta, pkg)
    json.dump(meta, open(os.path.join(HERE, film['reference']), 'w'), indent=1)
    print('scenes:', [(n, round(t, 2), round(d, 2)) for n, t, d in scenes], 'total', round(meta['videoDuration'], 2))
    v = subprocess.run([CLI, 'validate', pkg], capture_output=True, text=True); print(v.stdout.strip()[:800])
    if '--render' in sys.argv:
        cuts = [t for _, t, _ in scenes[1:]] + [scenes[-1][1] + scenes[-1][2]]
        times = [1.0, 3.0] + [x for c in cuts for x in (c - 0.1, c + 0.3)] + [meta['videoDuration'] - 5, meta['videoDuration'] - 1.5]
        subprocess.run([CLI, 'frames', pkg, '--out', os.path.join(work, 'frames'), '--times',
                        ','.join(f'{t:.2f}' for t in times), '--size', '720x450',
                        '--sheet', os.path.join(work, 'sheet.png')], check=False)
        subprocess.run([CLI, 'video', pkg, '--out', os.path.join(work, film['video'])], check=False)
