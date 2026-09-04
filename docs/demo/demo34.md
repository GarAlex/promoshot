# 34 Cube To Word

A cube of six pictures turns under one light, bursts into three thousand points, and the points gather into a chrome word — a faced box, worn pictures, a morph and a text body in one stage.

*1440×900, 10 s.* Part of [the demos](../../demo.md): a fresh agent, the media and the prompt below, the public skill and the headless MCP, nothing else.

## Resources given to the agent

<img src="34-cube-to-word/resources/face_1.png" width="240" alt="face_1.png"> 
<img src="34-cube-to-word/resources/face_2.png" width="240" alt="face_2.png"> 
<img src="34-cube-to-word/resources/face_3.png" width="240" alt="face_3.png"> 
<img src="34-cube-to-word/resources/face_4.png" width="240" alt="face_4.png"> 
<img src="34-cube-to-word/resources/face_5.png" width="240" alt="face_5.png"> 
<img src="34-cube-to-word/resources/face_6.png" width="240" alt="face_6.png"> 

## The prompt

> Make a 10-second piece on a 1440×900 canvas: a background whose linear gradient DRIFTS on four eased keyframes — at 0 s from 1B2440 through 0B0D12 to 2A1638 running corner to corner ([0,0] to [1,1]); at 4.9 s 142A4A / 0C0F1A / 35183F from [0.2,0] to [0.9,1]; at 6.2 s warmer 3A2A5C / 141826 / 5A2A3A from [0.5,0] to [0.6,1] as the cube bursts; at 10 s 10162A / 090B12 / 231436 from [1,0] to [0,1] — and the studio scene environment; ONE layer of kind 'stage' named 'Bench', placed 640 px tall in the centre (offset 30 px up), whose keyframes carry a key light that stays at yaw 35, pitch 32, intensity 1.4 while the camera holds a yaw of -25 and only eases in (pitch 18 to 14, distance 4.7 to 4.3 by 4.9 s), then settles to yaw -10, pitch 6, distance 4.0 by 8.6 s as the light moves to yaw 60, pitch 28 — both camera keyframes with "easing": "smooth" so the move never stops. Its members: a CUBE that is a model resource with no file and a PARTS recipe of one box (size [1.4,1.4,1.4], radius 0.05, "faces": true) whose six slots Cube/front, Cube/right, Cube/back, Cube/left, Cube/top, Cube/bottom each WEAR one of face_1.png … face_6.png as a surface (roughness 0.35), living the whole piece and spinning once and a bit, from a yaw of 0 to 380 linearly over 5.2 s, so each lit side passes the light; a WORD that is a model resource with a TEXT recipe 'PROMO' (bold, depth 0.35, size 0.5) with a chrome Face D8DDE6 (metallic 1, roughness 0.18) and a Side 5B8CFF (metallic 0.4, roughness 0.4), living the whole piece; and POINTS: a particles resource with a MORPH from the cube to the word (count 3000, spread 1.1, size 0.013, turbulence 0.2, stagger 0.3, colors @accent, FFFFFF, FFB050, seed 11) played by a DRAWING member living the whole piece whose keyframes hold progress 0 until 4.9 s, burst to 0.45 by 5.6 s, drift to 0.62 by 6.5 s and gather to 1 by 8.2 s, all three keyframes with "easing": "smooth" so the flight is one continuous curve — the morph dissolves the cube as the points leave and assembles the word as they land; and along the bottom from 8.5 s a bold caption 'One format. Real 3D.' that fades in word by word.
> 
> Files in `resources/`: face_1.png, face_2.png, face_3.png, face_4.png, face_5.png, face_6.png.
> 
> Text to use, in order:
> - One format. Real 3D.

## What the agent made

Score **100%** (21 of 21 rubric checks).

| the agent's work | |
|---|---|
| turns | 34 |
| wall time | 2 min 57 s (API 2 min 49 s) |
| cost at API list price | $2.27 — on a Claude subscription this is plan usage, not a bill |
| tokens in | 2.5M (2.4M cache read, 73k cache write) |
| tokens out | 14k (4k thinking) |
| claude-haiku-4-5 | 2k in, 22 out, $0.00 |
| claude-opus-5 | 2.5M in, 14k out, $2.27 |

| MCP tool | calls | time |
|---|---|---|
| promo_render_video | 1 | 3 s |
| promo_render_frames | 1 | 2 s |
| promo_validate | 2 (1 refused) | 0 s |
| promo_inspect | 1 | 0 s |
| promo_schema | 1 | 0 s |
| promo_workspace | 1 | 0 s |
| promo_schema_full | 1 | 0 s |
| **all** | 8 | 5 s |

<img src="34-cube-to-word/result.gif" width="480" alt="the result, looping">

Six moments:

<img src="34-cube-to-word/contact-result.png" width="800" alt="six moments of the result">

**[▶ Watch the video](https://github.com/garalex/promoshot/raw/demo-media/34-cube-to-word.mp4)** (1280 wide, 5.7 MB) · [small copy](34-cube-to-word/result.mp4) · [the project it wrote](34-cube-to-word/result-metadata.json)

| check | | detail |
|---|---|---|
| canvas | ✓ | [1440, 900] vs [1440, 900] |
| duration | ✓ | 10.0s vs 10.0s |
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
| feature:morph | ✓ | True |
| feature:facedBox | ✓ | True |
| feature:wornPicture | ✓ | True |
| feature:textBody | ✓ | True |
| feature:environment | ✓ | True |
| feature:particles | ✓ | True |
| feature:partsBody | ✓ | True |
| phrases | ✓ | 1 of 1 lines recognisable |

## The hand-built reference, same moments

<img src="34-cube-to-word/contact-reference.png" width="800" alt="six moments of the reference">

---

[← all demos](../../demo.md) · [how the suite works](../../demos/README.md)
