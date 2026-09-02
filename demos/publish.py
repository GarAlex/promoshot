"""Publish the demos: `demo.md` at the repository root and `docs/demo/`
(assets plus `demo.json` for the website), from each demo's latest scored
run — the resources, the prompt as typed, and what the fresh agent made,
beside the hand-built reference at the same moments.

    python3 demos/publish.py
"""
import json, os, shutil, subprocess
HERE = os.path.dirname(os.path.abspath(__file__))
CORE = os.path.dirname(HERE)
ASSETS = os.path.join(CORE, 'docs', 'demo')
PROMO = os.path.join(CORE, 'target', 'release', 'promo')
FFMPEG = '/opt/homebrew/bin/ffmpeg'
IMAGE = ('.png', '.jpg', '.jpeg', '.webp', '.gif')
VIDEO = ('.mp4', '.mov', '.webm')

def latest_run(demo):
    runs = os.path.join(demo, 'runs')
    if not os.path.isdir(runs):
        return None
    for r in sorted(os.listdir(runs), reverse=True):
        if os.path.exists(os.path.join(runs, r, 'score.json')):
            return os.path.join(runs, r)
    return None

def poster(video, out):
    subprocess.run([FFMPEG, '-v', 'error', '-y', '-ss', '1', '-i', video, '-frames:v', '1',
                    '-vf', 'scale=480:-2', out], check=False)

def gif(project, out):
    env = {**os.environ, 'PATH': '/opt/homebrew/bin:' + os.environ.get('PATH', '')}
    subprocess.run([PROMO, 'gif', project, '--out', out, '--fps', '8', '--size', '360x225'], check=False, env=env)
    if os.path.exists(out) and os.path.getsize(out) < 3_000_000:
        return True
    if os.path.exists(out):
        os.remove(out)
    return False

