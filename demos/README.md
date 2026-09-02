# Demos

Each folder here is one test of the whole product: media in, a prompt a
person would type, and what a **fresh agent** makes of it with nothing but
the public [skill](../skill/SKILL.md) and the headless MCP server — no
repository, no memory, no reference to copy. The result is scored against
a rubric and shown beside the hand-built reference at the same moments in
[demo.md](../demo.md).

```
demos/
  NN-slug/
    resources/       the media (small files; large ones live in _media/)
    prompt.md        the prompt, with the file list and caption lines
    rubric.json      canvas, duration, layer kinds, features, phrases, media
    reference.json   the hand-built project's metadata.json
    runs/<stamp>/    a run (git-ignored): ws/, agent.json, score.*, contact-*.png, agent.mp4
  _media/            files over 1 MB, shared by name
  features.py        what a project IS, structurally — shared by all three below
  score.py           rubric vs a project → percentage and per-check detail
  run.sh             one demo, one fresh agent
  run_all.sh         the set, then summary.md
  publish.py         demo.md + docs/demo/ (assets and demo.json)
  import_templates.py, prompts.json, task.md   the importer from a template library
```

## Running

```
cargo build --release -p promoshot-mcp -p promo-cli
demos/run.sh demos/23-soft-focus            # one; add a model name to override
demos/run_all.sh --skip-done                # the set, skipping demos already scored
python3 demos/publish.py                    # demo.md and docs/demo/
```

The runner needs Claude Code signed in (the binary it uses is the one the
desktop app ships, `CLAUDE_BIN=` overrides it) and ffmpeg on the PATH.
Each run spends the account's own credit.

## Adding a demo

Make a folder `NN-slug/` with `resources/`, a `prompt.md` that ends with
the text of `task.md`, a `rubric.json` and a `reference.json`. The easy
way is to build the reference by hand as a `.promo` project (it IS the
test's expected answer), then derive the rubric:

```
python3 -c "import json,sys; sys.path.insert(0,'demos'); from features import rubric_from; \
  m=json.load(open('path/to/ref.promo/metadata.json')); \
  json.dump(rubric_from(m, 'NN Name', ['a.png','b.mp4']), open('demos/NN-slug/rubric.json','w'), indent=1)"
```

A whole template library imports with `import_templates.py <library>`;
prompts for it live in `prompts.json` keyed by the template's number.

## What the score means

Structural, in the sense of the prompt: the canvas (or its aspect), the
length within a quarter, each layer kind present in about the right
number, every feature the reference uses (a chroma key, a LUT, chapters,
effects, a mask, viewports, sprites, reveals, blend modes, swaps,
transitions, timing anchors, speed cuts, narration, motion blur, a
grade), and whether the words on screen are the words in the prompt. A
reference scores 100 by construction; a run is judged by its percentage
and by the two contact sheets side by side — the score says what is
there, the sheets say whether it reads.
