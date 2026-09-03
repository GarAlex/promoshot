# 29 Bench

The bench of demo 28 as one stage layer — camera and light on the stage, three finished vases as its members, each turning on its own.

*1440×900, 9 s.* Part of [the demos](../../demo.md): a fresh agent, the media and the prompt below, the public skill and the headless MCP, nothing else.

## Resources given to the agent

`vase.glb` ([file](../../demos/29-bench/resources/vase.glb))

## The prompt

> Make a 9-second piece on a 1440×900 canvas: a radial dark-grey gradient background and the studio scene environment (compositionSettings.environment preset studio); ONE layer of kind 'stage' named 'Bench', placed 520 px tall in the centre, whose own keyframes carry the camera (drifting from a yaw of -14 to 14 over the piece with an ease in and out) and the key light (sweeping from a yaw of -70 to 70 at intensity 1.3), and whose members are vase.glb three times as three model resources over the same file — the first offset 1.5 left, painted silver D2D6DC with a chrome finish on the binding (metallic 1, roughness 0.12); the second in the middle, the palette's accent with a matte finish (metallic 0, roughness 0.85); the third offset 1.5 right, the same accent with a gloss finish (metallic 0, roughness 0.12) — every member turning a little on its own from a yaw of -20 to 20; and along the bottom a bold caption 'One stage. One layer.' in extruded type that fades in word by word from 1.2 seconds.
> 
> Files in `resources/`: vase.glb.
> 
> Text to use, in order:
> - One stage. One camera.

## What the agent made

Score **100%** (15 of 15 rubric checks).

| the agent's work | |
|---|---|
| turns | 30 |
| wall time | 2 min 41 s (API 2 min 37 s) |
| cost at API list price | $1.59 — on a Claude subscription this is plan usage, not a bill |
| tokens in | 1.7M (1.7M cache read, 48k cache write) |
| tokens out | 11k (3k thinking) |
| claude-haiku-4-5 | 1k in, 18 out, $0.00 |
| claude-opus-5 | 1.7M in, 11k out, $1.59 |

| MCP tool | calls | time |
|---|---|---|
| promo_render_video | 1 | 2 s |
| promo_render_frames | 2 | 0 s |
| promo_validate | 2 | 0 s |
| promo_inspect | 1 | 0 s |
| promo_schema | 1 | 0 s |
| promo_schema_full | 1 | 0 s |
| **all** | 8 | 2 s |

<img src="29-bench/result.gif" width="480" alt="the result, looping">

Six moments:

<img src="29-bench/contact-result.png" width="800" alt="six moments of the result">

**[▶ Watch the video](https://github.com/garalex/promoshot/raw/demo-media/29-bench.mp4)** (1280 wide, 0.4 MB) · [small copy](29-bench/result.mp4) · [the project it wrote](29-bench/result-metadata.json)

| check | | detail |
|---|---|---|
| canvas | ✓ | [1440, 900] vs [1440, 900] |
| duration | ✓ | 9.0s vs 9.0s |
| kind:background | ✓ | 1 vs 1 |
| kind:stage | ✓ | 1 vs 1 |
| kind:caption | ✓ | 1 vs 1 |
| feature:gradient | ✓ | True |
| feature:reveal | ✓ | True |
| feature:stage | ✓ | True |
| feature:stageLayer | ✓ | True |
| feature:model | ✓ | True |
| feature:camera | ✓ | True |
| feature:materials | ✓ | True |
| feature:finish | ✓ | True |
| feature:environment | ✓ | True |
| phrases | ✓ | 1 of 1 lines recognisable |

## The hand-built reference, same moments

<img src="29-bench/contact-reference.png" width="800" alt="six moments of the reference">

---

[← all demos](../../demo.md) · [how the suite works](../../demos/README.md)
