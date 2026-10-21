"""The hand-built reference for demo 36: a beat-cut promo of eight app
screenshots on the demo's own song (make-music.sh, 128 BPM, a lift at
bar 9). Nothing here states a tempo: the song is the beat clock, promo
measures it on open, and every time below is musical.

    python3 demos/36-beat-cut/build.py        # runs/build/reference.promo, then reference.json

  bar 1      the title lands on the downbeat (it zooms in over the half beat before)
  bars 3-8   twelve stills, two beats each, back to back (a `sequence`); each punches on the downbeat
  bar 9      the lift: a white flash, and the cuts halve to one beat, punching harder
  bar 14     the end card, `after` the last still; bar 15's hit flashes it

No `shake`: on a caption it writes margins as shifts (the words jump to the top),
and on a placed image it leaves the picture off its placement (2026-10-20, v0.3.11).
  bar 16     the end, on a downbeat
"""
import json, os, shutil, subprocess, sys
HERE = os.path.dirname(os.path.abspath(__file__))
DEMOS = os.path.dirname(HERE)
CORE = os.path.dirname(DEMOS)
PROMO = os.path.join(CORE, 'target', 'release', 'promo')
SONG = 'lift_song.wav'
STILLS = ['ui_lumen_1.png', 'ui_pulse_1.png', 'ui_verse_1.png', 'ui_lumen_2.png',
          'ui_pulse_2.png', 'ui_verse_2.png', 'ui_lumen_4.png', 'ui_lumen_5.png']
TITLE, END = 'Lumen · Pulse · Verse', 'Three apps. One beat.'
FIRST_HALF, SECOND_HALF = 12, 20      # stills at 2 beats (bars 3-8), then at 1 beat (bars 9-13)

def source(name):
    p = os.path.join(HERE, 'resources', name)
    return p if os.path.exists(p) else os.path.join(DEMOS, '_media', name)

