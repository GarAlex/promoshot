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
headline caption per device scene. Its scenes are SCREEN-SHAPED: a
scene that plays on a screen is built to that screen's proportions (a
phone's is tall), so it fills the screen top to bottom with no bars,
while a W×H band of it — the part the flight lands on — is what the top
level shows. Needs the release CLI. --render also writes a contact
sheet and the mp4 into the demo's runs/ for a look.
"""
import json, os, shutil, subprocess, sys, uuid
HERE = os.path.dirname(os.path.abspath(__file__)); CORE = os.path.abspath(os.path.join(HERE, '..', '..'))
CLI = os.path.join(CORE, 'target', 'release', 'promo'); RES = os.path.join(HERE, 'resources')
U = lambda: str(uuid.uuid4()).upper()
W, H = 1440, 900          # the cube piece's own canvas, so it plays as authored
FADE = 0.25               # the cut: the same document seen through the screen, then whole
RATE = 0.143              # the flight: ln(scale) per second — doubling every 4.8 s (a film may set its own)
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
def end_placement(name, band_v=0.5):
    """Where the flight lands: the placement that puts the W×H picture
    on this device's screen exactly over the canvas. `band_v` is where
    that picture's centre sits down the screen — the middle of a fitted
    landscape picture, or the band of a screen-shaped scene."""
    sx0, sy0, sx1, sy1 = MEASURED[name]['screen']
    sw, sh = sx1 - sx0 + 1, sy1 - sy0 + 1
    if sw / sh > ASPECT: pw, ph = sh * ASPECT, sh
    else: pw, ph = sw, sw / ASPECT
    pcx, pcy = (sx0 + sx1) / 2, sy0 + band_v * (sy1 - sy0)
    s = H / ph * OVERSHOOT
    return dict(height=600 * s, offset=[-(pcx - 499.5) * s, -(pcy - 499.0) * s])

# The screens' shapes, from the Screen plates in the app's
# Scripts/make-device-models.py (a slot's aspect follows its uvs). A
# scene built to the shape of the screen it plays on fills that screen.
SCREEN_ASPECT = {'phone': 66.5 / 144.6, 'tablet': 267.6 / 201.5, 'laptop': 301.6 / 198.2}

# The keyboard, measured like the screens: the keys' box at placement
# height 600 on a 1000-square, the laptop seen from above (yaw 0, pitch
# 62). DeviceLaptopKeys.glb gives the letters of PROMOSHOT their own
# slots (Key P …), built by the app's Scripts/make-device-models.py with
# the kind `laptopkeys`.
KEYBOARD = dict(box=(231, 430, 768, 629), pose=dict(yaw=0, pitch=62))

def keyboard_placement(share=0.86, band_dy=0):
    """The placement that lays the keyboard across `share` of the canvas
    width, centred — every key in view."""
    kx0, ky0, kx1, ky1 = KEYBOARD['box']
    s = share * W / (kx1 - kx0 + 1)
    return dict(height=600 * s, offset=[-((kx0 + kx1) / 2 - 499.5) * s, -((ky0 + ky1) / 2 - 499.0) * s + band_dy])

def screen_canvas(aspect):
    """A canvas W wide in a screen's shape: (height, the centre of its
    W×H band as a fraction of the height). A tall canvas keeps the band
    below the middle — wall above the table, less table below. Even
    padding keeps the band on whole pixels."""
    ch = max(H, int(round(W / aspect)))
    if (ch - H) % 2: ch += 1
    return ch, (0.62 if ch > 1.5 * H else 0.5)

def kf(time, **f):
    d = {"id": U(), "time": time, "transitionDuration": f.pop('ramp', 0)}; d.update(f); return d
def res(kind, filename, name, **f):
    d = {"id": U(), "kind": kind, "filename": filename, "displayName": name, "addedAt": 0,
         "imageCuts": [], "disabledAudioTrackIndices": []}; d.update(f); return d
def layer(name, sort, kind, start, dur, **f):
    d = {"id": U(), "name": name, "sortIndex": sort, "kind": kind, "isEnabled": True,
         "startTime": start, "duration": dur, "keyframes": []}; d.update(f); return d

# What each body IS, by word (rung 44): anodized metal in the device's
# colour; every screen wears glass over the scene it plays, and the
# stage each body stands on has a glossy floor (rung 45) that mirrors
# it and takes its shadow from the key light.
FINISH = {'laptop': "2B2B2E", 'tablet': "D8DADC", 'phone': "9B978F"}
BODY_FINISH, SCREEN_FINISH, FLOOR = "anodized", "glass", "glossy"
DEVICES = [('laptop', 'DeviceLaptop.glb', 1.095, dict(yaw=-30, pitch=16), 520, 40, 'bg_laptop.png'),
           ('tablet', 'DeviceTablet.glb', 0.864, dict(yaw=-26, pitch=12), 480, 30, 'bg_tablet.png'),
           ('phone',  'DevicePhone.glb',  0.395, dict(yaw=-24, pitch=10), 700, 20, 'bg_phone.png')]
DEVICE = {d[0]: d for d in DEVICES}
FILMS = {
    'laptop-first': dict(name='Infinite screens', order=('laptop', 'tablet', 'phone'), captions=None,
                         out='build', reference='reference.json', video='infinite-screens.mp4'),
    'phone-first': dict(name='Infinite screens, phone first', order=('phone', 'tablet', 'laptop'),
                        captions=('One project, four compositions', 'Every screen plays the next scene',
                                  'Zoom all the way in'), screen_shaped=True,
                        # farther away at the start and a faster flight — doubling every
                        # 3.5 s rather than 4.8 — for a more dynamic film of about the
                        # same length
                        distance=0.8, rate=0.2,
                        # the laptop scene types PROMOSHOT before its flight: a close-up
                        # on the keyboard, one key lit per beat, back out, a pause
                        typing=dict(scene='laptop', glb='DeviceLaptopKeys.glb', word='PROMOSHOT',
                                    beat=0.3, move=1.0, pause=1.0, color='FFB020'),
                        out='phone-first', reference='reference-phone-first.json',
                        video='infinite-screens-phone-first.mp4'),
}
CAPTION_STYLE = {"alignment": "center", "fontSize": 52, "isBold": True, "textColorHex": "FFFFFF",
                 "placement": {"anchor": "top", "offset": [0, 40]}, "padding": 16,
                 "shadowColorHex": "000000", "shadowOpacity": 0.6, "shadowRadius": 12}

def flight(h0, h1, rate=RATE):
    import math
    return math.log(h1 / h0) / rate

def exp_steps(t0, t1, grow, steps=12):
    """(time, growth) along an exponential from 1 to grow over t0..t1."""
    import math
    return [(t0 + (t1 - t0) * i / steps, math.exp(math.log(grow) * i / steps)) for i in range(steps + 1)]

def typing_prologue(typing):
    """How long the typing takes before the flight: in to the keys, the
    word plus a beat's hold, back out, and the pause."""
    return 2 * typing['move'] + (len(typing['word']) + 1) * typing['beat'] + typing['pause']

