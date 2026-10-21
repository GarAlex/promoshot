"""What a project IS, structurally: the layer kinds it draws, the features
it uses, the words that reach the screen. Shared by the importer (to
derive a rubric from a reference), the scorer (to read a result) and the
publisher. Keep the three reading the same file the same way."""
import re

TIME_FIELDS = ('startTime', 'duration', 'fadeIn', 'fadeOut')
MUSICAL = ('beats', 'bars', 'beat', 'bar', 'snap')

def seconds(v, default=0.0):
    """A time field as seconds: a number is one; a musical value
    ({"beats": 2}, {"bar": 5}) is resolved only by promo, so it counts
    as `default` here."""
    return v if isinstance(v, (int, float)) and not isinstance(v, bool) else default

def is_musical(v):
    return isinstance(v, dict) and any(k in v for k in MUSICAL)

def all_layers(meta):
    """The project's layers, stage members and every composition's own."""
    out = []
    def walk(layers):
        for l in layers or []:
            out.append(l)
            walk(l.get('members'))
    walk(meta.get('layers'))
    for r in meta.get('resources', []):
        if r.get('kind') == 'composition':
            walk((r.get('composition') or {}).get('layers'))
    return out

def beat_features(meta):
    """Cutting to music (rung 49): whose beats count, whether the grid is
    measured or typed in, musical times, back-to-back layouts and the
    three beat generators, and where a resource came from."""
    res = meta.get('resources', []); cs = meta.get('compositionSettings', {})
    layers = all_layers(meta)
    comps = [cs] + [(r.get('composition') or {}) for r in res if r.get('kind') == 'composition']
    clocks = [c['beatClock'] for c in comps if isinstance(c.get('beatClock'), dict)] \
        + [l['beatClock'] for l in layers if isinstance(l.get('beatClock'), dict)]
    by_id = {r.get('id'): r for r in res}
    times = [l.get(f) for l in layers for f in TIME_FIELDS] \
        + [(l.get(t) or {}).get('duration') for l in layers for t in ('transitionIn', 'transitionOut')] \
        + [k.get(f) for l in layers for k in l.get('keyframes') or [] for f in ('time', 'transitionDuration')]
    sequences = [c.get('sequence') for c in comps if isinstance(c.get('sequence'), list)]
    entries = [e for q in sequences for e in q if isinstance(e, dict)]
    return {
        "beatClock": bool(clocks),
        # the song is measured (no grid typed in), so another song re-times the edit
        "measuredClock": bool(clocks) and all(not (by_id.get(c.get('resourceID')) or {}).get('beatGrid') for c in clocks),
        "beatGrid": any(r.get('beatGrid') for r in res),
        "musicalTime": any(is_musical(v) for v in times)
                       or any('beats' in e or 'bars' in e or is_musical(e.get('duration')) for e in entries),
        "sequence": bool(entries),
        # cut points stated on the grid, however built: sequence entries, or
        # swap keyframes at musical times on one layer
        "beatCuts": len(entries) + sum(1 for l in layers for k in l.get('keyframes') or []
                                       if k.get('resourceID') and is_musical(k.get('time'))),
        "after": any(l.get('after') for l in layers),
        "pulse": any(l.get('pulse') for l in layers),
        "shake": any(l.get('shake') for l in layers),
        "flash": any(c.get('flash') for c in comps),
        "provenance": any(r.get('provenance') for r in res),
    }

def kinds(meta):
    c = {}
    for l in meta.get('layers', []):
        c[l['kind']] = c.get(l['kind'], 0) + 1
    return c

