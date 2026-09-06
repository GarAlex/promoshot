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
W, H = 1824, 1200
SCENE = 8.0; CUBE = 12.0; FADE = 0.35; TAIL = 0.4

# --- measured at placement height 600 on a 1000-square canvas at the pose
# the zoom ends in (Scripts-style measurement, 2026-09-05): the body's box
# and the Screen plate; the fitted 1.52 picture inside the screen.
MEASURED = {
    # The lid leans; face-on is a camera a touch BELOW level. Its body box
    # runs off the 1000-square at this pose, but the box is centred by
    # construction, so its centre is the canvas's.
    'laptop': dict(body=(0, 200, 999, 798), screen=(107, 231, 892, 738), pose=dict(yaw=0, pitch=-6)),
    'tablet': dict(body=(110, 201, 889, 798), screen=(130, 222, 869, 779), pose=dict(yaw=0, pitch=0)),
    'phone':  dict(body=(354, 201, 645, 798), screen=(367, 210, 632, 789), pose=dict(yaw=0, pitch=0)),
}
ASPECT = W / H
def end_placement(name):
    m = MEASURED[name]; bx0, by0, bx1, by1 = m['body']; sx0, sy0, sx1, sy1 = m['screen']
    sw, sh = sx1 - sx0 + 1, sy1 - sy0 + 1
    # the picture fitted inside the screen, centred
    if sw / sh > ASPECT: pw, ph = sh * ASPECT, sh   # bars at the sides, the picture the screen's height
    else: pw, ph = sw, sw / ASPECT                   # bars top and bottom, the picture the screen's width
    pcx, pcy = (sx0 + sx1) / 2, (sy0 + sy1) / 2
    bcx, bcy = (bx0 + bx1) / 2, (by0 + by1) / 2
    s = H / ph
    return dict(height=round(600 * s, 1), offset=[round(-(pcx - bcx) * s, 1), round(-(pcy - bcy) * s, 1)])

def kf(time, **f):
    d = {"id": U(), "time": time, "transitionDuration": f.pop('ramp', 0)}; d.update(f); return d
def res(kind, filename, name, **f):
    d = {"id": U(), "kind": kind, "filename": filename, "displayName": name, "addedAt": 0,
         "imageCuts": [], "disabledAudioTrackIndices": []}; d.update(f); return d
def layer(name, sort, kind, start, dur, **f):
    d = {"id": U(), "name": name, "sortIndex": sort, "kind": kind, "isEnabled": True,
         "startTime": start, "duration": dur, "keyframes": []}; d.update(f); return d

FINISH = {'laptop': ("2B2B2E", 1, 0.40), 'tablet': ("D8DADC", 1, 0.35), 'phone': ("9B978F", 1, 0.45)}
DEVICES = [('laptop', 'DeviceLaptop.glb', 1.095, dict(yaw=-30, pitch=16), 780, 60, 'bg_laptop.png'),
           ('tablet', 'DeviceTablet.glb', 0.864, dict(yaw=-26, pitch=12), 720, 40, 'bg_tablet.png'),
           ('phone',  'DevicePhone.glb',  0.395, dict(yaw=-24, pitch=10), 720, 30, 'bg_phone.png')]

