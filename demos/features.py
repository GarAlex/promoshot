"""What a project IS, structurally: the layer kinds it draws, the features
it uses, the words that reach the screen. Shared by the importer (to
derive a rubric from a reference), the scorer (to read a result) and the
publisher. Keep the three reading the same file the same way."""
import re

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
    for l in sorted(layers, key=lambda l: (l.get('startTime', 0), l.get('sortIndex', 0))):
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

def rubric_from(meta, name, media):
    cs = meta['compositionSettings']
    return {
        "template": name,
        "canvas": [cs.get('canvasWidth'), cs.get('canvasHeight')],
        "duration": meta.get('videoDuration') or max(
            (l.get('startTime', 0) + (l.get('duration') or 0) for l in meta.get('layers', [])), default=0),
        "kinds": kinds(meta),
        "features": features(meta),
        "phrases": phrases(meta),
        "media": media,
    }