def scene(name, glb, radius, start_pose, h0, off0, bg, floor, shadow, next_comp, rest, total,
          canvas=(H, 0.5), next_band_v=0.5, table=None, rate=RATE, typing=None):
    """One scene's composition: at rest (background panning) until `rest`,
    then the flight to the screen, then holding for whatever is left.
    `canvas` is (height, band centre): the scene lives in a W×H BAND of
    a canvas W wide — the whole canvas when it is H tall, else the band
    the top level shows of a screen-shaped scene; everything is placed
    about the band's centre, and the flight grows about it."""
    ch, band_v = canvas
    band_dy = int(round((band_v - 0.5) * ch))     # the band's centre below the canvas centre
    below = ch - (ch // 2 + band_dy + H // 2)     # canvas rows under the band: the table's front
    end = end_placement(name, next_band_v); pose = MEASURED[name]['pose']
    dur = flight(h0, end['height'], rate); grow = end['height'] / h0
    # A typing prologue delays the flight: the body flies from `rest`
    # plus the prologue, and everything else holds through it (the
    # background keeps panning).
    prologue = typing_prologue(typing) if typing else 0.0
    rest = rest + prologue
    hexv = FINISH[name]
    layers = []
    bgres = res("image", bg, f"Background {name}", pixelWidth=4096, pixelHeight=2700)
    screen = {"resourceID": next_comp["id"], "finish": SCREEN_FINISH}
    paint = {"colorHex": hexv, "finish": BODY_FINISH}
    body = res("model", glb, name.capitalize(), boundsRadius=radius, clips=[],
               materials={"Screen": screen, "Body": dict(paint), "Deck": dict(paint)})
    resources = [bgres, body]
    # The lit bodies: the same laptop with one letter's key in the
    # highlight colour, one resource per letter, each on its own short
    # layer over the base while its key is "pressed".
    lit = {}
    if typing:
        for L in sorted(set(typing['word'])):
            lit[L] = res("model", glb, f"{name.capitalize()} {L} lit", boundsRadius=radius, clips=[],
                         materials={"Screen": screen, "Body": dict(paint), "Deck": dict(paint),
                                    f"Key {L}": {"colorHex": typing['color'], "finish": "gloss"}})
            resources.append(lit[L])
    steps = exp_steps(rest, rest + dur, grow)
    # The background SCROLLS, plainly: a placement far wider than the canvas
    # panned at about PAN_SPEED px/s of its own scale for the whole film,
    # alternating direction scene to scene. It has to read through a
    # screen, and through a screen on a screen — a drift of a few px/s
    # did not. The pan runs from the film's first frame; the growth from
    # `rest`, and the pan scales with it.
    direction = 1 if name in ('laptop', 'phone') else -1
    pan = lambda t: direction * (PAN_SPEED * (rest + dur) / 2 - PAN_SPEED * t)
    bg_keys = [kf(0, placement={"width": BG_WIDTH, "anchor": "center", "offset": [pan(0), band_dy]})]
    bg_keys += [kf(t, ramp=(t - (steps[j-1][0] if j else 0)), easing="linear",
                   placement={"width": BG_WIDTH * g, "anchor": "center", "offset": [pan(t) * g, band_dy]})
                for j, (t, g) in enumerate(steps)]
    layers.append(layer(f"Background {name}", 0, "image", 0, total, resourceID=bgres["id"], keyframes=bg_keys))
    up = 1 if below else 0                        # a table front takes a row of the z-order
    if below:
        # The table's front: the floor's own dark from under its opaque
        # part to the canvas bottom, seen only through a screen.
        layers.append(layer(f"Table {name}", 1, "image", 0, total, resourceID=table["id"],
                            keyframes=[kf(0, placement={"height": below + 180, "anchor": "bottom"})]))
    floor_keys = [kf(0, placement={"width": 1700, "anchor": "bottom", **({"offset": [0, -below]} if below else {})})]
    floor_keys += [kf(t, ramp=(t - (steps[j-1][0] if j else 0)), easing="linear",
                      placement={"width": 1700 * g, "anchor": "bottom", "offset": [0, (g - 1) * 260 - below]})
                   for j, (t, g) in enumerate(steps)]
    layers.append(layer(f"Floor {name}", 1 + up, "image", 0, total, resourceID=floor["id"], keyframes=floor_keys))
    # The key light: from the front-right at rest, swinging left and lower
    # through the flight, so the highlight crosses the screen's glass and
    # the shadow on the floor turns with it — only the light moves.
    def light(f=0.0):
        return dict(yaw=15 - 50 * f, pitch=50 - 12 * f, intensity=1.05)
    cam0 = dict(yaw=start_pose['yaw'], pitch=start_pose['pitch'], roll=0, distance=4.2, fov=30)
    at_rest = dict(placement={"height": h0, "anchor": "center", "offset": [0, off0 + band_dy]}, camera=cam0, light=light())
    body_keys = [kf(0, **at_rest)]
    if typing:
        # In to the keyboard (eased), hold while the word is typed, back
        # out (eased), and the pause: the flight's first step then ramps
        # from the rest pose to itself over the pause, a hold.
        move, beat, word = typing['move'], typing['beat'], typing['word']
        start = rest - prologue
        close = dict(placement=dict(keyboard_placement(band_dy=band_dy), anchor="center"),
                     camera=dict(yaw=KEYBOARD['pose']['yaw'], pitch=KEYBOARD['pose']['pitch'], roll=0, distance=4.2, fov=30),
                     light=light())
        body_keys.append(kf(start + move, ramp=move, easing="easeInOut", **close))
        typed = start + move + (len(word) + 1) * beat
        body_keys.append(kf(typed + move, ramp=move, easing="easeInOut", **at_rest))
        for k, L in enumerate(word):
            layers.append(layer(f"Key {k + 1} {L}", 5 + up + k, "model", start + move + k * beat, beat,
                                resourceID=lit[L]["id"], keyframes=[kf(0, **close)]))
    for j, (t, g) in enumerate(steps):
        f = (t - rest) / dur
        cam = dict(yaw=start_pose['yaw'] + (pose['yaw'] - start_pose['yaw']) * f,
                   pitch=start_pose['pitch'] + (pose['pitch'] - start_pose['pitch']) * f,
                   roll=0, distance=4.2, fov=30)
        body_keys.append(kf(t, ramp=(t - (steps[j-1][0] if j else (rest - typing['pause'] if typing else 0))), easing="linear",
                            placement={"height": h0 * g, "anchor": "center",
                                       "offset": [end['offset'][0] * f, off0 * g * (1 - f) + end['offset'][1] * f + band_dy]},
                            camera=cam, light=light(f)))
    # The body stands in a STAGE of its own (rung 33) on a glossy floor
    # (rung 45): the placement, camera and light ride the stage's
    # keyframes; the member stays at the stage's centre. The floor
    # mirrors the body and takes the key light's shadow, and the layers
    # beneath — the table's picture — show through it.
    member = layer(name.capitalize(), 0, "model", 0, total, resourceID=body["id"],
                   keyframes=[kf(0, depth=0, stageOffset=[0, 0])])
    layers.append(layer(f"{name.capitalize()} on the table", 3 + up, "stage", 0, total, floor=FLOOR,
                        members=[member], keyframes=body_keys))
    comp = res("composition", "", f"Scene {name}", duration=total, pixelWidth=W, pixelHeight=ch,
               composition={"canvasWidth": W, "canvasHeight": ch, "backgroundColorHex": "0B0D12", "layers": layers})
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
    rate, distance, typing = film.get('rate', RATE), film.get('distance', 1.0), film.get('typing')
    # `distance` < 1 starts every body that much smaller — farther away;
    # the typing scene's body is the lettered one
    devices = [d[:1] + ((typing['glb'],) if typing and d[0] == typing['scene'] else d[1:2]) + d[2:4]
               + (round(d[4] * distance),) + d[5:] for d in (DEVICE[n] for n in order)]
    prologues = [typing_prologue(typing) if typing and n == typing['scene'] else 0.0 for n in order]
    cube = json.load(open(CUBE_REF))
    settings = dict(cube['compositionSettings'])
    settings.update({"canvasWidth": W, "canvasHeight": H,
                     "environment": {"preset": "studio", "intensity": 1.1}})
    resources = list(cube['resources'])
    floor = res("image", "floor.png", "Floor", pixelWidth=4096, pixelHeight=900); resources.append(floor)
    shadow = None   # the floor's own shadow and contact (rung 45) replaced the painted blob
    table = None
    if film.get('screen_shaped'):
        table = res("image", "table.png", "Table front", pixelWidth=4096, pixelHeight=64); resources.append(table)
    # Each scene's canvas: the first is seen only at the top level; each
    # later one plays on the previous device's screen, and takes that
    # screen's shape when the film is screen-shaped. The cube piece stays
    # its own 1440×900 (a laptop's screen is within 5% of that).
    canvases = [(H, 0.5)] + [screen_canvas(SCREEN_ASPECT[order[i - 1]]) if film.get('screen_shaped') else (H, 0.5)
                             for i in (1, 2)] + [(H, 0.5)]
    # the flight's timing first, so every composition knows the film's length
    durs = [flight(h0, end_placement(n)['height'], rate) + pro for (n, _, _, _, h0, _, _), pro in zip(devices, prologues)]
    starts = [sum(durs[:i]) for i in range(3)]
    t3 = sum(durs); total = t3 + cube['videoDuration'] - CUBE_SKIP
    comps = [None] * 4
    comps[3] = cube_scene(cube, t3 - CUBE_SKIP, total)
    for i in (2, 1, 0):
        name, glb, radius, pose0, h0, off0, bg = devices[i]
        comp, extra, _ = scene(name, glb, radius, pose0, h0, off0, bg, floor, shadow, comps[i + 1], starts[i], total,
                               canvas=canvases[i], next_band_v=canvases[i + 1][1], table=table, rate=rate,
                               typing=(typing if typing and name == typing['scene'] else None))
        comps[i] = comp; resources += extra
    resources += comps
    layers = []
    ends = starts[1:] + [t3]
    for i, comp in enumerate(comps):
        dur = total if i == 3 else ends[i] + FADE
        ch, band_v = canvases[i]
        # a screen-shaped scene at its own scale, its band over the canvas
        shown = ({"mode": "fill", "anchor": "center"} if ch == H else
                 {"height": ch, "anchor": "center", "offset": [0, -int(round((band_v - 0.5) * ch))]})
        layers.append(layer(comp["displayName"], 3 - i, "video", 0, dur, resourceID=comp["id"],
                            **({"transitionOut": {"kind": "fade", "duration": FADE}} if i < 3 else {}),
                            keyframes=[kf(0, placement=shown)]))
    # One headline per device scene, at the top, over the flight's first
    # part: in after the cut has settled, out well before the next one,
    # so no two captions ever share a frame — and the cube piece keeps
    # its own title.
    for i, text in enumerate(captions or ()):
        if typing and order[i] == typing['scene']:
            # The typed word, a typewriter in step with the keys: one
            # character per beat from the first key, held through the way
            # back and the pause, gone as the flight begins.
            start = starts[i] + typing['move']; end = starts[i] + prologues[i] + 0.3
            style = dict(CAPTION_STYLE, fontSize=64,
                         reveal={"by": "character", "mode": "wipe", "secondsPer": typing['beat']})
            layers.append(layer(f"Caption {order[i]}", 10 + i, "caption", start, end - start,
                                captionText=typing['word'], fadeOut=0.35, captionStyle=style))
            continue
        start = starts[i] + (0.3 if i == 0 else FADE + 0.15); end = starts[i] + 0.62 * durs[i]
        layers.append(layer(f"Caption {order[i]}", 10 + i, "caption", start, end - start, captionText=text,
                            fadeIn=0.35, fadeOut=0.35, captionStyle=dict(CAPTION_STYLE)))
    return {"id": U(), "name": film['name'], "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": total, "videoDuration": total, "subtitles": [],
            "minReaderVersion": 45, "compositionSettings": settings, "resources": resources, "layers": layers,
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
