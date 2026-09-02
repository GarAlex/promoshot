"""Import demos from a template library: for every `NN Name.promo` whose
number has a prompt in PROMPTS, write `demos/NN-slug/` with the media,
the prompt (file list and caption lines appended), a rubric derived from
the template, and the template's metadata as `reference.json`. Files over
the shared threshold go to `demos/_media/` once and are named in the
rubric's `media` list; the runner reassembles them.

    python3 import_templates.py "~/Desktop/PromoShot Templates"
"""
import json, os, re, shutil, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from features import rubric_from  # noqa: E402
HERE = os.path.dirname(os.path.abspath(__file__))
SHARED_OVER = 1_000_000

PROMPTS = json.load(open(os.path.join(HERE, 'prompts.json')))
TASK = open(os.path.join(HERE, 'task.md')).read()

def slug(name):
    return re.sub(r'[^a-z0-9]+', '-', name.lower()).strip('-')

def main(lib):
    lib = os.path.expanduser(lib)
    os.makedirs(os.path.join(HERE, '_media'), exist_ok=True)
    for d in sorted(os.listdir(lib)):
        if not d.endswith('.promo'):
            continue
        nn = d[:2]
        if nn not in PROMPTS:
            continue
        meta = json.load(open(os.path.join(lib, d, 'metadata.json')))
        name = d[:-6]
        folder = os.path.join(HERE, f"{nn}-{slug(name[3:])}")
        # Refresh the inputs; runs/ is history and stays.
        shutil.rmtree(os.path.join(folder, 'resources'), ignore_errors=True)
        os.makedirs(os.path.join(folder, 'resources'))
        media = []
        for f in sorted(os.listdir(os.path.join(lib, d, 'Resources'))):
            # Narration audio is a RESULT of the prompt's lines: the agent
            # synthesizes it, so it stays out.
            if f.startswith('narration-') or f.startswith('speech-'):
                continue
            src = os.path.join(lib, d, 'Resources', f)
            if os.path.getsize(src) > SHARED_OVER:
                dst = os.path.join(HERE, '_media', f)
                if not os.path.exists(dst):
                    shutil.copy2(src, dst)
            else:
                shutil.copy2(src, os.path.join(folder, 'resources', f))
            media.append(f)
        json.dump(meta, open(os.path.join(folder, 'reference.json'), 'w'), indent=1)
        rubric = rubric_from(meta, name, media)
        prompt = PROMPTS[nn].strip() + "\n"
        if media:
            prompt += "\nFiles in `resources/`: " + ", ".join(media) + ".\n"
        if rubric['phrases']:
            prompt += "\nText to use, in order:\n" + "".join(f"- {t}\n" for t in rubric['phrases'])
        prompt += TASK
        open(os.path.join(folder, 'prompt.md'), 'w').write(prompt)
        json.dump(rubric, open(os.path.join(folder, 'rubric.json'), 'w'), indent=1)
        print(f"  {os.path.basename(folder)}: {len(media)} files, {len(rubric['phrases'])} lines")

if __name__ == '__main__':
    main(sys.argv[1] if len(sys.argv) > 1 else '~/Desktop/PromoShot Templates')
