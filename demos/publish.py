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
MEDIA = os.path.join(CORE, 'docs', 'demo-media')
MEDIA_URL = 'https://github.com/garalex/promoshot/raw/demo-media'
PROMO = os.path.join(CORE, 'target', 'release', 'promo')
FFMPEG = '/opt/homebrew/bin/ffmpeg'
IMAGE = ('.png', '.jpg', '.jpeg', '.webp', '.gif')
VIDEO = ('.mp4', '.mov', '.webm')

def latest_run(demo):
    runs = os.path.join(demo, 'runs')
    if not os.path.isdir(runs):
        return None
    for r in sorted(os.listdir(runs), reverse=True):
        # Complete only once the agent has answered: a run in flight can
        # already have a scored out.promo.
        agent = os.path.join(runs, r, 'agent.json')
        if os.path.exists(os.path.join(runs, r, 'score.json')) and os.path.exists(agent) and os.path.getsize(agent) > 0:
            return os.path.join(runs, r)
    return None

def poster(video, out):
    subprocess.run([FFMPEG, '-v', 'error', '-y', '-ss', '1', '-i', video, '-frames:v', '1',
                    '-vf', 'scale=480:-2', out], check=False)

def gif(project, out):
    env = {**os.environ, 'PATH': '/opt/homebrew/bin:' + os.environ.get('PATH', '')}
    subprocess.run([PROMO, 'gif', project, '--out', out, '--fps', '6', '--size', '320x200'], check=False, env=env)
    if os.path.exists(out) and os.path.getsize(out) < 6_000_000:
        return True
    if os.path.exists(out):
        os.remove(out)
    return False

def run_stats(run):
    """What the run cost: turns, wall and API time, tokens by model, and
    the MCP's own timing when the server logged it."""
    stats = {}
    try:
        j = json.load(open(os.path.join(run, 'agent.json')))
    except Exception:
        return stats
    stats['turns'] = j.get('num_turns')
    stats['wall_s'] = round((j.get('duration_ms') or 0) / 1000)
    stats['api_s'] = round((j.get('duration_api_ms') or 0) / 1000)
    stats['cost_usd'] = round(j.get('total_cost_usd') or 0, 2)
    u = j.get('usage') or {}
    stats['tokens'] = {
        "input": u.get('input_tokens', 0), "cache_read": u.get('cache_read_input_tokens', 0),
        "cache_write": u.get('cache_creation_input_tokens', 0), "output": u.get('output_tokens', 0),
        "thinking": (u.get('output_tokens_details') or {}).get('thinking_tokens', 0),
    }
    stats['models'] = []
    for model, m in (j.get('modelUsage') or {}).items():
        stats['models'].append({"model": m.get('canonicalModel', model),
                                "input": m.get('inputTokens', 0) + m.get('cacheReadInputTokens', 0) + m.get('cacheCreationInputTokens', 0),
                                "output": m.get('outputTokens', 0), "cost_usd": round(m.get('costUSD', 0), 2)})
    log = os.path.join(run, 'mcp.log')
    if os.path.exists(log):
        calls = []
        for line in open(log):
            parts = line.rstrip('\n').split('\t')
            if len(parts) >= 4:
                try:
                    calls.append((parts[1], int(parts[2]), parts[3]))
                except ValueError:
                    pass
        by_tool = {}
        for tool, ms, ok in calls:
            t = by_tool.setdefault(tool, {"calls": 0, "ms": 0, "errors": 0})
            t["calls"] += 1; t["ms"] += ms; t["errors"] += (ok != 'ok')
        stats['mcp'] = {"calls": len(calls), "ms": sum(c[1] for c in calls),
                        "tools": dict(sorted(by_tool.items(), key=lambda kv: -kv[1]['ms']))}
    return stats

def fmt_tokens(n):
    return f"{n/1_000_000:.1f}M" if n >= 1_000_000 else f"{n/1000:.0f}k" if n >= 1000 else str(n)

def fmt_secs(s):
    return f"{s // 60} min {s % 60:02d} s" if s >= 60 else f"{s} s"

def thumb(video, out):
    subprocess.run([FFMPEG, '-v', 'error', '-y', '-ss', '3', '-i', video, '-frames:v', '1',
                    '-vf', 'scale=320:-2', out], check=False)
    return os.path.exists(out)

BLURBS = json.load(open(os.path.join(HERE, 'blurbs.json'))) if os.path.exists(os.path.join(HERE, 'blurbs.json')) else {}