def build(stills, fades=True):
    resources, layers = [], []
    settings = {"canvasWidth": W, "canvasHeight": H, "backgroundColorHex": "0B0D12",
                "palette": [{"name": "canvas", "colorHex": "0B0D12"}, {"name": "edge", "colorHex": "26364F"},
                            {"name": "accent", "colorHex": "FFB050"}, {"name": "text", "colorHex": "FFFFFF"}],
                "environment": {"preset": "studio", "intensity": 1.0},
                "subtitleFontFamily": "system", "subtitleFontSize": 96, "subtitleBold": True,
                "subtitleColorHex": "@text", "subtitleBackgroundOpacity": 0, "subtitleBackgroundPadding": 16,
                "subtitleLeftMargin": 120, "subtitleRightMargin": 120, "subtitleVerticalMargin": 980,
                "videoCornerRadius": 0, "videoBorderWidth": 0, "videoBorderColorHex": "@edge"}
    floor = res("image", "floor.png", "Floor", pixelWidth=4096, pixelHeight=900); resources.append(floor)
    shadow = res("image", "shadow.png", "Shadow", pixelWidth=1600, pixelHeight=500); resources.append(shadow)
    sort = 0
    for i, (name, glb, radius, start_pose, h0, off0, bg) in enumerate(DEVICES):
        t0 = i * SCENE; scene_end = t0 + SCENE; extra = TAIL  # every scene runs a little under the next's fade
        fade = FADE if (i > 0 and fades) else None
        bgres = res("image", bg, f"Background {i+1}", pixelWidth=4096, pixelHeight=2700); resources.append(bgres)
        still = res("image", stills[i], f"Screen {i+1}", pixelWidth=W, pixelHeight=H); resources.append(still)
        hexv, met, rough = FINISH[name]
        body = res("model", glb, name.capitalize(), boundsRadius=radius, clips=[],
                   materials={"Screen": {"resourceID": still["id"]},
                              "Body": {"colorHex": hexv, "metallic": met, "roughness": rough},
                              "Deck": {"colorHex": hexv, "metallic": met, "roughness": rough}})
        resources.append(body)
        dur = SCENE + extra
        # The scrolling background: taller than the canvas, panned across the slack.
        layers.append(layer(f"Background {i+1}", sort, "image", t0, dur, resourceID=bgres["id"],
            **({"fadeIn": fade} if fade else {}),
            # Still through the cut's fade — a scroll under a crossfade
            # ghosts — then the slow pan for the rest of the scene.
            keyframes=[kf(0, placement={"height": 1500, "anchor": "center", "offset": [-220, 0]}),
                       kf(FADE, placement={"height": 1500, "anchor": "center", "offset": [-220, 0]}),
                       kf(dur, ramp=dur - FADE, placement={"height": 1500, "anchor": "center", "offset": [220, 0]})])); sort += 1
        layers.append(layer(f"Floor {i+1}", sort, "image", t0, dur, resourceID=floor["id"],
            **({"fadeIn": fade} if fade else {}),
            keyframes=[kf(0, placement={"width": 2200, "anchor": "bottom"})])); sort += 1
        layers.append(layer(f"Shadow {i+1}", sort, "image", t0, dur, resourceID=shadow["id"],
            **({"fadeIn": fade} if fade else {}),
            keyframes=[kf(0, placement={"height": 150, "anchor": "center", "offset": [0, off0 + h0 / 2 - 20]}, opacity=0.9),
                       kf(3.0, placement={"height": 150, "anchor": "center", "offset": [0, off0 + h0 / 2 - 20]}, opacity=0.9),
                       kf(4.8, ramp=1.8, placement={"height": 150, "anchor": "center", "offset": [0, off0 + h0 / 2 - 20]}, opacity=0.0)])); sort += 1
        end = end_placement(name); pose = MEASURED[name]['pose']
        cam0 = dict(yaw=start_pose['yaw'], pitch=start_pose['pitch'], roll=0, distance=4.2, fov=30)
        cam1 = dict(yaw=pose['yaw'], pitch=pose['pitch'], roll=0, distance=4.2, fov=30)
        light = dict(yaw=15, pitch=50, intensity=1.0)
        layers.append(layer(name.capitalize(), sort, "model", t0, dur, resourceID=body["id"],
            **({"fadeIn": fade} if fade else {}),
            keyframes=[kf(0, placement={"height": h0, "anchor": "center", "offset": [0, off0]}, camera=cam0, light=light),
                       kf(3.0, placement={"height": h0, "anchor": "center", "offset": [0, off0]}, camera=cam0, light=light),
                       kf(7.6, ramp=4.6, easing="easeInOut",
                          placement={"height": end["height"], "anchor": "center", "offset": end["offset"]},
                          camera=cam1, light=light)])); sort += 1
    # --- the final scene: the cube turning under confetti
    t0 = 3 * SCENE
    faces = [res("image", f"face_{n}.png", f"Face {n}", pixelWidth=512, pixelHeight=512) for n in range(1, 7)]
    resources += faces
    cube = res("model", "", "Cube", recipe={"parts": [{"slot": "Cube", "shape": {"box": {"size": [1.4, 1.4, 1.4], "radius": 0.05, "faces": True}}}]},
               materials={f"Cube/{side}": {"resourceID": faces[n]["id"], "mode": "surface", "metallic": 0.0, "roughness": 0.35}
                          for n, side in enumerate(["front", "right", "back", "left", "top", "bottom"])})
    resources.append(cube)
    confetti = res("particles", "", "Confetti", particles={
        "anchor": [0.5, 0.08], "extent": [0.7, 0], "burst": 260, "rate": 0, "direction": 270, "spread": 45,
        "speed": [0.15, 0.5], "gravity": 0.55, "drag": 0.7, "size": [0.01, 0.022], "shape": "square",
        "colors": ["@accent", "FFFFFF", "FFD27A", "FF6B6B"], "life": [2.5, 4.0], "spin": [-240, 240], "seed": 3})
    resources.append(confetti)
    title = res("caption", "", "Made with PromoShot", captionText="Made with PromoShot",
                captionStyle={"alignment": "center", "fontSize": 110, "isBold": True, "verticalMargin": 960,
                              "shadowOpacity": 0.45, "shadowRadius": 24, "shadowOffset": [0, 8]})
    resources.append(title)
    grad = lambda a, b, c, s, e: {"kind": "linear", "start": s, "end": e,
                                  "stops": [{"colorHex": a, "at": 0}, {"colorHex": b, "at": 0.55}, {"colorHex": c, "at": 1}]}
    cube_fade = {"fadeIn": FADE} if fades else {}
    layers.append(layer("Backdrop", sort, "background", t0, CUBE, **cube_fade, keyframes=[
        kf(0, gradient=grad("1B2440", "0B0D12", "2A1638", [0, 0], [1, 1])),
        kf(5.9, ramp=5.9, easing="easeInOut", gradient=grad("142A4A", "0C0F1A", "35183F", [0.2, 0], [0.9, 1])),
        kf(7.4, ramp=1.5, easing="easeInOut", gradient=grad("3A2A5C", "141826", "5A2A3A", [0.5, 0], [0.6, 1])),
        kf(12, ramp=4.6, easing="easeInOut", gradient=grad("10162A", "090B12", "231436", [1, 0], [0, 1]))])); sort += 1
    cube_cam = lambda yaw: dict(yaw=yaw, pitch=18, roll=0, distance=4.7, fov=28)
    cube_light = dict(yaw=35, pitch=32, intensity=1.4)
    cube_place = {"height": 700, "anchor": "center", "offset": [0, -20]}
    layers.append(layer("Cube", sort, "model", t0, CUBE, resourceID=cube["id"], **cube_fade, keyframes=[
        # Still through the fade — the picture the phone's screen held —
        # then a turn and a half over the rest of the scene.
        kf(0, placement=cube_place, camera=cube_cam(-25), light=cube_light),
        kf(FADE + 0.15, placement=cube_place, camera=cube_cam(-25), light=cube_light),
        kf(12, ramp=12 - FADE - 0.15, easing="linear", placement=cube_place, camera=cube_cam(695), light=cube_light)])); sort += 1
    for burst_at in (1.0, 6.0):
        layers.append(layer("Confetti", sort, "drawing", t0 + burst_at, 5.0, resourceID=confetti["id"],
                            keyframes=[kf(0, zoom=1.0, horizontalShift=0, verticalShift=0)])); sort += 1
    layers.append(layer("Title", sort, "caption", t0 + 8.5, 3.5, resourceID=title["id"],
                        keyframes=[kf(0, opacity=0), kf(0.6, ramp=0.6, opacity=1)])); sort += 1
    total = t0 + CUBE
    return {"id": U(), "name": "Infinite screens", "createdAt": 0, "state": "recorded",
            "trimStart": 0, "trimEnd": total, "videoDuration": total, "subtitles": [],
            "minReaderVersion": 42, "compositionSettings": settings, "resources": resources, "layers": layers}

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
        if not os.path.exists(os.path.join(RES, n)): Image.new('RGB', (W, H), (11, 13, 18)).save(os.path.join(RES, n))
    # Reverse order: the phone shows scene 4, the tablet scene 3, the laptop scene 2.
    # The stills come from a build WITHOUT the cut fades — a scene's true
    # first frame, not the eleventh percent of a fade-in — in reverse order,
    # so the tablet's screen already shows the phone when the laptop's
    # still is taken.
    for scene, name in ((4, 'still_s4.png'), (3, 'still_s3.png'), (2, 'still_s2.png')):
        materialize(build(names, fades=False), pkg)
        still(pkg, (scene - 1) * SCENE + 0.02, os.path.join(RES, name))
        print('rendered', name)
    meta = build(names); materialize(meta, pkg)
    json.dump(meta, open(os.path.join(HERE, 'reference.json'), 'w'), indent=1)
    v = subprocess.run([CLI, 'validate', pkg], capture_output=True, text=True); print(v.stdout.strip()[:600])
    if '--render' in sys.argv:
        subprocess.run([CLI, 'frames', pkg, '--out', os.path.join(work, 'frames'), '--times',
                        '1,5.5,7.5,8.2,13.5,15.5,16.2,21,23.5,24.2,27,34', '--size', '912x600',
                        '--sheet', os.path.join(work, 'sheet.png')], check=False)
        subprocess.run([CLI, 'video', pkg, '--out', os.path.join(work, 'infinite-screens.mp4')], check=False)
