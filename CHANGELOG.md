# Changelog

Releases follow `0.MINOR.PATCH` until 1.0: MINOR moves for a new format rung (`minReaderVersion`), a new tool, or user/agent-visible behaviour changes; PATCH is for fixes and packaging updates.

## 0.4.9 — 2026-11-27

- Updated `local-tts-core` to `69dcba0da` (core, catalog, Kokoro/Piper/VITS, and English pronunciation fixes).
- `promoshot-mcp`: added `--version` / `-V` and `--help` / `-h` flags.
- Docker image: created writable `/music` (`PROMOSHOT_MUSIC_MODELS`) and `/sounds` (`PROMOSHOT_SOUNDS`) mountpoints for the non-root `promoshot` user and verified them in the release smoke test.
- Release CI: added a macOS tarball smoke test (`promo --version`, `promoshot-mcp --version`, `ProductCard.promo` still render).
- Docs & metadata: added `rust-version`, `repository`, and `homepage` to `Cargo.toml`; updated `README.md` (crates table, rung 51, Docker volumes) and `promoshot.pad.xml` (`3.3`).

## 0.4.8 — 2026-11-23

- Multi-language projects (`languages`, `promo strings`, `--language` renders), Lyria song support, normalize measured over played ranges, and issues #21–#27.

## 0.4.7 — 2026-10-28

- Fixed Linux release tarball publication.

## 0.4.6 — 2026-10-28

- SVG as a vector resource (`promo-svg`, `promo resource add`), bundled CPU Vulkan (`lavapipe/`) for headless Linux tarballs.

## 0.4.5 — 2026-10-25

- Audio layers can play a video resource's sound; overshoot (bouncy/elastic) easings.

## 0.4.4 — 2026-10-23

- Google word marks on narration; keyless local voices (`local-tts`) wired for the apps.

## 0.4.3 — 2026-10-22

- Swap transitions preserve the outgoing picture.

## 0.4.2 — 2026-10-22

- Composition swaps, looping audio, per-layer ducking depth/speed, imported word timings.

## 0.4.1 — 2026-10-21

- Speech aligner pauses at script punctuation (#10).

## 0.4.0 — 2026-10-21

- Speech-sync timing (`promo words`, word-timed captions, layer snapping to words/sentences).