def main():
    os.makedirs(ASSETS, exist_ok=True)
    manifest, rows, pages = [], [], []
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
                      "checks": score['checks'], "summary": summary.split(' result=')[0],
                      "run": run_stats(run)}
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
                # The real one: 1280 wide, a viewing bitrate, kept off main
                # (docs/demo-media is git-ignored there) and published to the
                # demo-media branch by publish_media.sh.
                os.makedirs(MEDIA, exist_ok=True)
                hd = os.path.join(MEDIA, f"{name}.mp4")
                subprocess.run([FFMPEG, '-v', 'error', '-y', '-i', src, '-vf', 'scale=1280:-2',
                                '-c:v', 'libx264', '-crf', '22', '-preset', 'slow', '-pix_fmt', 'yuv420p',
                                '-c:a', 'aac', '-b:a', '128k', '-movflags', '+faststart', hd], check=False)
                if os.path.exists(hd):
                    result['video_hd'] = f"{MEDIA_URL}/{name}.mp4"
                    result['video_hd_bytes'] = os.path.getsize(hd)
            project = os.path.join(run, 'ws', 'out.promo')
            if os.path.exists(os.path.join(project, 'metadata.json')):
                shutil.copy2(os.path.join(project, 'metadata.json'), os.path.join(out, 'result-metadata.json'))
                result["metadata"] = f"docs/demo/{name}/result-metadata.json"
                if gif(project, os.path.join(out, 'result.gif')):
                    result["gif"] = f"docs/demo/{name}/result.gif"
        if result and 'video' in result:
            t = os.path.join(out, 'thumb.png')
            if thumb(os.path.join(CORE, result['video']), t):
                result['thumb'] = f"docs/demo/{name}/thumb.png"
        blurb = BLURBS.get(name[:2], user_prompt.split('.')[0] + '.')
        manifest.append({"id": name[:2], "slug": name, "title": rubric['template'][3:], "blurb": blurb,
                         "canvas": rubric['canvas'], "duration": rubric['duration'],
                         "prompt": user_prompt, "resources": resources, "result": result,
                         "page": f"docs/demo/demo{name[:2]}.md"})

        # --- the demo's own page (links relative to docs/demo/) ---
        rel = lambda path: path[len('docs/demo/'):] if path.startswith('docs/demo/') else '../../' + path
        pg = [f"# {rubric['template']}", "",
              f"{blurb}", "",
              f"*{rubric['canvas'][0]}×{rubric['canvas'][1]}, {rubric['duration']:.0f} s.* "
              "Part of [the demos](../../demo.md): a fresh agent, the media and the prompt below, "
              "the public skill and the headless MCP, nothing else.", "",
              "## Resources given to the agent", ""]
        if not resources:
            pg.append("*None — the prompt carries the text.*")
        for r in resources:
            if r['kind'] == 'image' and 'path' in r:
                pg.append(f'<img src="{rel(r["path"])}" width="240" alt="{r["file"]}"> ')
            elif r['kind'] == 'video':
                if 'poster' in r:
                    pg.append(f'<img src="{rel(r["poster"])}" width="240" alt="{r["file"]}"> ')
                pg.append(f"`{r['file']}`" + (f" ([file]({rel(r['path'])}))" if 'path' in r else ""))
            else:
                pg.append(f"`{r['file']}`" + (f" ([file]({rel(r['path'])}))" if 'path' in r else ""))
        pg += ["", "## The prompt", "", "> " + user_prompt.replace("\n", "\n> "), ""]
        if result:
            pg += ["## What the agent made", ""]
            pg.append(f"Score **{result['score']}%** ({result['passed']} of {result['total']} rubric checks).")
            pg.append("")
            st = result.get('run') or {}
            if st:
                tk = st.get('tokens', {})
                pg += ["| the agent's work | |", "|---|---|",
                       f"| turns | {st.get('turns')} |",
                       f"| wall time | {fmt_secs(st.get('wall_s', 0))} (API {fmt_secs(st.get('api_s', 0))}) |",
                       f"| cost at API list price | ${st.get('cost_usd', 0):.2f} — on a Claude subscription this is plan usage, not a bill |",
                       f"| tokens in | {fmt_tokens(tk.get('input', 0) + tk.get('cache_read', 0) + tk.get('cache_write', 0))} "
                       f"({fmt_tokens(tk.get('cache_read', 0))} cache read, {fmt_tokens(tk.get('cache_write', 0))} cache write) |",
                       f"| tokens out | {fmt_tokens(tk.get('output', 0))} ({fmt_tokens(tk.get('thinking', 0))} thinking) |"]
                for m in st.get('models', []):
                    pg.append(f"| {m['model']} | {fmt_tokens(m['input'])} in, {fmt_tokens(m['output'])} out, ${m['cost_usd']:.2f} |")
                pg.append("")
                mcp = st.get('mcp')
                if mcp:
                    pg += [f"| MCP tool | calls | time |", "|---|---|---|"]
                    for tool, t in mcp['tools'].items():
                        pg.append(f"| {tool} | {t['calls']}{' (' + str(t['errors']) + ' refused)' if t['errors'] else ''} | {fmt_secs(round(t['ms'] / 1000))} |")
                    pg.append(f"| **all** | {mcp['calls']} | {fmt_secs(round(mcp['ms'] / 1000))} |")
                    pg.append("")
            if 'gif' in result:
                pg += [f'<img src="{rel(result["gif"])}" width="480" alt="the result, looping">', ""]
            if 'contact' in result:
                pg += ["Six moments:", "", f'<img src="{rel(result["contact"])}" width="800" alt="six moments of the result">', ""]
            links = []
            if 'video_hd' in result:
                links.append(f"**[▶ Watch the video]({result['video_hd']})** (1280 wide, {result['video_hd_bytes'] / 1_000_000:.1f} MB)")
            if 'video' in result:
                links.append(f"[small copy]({rel(result['video'])})")
            if 'metadata' in result:
                links.append(f"[the project it wrote]({rel(result['metadata'])})")
            if links:
                pg += [" · ".join(links), ""]
            pg += ["| check | | detail |", "|---|---|---|"]
            for c in result['checks']:
                pg.append(f"| {c['check']} | {'✓' if c['ok'] else '✗'} | {c['detail']} |")
            pg.append("")
            if 'reference_contact' in result:
                pg += ["## The hand-built reference, same moments", "",
                       f'<img src="{rel(result["reference_contact"])}" width="800" alt="six moments of the reference">', ""]
        else:
            pg += ["## What the agent made", "", "*Not run yet.*", ""]
        pg += ["---", "", "[← all demos](../../demo.md) · [how the suite works](../../demos/README.md)", ""]
        open(os.path.join(ASSETS, f"demo{name[:2]}.md"), 'w').write("\n".join(pg))

        # --- the index row ---
        if result:
            st = result.get('run') or {}
            tk = st.get('tokens', {})
            total_in = tk.get('input', 0) + tk.get('cache_read', 0) + tk.get('cache_write', 0)
            status = f"**{result['score']}%** · {st.get('turns', '?')} turns · {fmt_secs(st.get('wall_s', 0))} · ${st.get('cost_usd', 0):.2f} · {fmt_tokens(total_in)} in / {fmt_tokens(tk.get('output', 0))} out"
            if st.get('mcp'):
                status += f" · MCP {fmt_secs(round(st['mcp']['ms'] / 1000))} in {st['mcp']['calls']} calls"
            if 'video_hd' in result:
                status = f"[▶ watch]({result['video_hd']}) · " + status
            pic = f'<a href="docs/demo/demo{name[:2]}.md"><img src="{result["thumb"]}" width="160"></a>' if 'thumb' in result else ""
        else:
            status, pic = "not run yet", ""
        rows.append(f"| {pic} | [{rubric['template']}](docs/demo/demo{name[:2]}.md) | {blurb} | {status} |")

    head = """# Demos — prompt in, video out

Every piece here was made by a **fresh agent**: a Claude Code session with
nothing but the media shown, the prompt shown, the public
[PromoShot skill](skill/SKILL.md) and the headless PromoShot MCP server
fenced to an empty folder. No repository, no memory, no example to copy.
It wrote the project, validated it, rendered it. Each page links the
video (GitHub plays it on its own page; the inline loop is a preview) and shows the
resources, the prompt, the result beside the piece a person built by hand
for the same brief at the same six moments, and a structural score:
length, the mix of layers, the features the prompt asked for, the words
that reach the screen.

The suite is in [demos/](demos/README.md) — adding a test is adding a
folder — and `docs/demo/demo.json` carries the same material for the
website. Footage credit: Big Buck Bunny (Blender Foundation, CC BY 3.0)
where a screen recording stands in.

The agent's work is given as turns, wall time, tokens and a cost. The
cost is the API's list price for those tokens; on a Claude subscription
the same run counts against the plan's usage window and bills nothing.
Most of the tokens are cache reads — the session's context re-read on
each turn at a tenth of the input price — which is why a run of a
million-odd tokens costs a couple of dollars at list.

| | demo | what the brief asks for | result · the agent's work |
|---|---|---|---|
"""
    open(os.path.join(CORE, 'demo.md'), 'w').write(head + "\n".join(rows) + "\n")
    json.dump(manifest, open(os.path.join(ASSETS, 'demo.json'), 'w'), indent=1)
    done = sum(1 for m in manifest if m['result'])
    print(f"demo.md: {len(manifest)} demos, {done} with results; pages and assets in docs/demo")

if __name__ == '__main__':
    main()