def main():
    out = os.path.join(HERE, 'runs', 'build', 'reference.promo')
    shutil.rmtree(out, ignore_errors=True)
    os.makedirs(os.path.join(out, 'Resources'))
    for f in STILLS + [SONG]:
        shutil.copy2(source(f), os.path.join(out, 'Resources', f))
    # Only for the stored seconds a reader without the clock would see
    # (videoDuration): the file itself states no grid.
    beats = json.loads(subprocess.run([PROMO, 'beats', source(SONG), '--json'],
                                      capture_output=True, text=True, check=True).stdout)
    end = round(beats['offset'] + 60 * 60 / beats['bpm'], 3)          # bar 16 = beat 61

    resources = [{"id": "song", "kind": "audio", "filename": SONG, "displayName": "Lift (128 BPM)",
                  "addedAt": 0, "imageCuts": [], "disabledAudioTrackIndices": [],
                  "provenance": {"tool": "ffmpeg aevalsrc", "license": "CC0-1.0",
                                 "prompt": "128 BPM four-on-the-floor, clap on 2 and 4, a lift at bar 9",
                                 "cost": 0, "currency": "USD",
                                 "url": "https://github.com/garalex/promoshot/blob/main/demos/36-beat-cut/make-music.sh"}}]
    for i, f in enumerate(STILLS):
        resources.append({"id": f"p{i + 1}", "kind": "image", "filename": f,
                          "displayName": f.rsplit('.', 1)[0], "addedAt": 0,
                          "pixelWidth": 1216, "pixelHeight": 760, "imageCuts": [],
                          "disabledAudioTrackIndices": [],
                          "frame": {"kind": "border", "borderColorHex": "@edge", "borderWidth": 3, "cornerRadius": 16}})

    layers = [
        {"id": "bg", "name": "Background", "sortIndex": 0, "kind": "background", "isEnabled": True,
         "startTime": 0, "duration": end, "keyframes": []},
        {"id": "music", "name": "Music", "sortIndex": 1, "kind": "audio", "isEnabled": True,
         "startTime": 0, "duration": {"bar": 16}, "fadeOut": {"beats": 2}, "resourceID": "song", "keyframes": []},
    ]
    sequence = []
    n = FIRST_HALF + SECOND_HALF
    for k in range(n):
        lift = k >= FIRST_HALF
        sid = f"s{k + 1:02d}"
        pulse = ([{"every": "downbeat", "property": "viewport", "amount": 0.09, "decay": {"beats": 0.5}},
                  {"every": "beat", "property": "viewport", "amount": 0.03, "decay": {"beats": 0.25}}]
                 if lift else {"every": "downbeat", "property": "viewport", "amount": 0.05, "decay": {"beats": 0.5}})
        layers.append({"id": sid, "name": f"Still {k + 1}", "sortIndex": 2 + k, "kind": "image", "isEnabled": True,
                       # the first still opens the sequence on bar 3; the rest take their starts from it
                       "startTime": {"bar": 3} if k == 0 else 0, "duration": 1,
                       "resourceID": f"p{k % len(STILLS) + 1}", "pulse": pulse,
                       "keyframes": [{"id": f"{sid}k", "time": 0, "transitionDuration": 0,
                                      "placement": {"height": 800, "anchor": "center", "offset": [0, 0]}}]})
        sequence.append({"layer": sid, "beats": 1 if lift else 2})
    style = {"alignment": "center", "weight": "heavy", "tracking": -1.5, "textColorHex": "@text",
             "backgroundOpacity": 0}
    layers.append({"id": "title", "name": "Title", "sortIndex": 2 + n, "kind": "caption", "isEnabled": True,
                   # half a beat early, so the zoom-in lands ON beat 1
                   "startTime": {"beat": 0.5}, "duration": {"bar": 3},
                   "transitionIn": {"kind": "zoom", "duration": {"beats": 0.5}, "easing": "easeOut"},
                   "captionText": TITLE, "captionStyle": {**style, "fontSize": 132, "verticalMargin": 440},
                   "keyframes": []})
    layers.append({"id": "end", "name": "End card", "sortIndex": 3 + n, "kind": "caption", "isEnabled": True,
                   "startTime": 0, "after": f"s{n:02d}", "duration": {"bar": 16}, "fadeOut": {"beats": 2},
                   "captionText": END, "captionStyle": {**style, "fontSize": 120, "verticalMargin": 452},
                   "keyframes": []})

    meta = {"id": "36-beat-cut", "name": "36 Beat Cut", "createdAt": 0, "state": "recorded",
            "minReaderVersion": 49, "trimStart": 0, "trimEnd": end, "videoDuration": end, "subtitles": [],
            "compositionSettings": {
                "canvasWidth": 1920, "canvasHeight": 1080, "fps": 30,
                "backgroundColorHex": "@canvas",
                "palette": [{"name": "canvas", "colorHex": "0B1020"}, {"name": "edge", "colorHex": "2A3A5C"},
                            {"name": "text", "colorHex": "FFFFFF"}],
                "backgroundGradient": {"kind": "linear", "start": [0, 0], "end": [1, 1],
                                       "stops": [{"colorHex": "0B1020", "at": 0}, {"colorHex": "2B1F5C", "at": 1}]},
                "videoShadowOpacity": 0.55, "videoShadowRadius": 40,
                "beatClock": {"resourceID": "song"},
                "sequence": sequence,
                "flash": [{"on": "downbeat", "range": {"bar": 9, "bars": 1}, "colorHex": "FFFFFF",
                           "opacity": 0.9, "duration": {"beats": 1}},
                          {"on": "downbeat", "range": {"bar": 15, "bars": 1}, "colorHex": "FFFFFF",
                           "opacity": 0.6, "duration": {"beats": 0.5}}]},
            "resources": resources, "layers": layers}
    json.dump(meta, open(os.path.join(out, 'metadata.json'), 'w'), indent=1, ensure_ascii=False)
    r = subprocess.run([PROMO, 'validate', out], capture_output=True, text=True)
    print(r.stdout.strip() or r.stderr.strip())
    if r.returncode != 0:
        sys.exit(r.returncode)
    shutil.copy2(os.path.join(out, 'metadata.json'), os.path.join(HERE, 'reference.json'))
    print(out)

if __name__ == '__main__':
    main()
