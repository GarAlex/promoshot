# Examples — the recipes, runnable

One project per recipe in `promo_schema`, synthetic media included. Each
`metadata.json` is byte-identical to its recipe in the doc — a test holds
the two together — so what the doc teaches is exactly what renders here.

| Project | Teaches |
|---|---|
| `ProductCard.promo` | The product-promo path: a device-framed app screenshot pushes in over a palette ground, title above — 16:9, 6s |
| `TwoClips.promo` | Two video layers overlapped under a wipe (video cannot swap; stills do this on one layer) |
| `FocusPush.promo` | A recording with a Ken Burns `viewport` push and a stroked lower caption |
| `Story.promo` | The SAME card re-stamped 9:16 — placement rules are why this is a re-stamp, not a redesign |
| `LinuxSmoke.promo` | The kitchen-sink smoke project: clip + audio, image, palette, easing, a waiting keyframe |

Render any of them:

    promo video examples/ProductCard.promo --out card.mp4