def main():
    os.makedirs(ASSETS, exist_ok=True)
    manifest, sections = [], []
    for name in sorted(os.listdir(HERE)):
        demo = os.path.join(HERE, name)
        if not (os.path.isdir(demo) and name[:2].isdigit() and os.path.exists(os.path.join(demo, 'rubric.json'))):
            continue
        rubric = json.load(open(os.path.join(demo, 'rubric.json')))
        prompt = open(os.path.join(demo, 'prompt.md')).read().strip()
        user_prompt = prompt.split('\nUse the PromoShot skill')[0].strip()
        out = os.path.join(ASSETS, name)
        shutil.rmtree(out, ignore_errors=True)
        os.makedirs(os.path.join(out, 'resources'))
        resources = []
        for f in rubric['media']:
            src = os.path.join(demo, 'resources', f)
            if not os.path.exists(src):
                src = os.path.join(HERE, '_media', f)
            ext = os.path.splitext(f)[1].lower()
            entry = {"file": f, "kind": "image" if ext in IMAGE else "video" if ext in VIDEO else "file"}
            if os.path.exists(src):
                if ext in IMAGE or ext == '.cube':
                    shutil.copy2(src, os.path.join(out, 'resources', f))
                    entry["path"] = f"docs/demo/{name}/resources/{f}"
                else:
                    entry["path"] = f"demos/{'_media' if not os.path.exists(os.path.join(demo, 'resources', f)) else name + '/resources'}/{f}"
                if ext in VIDEO:
                    p = os.path.join(out, 'resources', os.path.splitext(f)[0] + '.poster.png')
                    poster(src, p)
                    if os.path.exists(p):
                        entry["poster"] = f"docs/demo/{name}/resources/{os.path.basename(p)}"
            resources.append(entry)
        open(os.path.join(out, 'prompt.md'), 'w').write(user_prompt + "\n")
        run = latest_run(demo)
        result = None
        if run:
            score = json.load(open(os.path.join(run, 'score.json')))
            summary_path = os.path.join(run, 'summary.txt')
            summary = open(summary_path).read().strip() if os.path.exists(summary_path) else ''
            result = {"score": score['score'], "passed": score['passed'], "total": score['total'],
                      "checks": score['checks'], "summary": summary.split(' result=')[0]}
            for f, key in (('contact-agent.png', 'contact'), ('contact-reference.png', 'reference_contact')):
                src = os.path.join(run, f)
                if os.path.exists(src):
                    dst = os.path.join(out, f.replace('agent', 'result'))
                    shutil.copy2(src, dst)
                    result[key] = f"docs/demo/{name}/{os.path.basename(dst)}"
            # The video, small on purpose: 640 wide, a page's bitrate, so
            # the repository does not carry a screening copy.
            src = os.path.join(run, 'agent.mp4')
            if os.path.exists(src):
                dst = os.path.join(out, 'result.mp4')
                subprocess.run([FFMPEG, '-v', 'error', '-y', '-i', src, '-vf', 'scale=640:-2',
                                '-c:v', 'libx264', '-crf', '30', '-preset', 'slow', '-pix_fmt', 'yuv420p',
                                '-c:a', 'aac', '-b:a', '96k', '-movflags', '+faststart', dst], check=False)
                if os.path.exists(dst):
                    result['video'] = f"docs/demo/{name}/result.mp4"
            project = os.path.join(run, 'ws', 'out.promo')
            if os.path.exists(os.path.join(project, 'metadata.json')):
                shutil.copy2(os.path.join(project, 'metadata.json'), os.path.join(out, 'result-metadata.json'))
                result["metadata"] = f"docs/demo/{name}/result-metadata.json"
                if gif(project, os.path.join(out, 'result.gif')):
                    result["gif"] = f"docs/demo/{name}/result.gif"
        manifest.append({"id": name[:2], "slug": name, "title": rubric['template'][3:],
                         "canvas": rubric['canvas'], "duration": rubric['duration'],
                         "prompt": user_prompt, "resources": resources, "result": result})
        s = [f"## {rubric['template']}", "",
             f"*{rubric['canvas'][0]}×{rubric['canvas'][1]}, {rubric['duration']:.0f} s.*", "",
             "**Resources given to the agent**", ""]
        if not resources:
            s.append("*None — the prompt carries the text.*")
        for r in resources:
            if r['kind'] == 'image' and 'path' in r:
                s.append(f'<img src="{r["path"]}" width="220" alt="{r["file"]}"> ')
            elif r['kind'] == 'video':
                if 'poster' in r:
                    s.append(f'<img src="{r["poster"]}" width="220" alt="{r["file"]}"> ')
                s.append(f"`{r['file']}`" + (f" ([file]({r['path']}))" if 'path' in r else ""))
            else:
                s.append(f"`{r['file']}`" + (f" ([file]({r['path']}))" if 'path' in r else ""))
        s += ["", "**The prompt**", "", "> " + user_prompt.replace("\n", "\n> "), ""]
        if result:
            s.append(f"**What the agent made** — score {result['score']}% ({result['passed']}/{result['total']} rubric checks)"
                     + (f", {result['summary']}" if result['summary'] else ""))
            s.append("")
            if 'gif' in result:
                s += [f'<img src="{result["gif"]}" width="480" alt="result">', ""]
            if 'contact' in result:
                s += [f'<img src="{result["contact"]}" width="720" alt="six moments of the result">', ""]
            links = [f"[video]({result['video']})"] if 'video' in result else []
            if 'metadata' in result:
                links.append(f"[the project it wrote]({result['metadata']})")
            if links:
                s += [" · ".join(links), ""]
            missed = [c for c in result['checks'] if not c['ok']]
            if missed:
                s += ["Missed: " + ", ".join(f"{c['check']} ({c['detail']})" for c in missed), ""]
            if 'reference_contact' in result:
                s += ["**The hand-built reference, same moments**", "",
                      f'<img src="{result["reference_contact"]}" width="720" alt="six moments of the reference">', ""]
        else:
            s += ["*Not run yet.*", ""]
        sections.append("\n".join(s))
    head = """# Demos — prompt in, video out

Every piece below was made by a **fresh agent**: a Claude Code session
with nothing but the media shown, the prompt shown, the public
[PromoShot skill](skill/SKILL.md) and the headless PromoShot MCP server
fenced to an empty folder. No repository, no memory, no example to copy.
It wrote the project, validated it, rendered it — and the result is shown
beside the piece a person built by hand for the same prompt, at the same
six moments, with a structural score: canvas, length, the mix of layers,
the features the prompt implied, the words that reach the screen.

The suite lives in [demos/](demos/): the media, the prompts, the rubrics,
the runner and the scorer. Adding a demo is adding a folder; see
[demos/README.md](demos/README.md). `docs/demo/demo.json` carries the
same material for the website. Footage credit: Big Buck Bunny (Blender
Foundation, CC BY 3.0) where a screen recording stands in.

"""
    open(os.path.join(CORE, 'demo.md'), 'w').write(head + "\n\n".join(sections) + "\n")
    json.dump(manifest, open(os.path.join(ASSETS, 'demo.json'), 'w'), indent=1)
    done = sum(1 for m in manifest if m['result'])
    print(f"demo.md: {len(manifest)} demos, {done} with results; assets in docs/demo")

if __name__ == '__main__':
    main()
