"""The creative set: a goal, the material and the tools — nothing about
how. Each entry becomes `demos/cN-slug/` with the media, a prompt that
states the goal and the constraints a client would state, and a brief the
scorer reads instead of a rubric: valid, rendered, in the asked length,
uses what it was given, and how much of the vocabulary it reached for on
its own. The piece itself is the real answer; the page shows it beside
the agent's own notes.

    python3 demos/creative.py        # (re)writes the folders; runs/ stay
"""
import json, os, shutil
HERE = os.path.dirname(os.path.abspath(__file__))
TASK = open(os.path.join(HERE, 'task.md')).read()

SETS = [
 {"id": "c1", "slug": "launch-video", "title": "Launch video",
  "goal": "Lumen is an analytics app for small teams. Here are five screenshots — three of Lumen, and one each of two sibling apps from the same suite, Pulse and Verse, which you may use or leave out — and a short clip of the Lumen window drifting over a green screen. Make a launch video, 20 to 30 seconds, landscape, that makes someone want to try it. Everything else is your call: pacing, words, motion, looks.",
  "media": {"01-app-store-hero": ["ui_lumen_1.png", "ui_lumen_2.png", "ui_lumen_5.png", "ui_pulse_1.png", "ui_verse_1.png"],
            "19-green-room": ["green_lumen.mp4"]},
  "duration": [20, 30], "must_use": ["ui_lumen_1.png", "green_lumen.mp4"]},
 {"id": "c2", "slug": "social-teaser", "title": "Social teaser",
  "goal": "This is a screen recording and one screenshot from the same app. Make a 15-second vertical teaser for social media — the kind that stops a thumb. Loud is fine. Your choice of words, cuts and effects.",
  "media": {"07-narrated-tour": ["bbb_test.mp4"], "01-app-store-hero": ["ui_lumen_1.png"]},
  "duration": [12, 18], "must_use": ["bbb_test.mp4"]},
 {"id": "c3", "slug": "mood-piece", "title": "Mood piece",
  "goal": "One screenshot and three colour looks in .cube files. Make a 12-second piece about focus and calm — something a person would happily watch loop on a landing page. No brief beyond that.",
  "media": {"20-look-book": ["ui_lumen_1.png", "look_warm.cube", "look_cool.cube", "look_mono.cube"]},
  "duration": [10, 14], "must_use": ["ui_lumen_1.png"]},
 {"id": "c4", "slug": "manifesto", "title": "Manifesto",
  "goal": "No media. Three lines: 'Ship the demo, not the deck.' 'One document, every screen.' 'Say it once.' Make a striking 10-to-15-second typographic piece from them, any canvas you like.",
  "media": {}, "duration": [9, 16], "must_use": []},
 {"id": "c5", "slug": "story-with-voice", "title": "Story with a voice",
  "goal": "Screenshots, a green-screen clip and a warm look. Write and synthesize a short narration of your own (three or four sentences about an analytics app called Lumen), and build a 25-to-35-second story around it with chapters a player can jump to. Level the voice so it sits well.",
  "media": {"01-app-store-hero": ["ui_lumen_1.png", "ui_lumen_2.png", "ui_lumen_5.png"], "19-green-room": ["green_lumen.mp4"], "20-look-book": ["look_warm.cube"]},
  "duration": [24, 36], "must_use": ["green_lumen.mp4"]},
 {"id": "c6", "slug": "title-card", "title": "Title card",
  "goal": "No media. One title: 'LUMEN 2.0' with the line 'See everything.' under it. Make an 8-to-12-second title card, landscape, the kind a motion designer opens a keynote with — solid, dimensional type that moves like it means it. Palette, motion and finish are yours.",
  "media": {}, "duration": [7, 13], "must_use": []},
 {"id": "c7", "slug": "product-spin", "title": "Product spin",
  "goal": "A 3D model of a tablet (slab.glb — look at it before you place it) and three screenshots of Lumen, an analytics app. Make a 10-to-15-second product spot, landscape, where the device is the hero: show the app on its screen, move the camera like a product film would, and say one thing worth saying. Palette, type and motion are yours.",
  "media": {"25-turntable": ["slab.glb"], "01-app-store-hero": ["ui_lumen_1.png", "ui_lumen_2.png", "ui_lumen_5.png"]},
  "duration": [9, 16], "must_use": ["slab.glb"]},
]

def main():
    for s in SETS:
        folder = os.path.join(HERE, f"{s['id']}-{s['slug']}")
        shutil.rmtree(os.path.join(folder, 'resources'), ignore_errors=True)
        os.makedirs(os.path.join(folder, 'resources'))
        media = []
        for src_demo, files in s['media'].items():
            for f in files:
                src = os.path.join(HERE, src_demo, 'resources', f)
                if not os.path.exists(src):
                    src = os.path.join(HERE, '_media', f)
                if os.path.getsize(src) > 1_000_000:
                    pass  # shared: the runner takes it from _media by name
                else:
                    shutil.copy2(src, os.path.join(folder, 'resources', f))
                media.append(f)
        prompt = s['goal'].strip() + "\n"
        if media:
            prompt += "\nFiles in `resources/`: " + ", ".join(media) + ".\n"
        prompt += TASK
        open(os.path.join(folder, 'prompt.md'), 'w').write(prompt)
        json.dump({"kind": "creative", "template": f"{s['id'].upper()} {s['title']}", "media": media,
                   "duration": s['duration'], "must_use": s['must_use']},
                  open(os.path.join(folder, 'rubric.json'), 'w'), indent=1)
        print(f"  {os.path.basename(folder)}: {len(media)} files")

if __name__ == '__main__':
    main()
