"""Score a project against a demo's rubric: how close in SENSE it is to
the reference. Structural, not pixel: canvas, length, the mix of layer
kinds, the features the prompt implied, and the words on screen.

    python3 score.py <demo dir> <project dir> [--json]
"""
import json, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from features import features, kinds, phrases, words  # noqa: E402

# A feature counts only when the PROMPT asked for it, by these words: the
# rubric comes from the reference, which uses whatever its author liked,
# and a fresh agent is judged on the brief, not on the author's taste.
ASKED = {
    "chromaKey": ("green screen", "key out", "keyed", "chroma"),
    "lut": (".cube", "lut", "look"),
    "chapters": ("chapter",),
    "markers": ("marker", "chapter"),
    "audioEffects": ("level", "normalize", "compress", "loudness"),
    "composition": ("built once", "placed three times", "reusable", "composition"),
    "effects": ("blur", "glow", "vignette", "grain", "sharpen", "smear"),
    "mask": ("spotlight", "mask", "window", "visible only inside"),
    "maskInverted": ("punch", "invert", "cut out"),
    "viewport": ("zoom in on", "view inside", "the view", "viewport", "moves to"),
    "motionPath": ("curved path", "path", "flying", "climbs"),
    "sprite": ("sprite",),
    "gradient": ("gradient",),
    "reveal": ("typewriter", "word by word", "word-by-word", "karaoke", "letter by letter", "reveal", "typed"),
    "blendModes": ("screen blend", "multipl", "added on top", "blend"),
    "swaps": ("swap", "replace", "in turn", "push", "crossfade", "deck", "before-and-after", "before and after"),
    "transitions": ("transition", "push", "wipe", "crossfade", "dissolve", "slide"),
    "timingAnchors": ("starts half a second before", "retrimmed", "pinned", "follows it"),
    "mediaCuts": ("speed", "slow", "half-speed", "double-speed"),
    "narration": ("narrat", "voice", "synthesize", "spoken", "speak"),
    "motionBlur": ("motion blur", "whip", "smear", "blurrier"),
    "grade": ("black and white", "sepia", "duotone", "desaturat", "grade", "look", "flat"),
}

def asked_for(key, prompt):
    words = ASKED.get(key)
    if words is None:
        return True
    p = prompt.lower()
    return any(w in p for w in words)

def score(demo, project):
    rubric = json.load(open(os.path.join(demo, 'rubric.json')))
    meta = json.load(open(os.path.join(project, 'metadata.json')))
    prompt_path = os.path.join(demo, 'prompt.md')
    prompt = open(prompt_path).read() if os.path.exists(prompt_path) else ''
    checks = []
    def check(name, ok, detail=""):
        checks.append({"check": name, "ok": bool(ok), "detail": detail})
    cs = meta.get('compositionSettings', {})
    got_canvas = [cs.get('canvasWidth'), cs.get('canvasHeight')]
    want_canvas = rubric['canvas']
    same_aspect = all(got_canvas) and all(want_canvas) and \
        abs(got_canvas[0] / got_canvas[1] - want_canvas[0] / want_canvas[1]) < 0.02
    # The canvas counts when the brief named one ("1440 by 900", "vertical").
    names_size = any(t in prompt.lower() for t in (" by ", "×", "vertical", "portrait", "landscape"))
    if names_size:
        check("canvas", got_canvas == want_canvas or same_aspect, f"{got_canvas} vs {want_canvas}")
    layers = meta.get('layers', [])
    dur = meta.get('videoDuration') or (max(l.get('startTime', 0) + (l.get('duration') or 0) for l in layers) if layers else 0)
    want = rubric['duration'] or 0
    check("duration", want and abs(dur - want) <= max(0.25 * want, 1.0), f"{dur:.1f}s vs {want:.1f}s")
    got_kinds, want_kinds = kinds(meta), rubric['kinds']
    for kind, n in want_kinds.items():
        g = got_kinds.get(kind, 0)
        # Present, and not wildly fewer: more layers than the reference is a
        # different construction, not a missing one.
        check(f"kind:{kind}", g > 0 and g >= (n + 1) // 2, f"{g} vs {n}")
    got_f, want_f = features(meta), rubric['features']
    for key, wanted in want_f.items():
        if key == 'placement':
            continue  # a way of positioning, not something a prompt asks for
        if not asked_for(key, prompt):
            continue  # the reference's taste, not the brief
        if isinstance(wanted, list):
            if wanted:
                check(f"feature:{key}", set(wanted) <= set(got_f.get(key, [])), f"{got_f.get(key)} vs {wanted}")
        elif isinstance(wanted, bool):
            if wanted:
                check(f"feature:{key}", got_f.get(key), f"{got_f.get(key)}")
        elif isinstance(wanted, int):
            if wanted:
                g = got_f.get(key, 0)
                check(f"feature:{key}", g >= max(1, wanted - 1), f"{g} vs {wanted}")
    want_phr = rubric['phrases']
    if want_phr:
        got_words = set(w for t in phrases(meta) for w in words(t))
        hit = 0
        for t in want_phr:
            ws = [w for w in words(t) if len(w) > 3]
            if ws and sum(1 for w in ws if w in got_words) / len(ws) >= 0.4:
                hit += 1
        check("phrases", hit >= max(1, len(want_phr) // 2), f"{hit} of {len(want_phr)} lines recognisable")
    passed = sum(1 for c in checks if c['ok'])
    return {"template": rubric['template'], "score": round(100 * passed / max(1, len(checks))),
            "passed": passed, "total": len(checks), "checks": checks}

if __name__ == '__main__':
    result = score(sys.argv[1], sys.argv[2])
    if '--json' in sys.argv:
        print(json.dumps(result, indent=1))
    else:
        print(f"{result['template']}: {result['score']}% ({result['passed']}/{result['total']})")
        for c in result['checks']:
            print(f"  {'ok ' if c['ok'] else 'MISS'} {c['check']:22s} {c['detail']}")