def features(meta):
    layers = meta.get('layers', []); res = meta.get('resources', [])
    cs = meta.get('compositionSettings', {})
    kf = [k for l in layers for k in l.get('keyframes', [])]
    return {
        "chromaKey": any(l.get('chromaKey') for l in layers),
        "lut": any(r.get('kind') == 'lut' for r in res),
        "chapters": sum(1 for m in meta.get('markers', []) if m.get('kind') == 'chapter'),
        "markers": len(meta.get('markers', [])),
        "audioEffects": any(r.get('audioEffects') for r in res),
        "composition": any(r.get('kind') == 'composition' for r in res),
        "effects": any(l.get('effects') for l in layers)
                   or any(k.get(x) is not None for k in kf for x in ('blur', 'glow', 'vignette')),
        "mask": any(l.get('maskResourceID') for l in layers),
        "maskInverted": any(l.get('maskInverted') for l in layers),
        "viewport": any(k.get('viewport') for k in kf),
        "motionPath": any(k.get('motionPath') for k in kf) or any(r.get('kind') == 'path' for r in res),
        "sprite": any(r.get('sprite') for r in res),
        "gradient": bool(cs.get('backgroundGradient')) or any(k.get('gradient') for k in kf),
        "reveal": any((r.get('captionStyle') or {}).get('reveal') for r in res)
                  or any((l.get('captionStyle') or {}).get('reveal') for l in layers)
                  or bool(cs.get('subtitleReveal')),
        "stage": any(l.get('stage') for l in layers) or any(l.get('kind') == 'stage' for l in layers),
        "stageLayer": any(l.get('kind') == 'stage' for l in layers),
        "model": any(l.get('kind') == 'model' for l in layers)
                 or any(m.get('kind') == 'model' for l in layers for m in (l.get('members') or [])),
        "camera": any(k.get('camera') or k.get('light') for k in kf),
        "materials": any(r.get('materials') for r in res),
        "finish": any(isinstance(b, dict) and (b.get('metallic') is not None or b.get('roughness') is not None)
                      for r in res for b in (r.get('materials') or {}).values()),
        "finishWord": any(isinstance(b, dict) and isinstance(b.get('finish'), str)
                          for r in res for b in (r.get('materials') or {}).values()),
        "floor": any(l.get('floor') for l in layers),
        "device": any(isinstance(r.get('recipe'), dict) and 'device' in r['recipe'] for r in res),
        "light": any(k.get('light') for k in kf),
        "morph": any(isinstance(r.get('particles'), dict) and r['particles'].get('morph') for r in res),
        "route": any(isinstance(r.get('route'), dict) for r in res),
        "cameraRoute": any(isinstance(k.get('camera'), dict) and k['camera'].get('motionPath')
                           for l in layers for k in l.get('keyframes') or []),
        "cameraTarget": any(isinstance(k.get('camera'), dict) and k['camera'].get('target') is not None
                            for l in layers for k in l.get('keyframes') or []),
        "memberRoute": any(k.get('motionPath') for l in layers for m in l.get('members') or [] for k in m.get('keyframes') or []),
        "facedBox": any(isinstance(r.get('recipe'), dict) and any(
                            isinstance(p.get('shape'), dict) and (p['shape'].get('box') or {}).get('faces')
                            for p in r['recipe'].get('parts') or []) for r in res),
        "wornPicture": any(isinstance(b, dict) and b.get('mode') == 'surface' and b.get('resourceID')
                           for r in res for b in (r.get('materials') or {}).values()),
        "wornVideo": any(isinstance(b, dict) and b.get('mode') == 'surface'
                         and next((x for x in res if x.get('id') == b.get('resourceID')), {}).get('kind') == 'video'
                         for r in res for b in (r.get('materials') or {}).values()),
        "textBody": any(isinstance(r.get('recipe'), dict) and 'text' in r['recipe'] for r in res),
        "environment": bool(cs.get('environment')),
        "particles": any(r.get('kind') == 'particles' for r in res),
        "partsBody": any(isinstance(r.get('recipe'), dict) and 'parts' in r['recipe'] for r in res),
        "depth": any((l.get('captionStyle') or {}).get('depth') for l in layers)
                 or any((r.get('captionStyle') or {}).get('depth') for r in res),
        "captionTilt": any(l.get('kind') == 'caption' and any(k.get('tiltX') is not None or k.get('tiltY') is not None for k in l.get('keyframes') or []) for l in layers),
        "kineticReveal": any(((l.get('captionStyle') or {}).get('reveal') or {}).get('mode') in ('flip', 'tumble', 'slide') for l in layers)
                 or any(((r.get('captionStyle') or {}).get('reveal') or {}).get('mode') in ('flip', 'tumble', 'slide') for r in res)
                 or (cs.get('subtitleReveal') or {}).get('mode') in ('flip', 'tumble', 'slide'),
        "blendModes": sorted({l['blendMode'] for l in layers if l.get('blendMode')}),
        "swaps": sum(1 for k in kf if k.get('resourceID')),
        "transitions": any(l.get('transitionIn') or l.get('transitionOut') for l in layers)
                       or any(k.get('transition') for k in kf),
        "timingAnchors": any(l.get('timing') for l in layers),
        "mediaCuts": any(r.get('mediaCuts') for r in res),
        "narration": any(r.get('speech') for r in res) or any(r.get('kind') == 'audio' for r in res),
        "motionBlur": any(l.get('motionBlur') for l in layers) or any(k.get('shutter') is not None for k in kf),
        "grade": any(l.get('adjustments') for l in layers) or any(k.get('saturation') is not None for k in kf),
        "placement": any(k.get('placement') for k in kf),
        **beat_features(meta),
    }

def phrases(meta):
    """Every caption text, wherever it lives — a resource, a layer of the
    older shape, a composition's own layers — in play order."""
    out, seen = [], set()
    by_id = {r['id']: r for r in meta.get('resources', [])}
    layers = list(meta.get('layers', []))
    for r in meta.get('resources', []):
        if r.get('kind') == 'composition':
            layers += (r.get('composition') or {}).get('layers', [])
    for l in sorted(layers, key=lambda l: (seconds(l.get('startTime', 0)), l.get('sortIndex', 0))):
        texts = []
        if l.get('captionText'):
            texts.append(l['captionText'])
        r = by_id.get(l.get('resourceID'))
        if r and r.get('captionText'):
            texts.append(r['captionText'])
        for k in l.get('keyframes', []):
            r2 = by_id.get(k.get('resourceID'))
            if r2 and r2.get('captionText'):
                texts.append(r2['captionText'])
        for t in texts:
            t = t.strip()
            if t and t not in seen:
                seen.add(t); out.append(t)
    return out

def words(text):
    return re.sub(r'[^a-z0-9 ]+', ' ', text.lower()).split()

def duration_of(meta):
    """The piece's length: videoDuration, else the latest layer end in seconds."""
    return meta.get('videoDuration') or max(
        (seconds(l.get('startTime', 0)) + seconds(l.get('duration') or 0) for l in meta.get('layers', [])), default=0)

def rubric_from(meta, name, media):
    cs = meta['compositionSettings']
    rubric = {
        "template": name,
        "canvas": [cs.get('canvasWidth'), cs.get('canvasHeight')],
        "duration": duration_of(meta),
        "kinds": kinds(meta),
        "features": features(meta),
        "phrases": phrases(meta),
        "media": media,
    }
    clock = cs.get('beatClock')
    if isinstance(clock, dict):
        song = next((r.get('filename') for r in meta.get('resources', []) if r.get('id') == clock.get('resourceID')), None)
        if song:
            # the agent's own video, its cuts found in the picture against the song's measured beats
            rubric["onBeat"] = {"song": song, "minCuts": 8, "share": 0.9}
    return rubric
