# 25 Turntable

A generated phone model on a turntable, its body painted by the palette, beside extruded kinetic type.

*1440×900, 7 s.* Part of [the demos](../../demo.md): a fresh agent, the media and the prompt below, the public skill and the headless MCP, nothing else.

## Resources given to the agent

`phone.glb` ([file](../../demos/25-turntable/resources/phone.glb))

## The prompt

> Make a 7-second piece on a 1440×900 canvas: a radial dark-blue gradient background; the phone.glb model placed on the left, 760 px tall, its Body material painted with the palette's accent, turning on a turntable from a yaw of -70 to 35 degrees over the whole piece with an ease in and out, lit from the upper left; on the right a bold two-line title 'Every theme,\nevery angle.' in extruded type that slides in word by word, and under it a smaller line 'A .glb on a layer. The palette paints it.' that fades in word by word two seconds later.
> 
> Files in `resources/`: phone.glb.
> 
> Text to use, in order:
> - Every theme,
> every angle.
> - A .glb on a layer. The palette paints it.

## What the agent made

Score **100%** (10 of 10 rubric checks).

| the agent's work | |
|---|---|
| turns | 49 |
| wall time | 4 min 58 s (API 4 min 55 s) |
| cost at API list price | $2.63 — on a Claude subscription this is plan usage, not a bill |
| tokens in | 2.8M (2.7M cache read, 75k cache write) |
| tokens out | 21k (10k thinking) |
| claude-haiku-4-5 | 1k in, 15 out, $0.00 |
| claude-opus-5 | 2.8M in, 21k out, $2.63 |

| MCP tool | calls | time |
|---|---|---|
| promo_render_video | 1 | 1 s |
| promo_validate | 1 | 1 s |
| promo_render_frames | 3 | 0 s |
| promo_inspect | 1 | 0 s |
| promo_schema_types | 1 | 0 s |
| promo_schema | 1 | 0 s |
| promo_workspace | 1 | 0 s |
| promo_schema_full | 1 | 0 s |
| **all** | 10 | 2 s |

<img src="25-turntable/result.gif" width="480" alt="the result, looping">

Six moments:

<img src="25-turntable/contact-result.png" width="800" alt="six moments of the result">

**[▶ Watch the video](https://github.com/garalex/promoshot/raw/demo-media/25-turntable.mp4)** (1280 wide, 0.2 MB) · [small copy](25-turntable/result.mp4) · [the project it wrote](25-turntable/result-metadata.json)

| check | | detail |
|---|---|---|
| canvas | ✓ | [1440, 900] vs [1440, 900] |
| duration | ✓ | 7.0s vs 7.0s |
| kind:background | ✓ | 1 vs 1 |
| kind:model | ✓ | 1 vs 1 |
| kind:caption | ✓ | 2 vs 2 |
| feature:gradient | ✓ | True |
| feature:reveal | ✓ | True |
| feature:depth | ✓ | True |
| feature:kineticReveal | ✓ | True |
| phrases | ✓ | 2 of 2 lines recognisable |

## The hand-built reference, same moments

<img src="25-turntable/contact-reference.png" width="800" alt="six moments of the reference">

---

[← all demos](../../demo.md) · [how the suite works](../../demos/README.md)
