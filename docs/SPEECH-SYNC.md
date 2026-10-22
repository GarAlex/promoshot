# Speech timing and speech sync: implementation design

Status: slices 1 to 5 built (2026-10-21); where building changed the design,
the "Slice N as built" notes under §10 say what and why. Modelled on
`docs/BEAT-SYNC.md`, including its "as built" notes, and reusing its
machinery wherever it fits. Owner requirements: one word-timing format for
every take whatever produced it; time fields that point into a take, the way
musical values point into a beat grid; existing files unchanged.

The rule stays the beat-sync rule, which `promo-timeline/src/attachment.rs`
first stated: **resolve once, write numbers, and leave nothing for a renderer
to interpret.** A value like "word 12 of the intro" is a spec a person edits.
`startTime`, `duration` and the other fields stay plain `f64` answers. The
engine never learns that a word exists. It learns only that a caption has
per-unit arrival times (§7.1), which are plain numbers too.

---

## 0. What exists today (the starting point)

- **`promo-speech`** buys takes:
  - `settle_except` walks the resources whose `speech.text` says something. The receipt is `renderedHash` = sha256(provider|voice|text), plus a lexicon hash for local voices.
  - `placehold` writes a silent MP3 of `estimate::estimate` length, sets `speech.placeholder: true`, and leaves no receipt.
  - `synthesize_with_key` posts to OpenAI `/v1/audio/speech`, ElevenLabs `/v1/text-to-speech/{voice}` and Google `v1/text:synthesize`. None of them asks for timing.
  - `local::synthesize` runs local-tts and returns WAV bytes only.
- **The apps:**
  - They synthesize through the same client over the FFI (`promo_speech_*` job API, core-private `promo-ffi/src/speech.rs`) and write placeholders through `promo_speech_placeholder_write`.
  - The FFI links `promo-speech` with `default-features = false`, so **the apps have no local voices**. A local take is always made headless, and the app only opens it.
  - The Swift `SpeechSynthesizer.generate` measures the duration with AVFoundation, and that is all it learns about a take.
- **Caption reveal** (`promo-timeline/src/reveal.rs`) is a uniform walk. `TextReveal::total_seconds` spreads N units over `seconds`, `secondsPer`·N or the layer's tenure.
  - A word unit is a whitespace-separated run of the caption's text (`promo-text` `reveal_layout`, `RevealBy::Word`), so a punctuation-only token such as "—" is a unit of its own.
  - Nothing knows when a word is actually said.
- **The local engines** (`/Users/alex/src/tts`, remote `https://github.com/GarAlex/local-tts.git`). PromoShot pins tag `v0.6.0` (23ccd8e) in `promo-speech/Cargo.toml`; the cargo checkouts are under `~/.cargo/git/checkouts/local-tts-b719b0f9f09eb0be/`. Both PromoShot models have duration predictors. I read the ONNX graphs of the installed packages in `~/src/tts/voices`, with a protobuf walk and no onnx module:

  | package | graph outputs | per-token durations |
  |---|---|---|
  | `kitten-nano-en` (`kitten_tts_nano_v0_8.onnx`) | `waveform`, `duration` | **already an output**; `compact_english.rs` reads only `out[0]` |
  | `kokoro-en-q8` (and `kokoro-en`, `kokoro-en-q4`) | `waveform` only | computed inside the graph as `/encoder/Clip_output_0` (Round, then Clip ≥ 1, then the `CumSum` that expands frames); not exposed |
  | `inflect-nano-en` (`duration.onnx`) | `m_p_exp`, `logs_p_exp`, `y_mask` (already expanded) | not exposed (out of scope, §3.4) |

---

## 1. The word-timing format (one format for every take)

### 1.1 The record

`promo-model/src/speech.rs` (new) holds the wire types, so the timeline and
`promo-author` can read them without `promo-media`. The record is the same in
the cache file, in CLI `--json` and in MCP:

```json
{ "timingVersion": 1,
  "audio":  {"filename": "narration-intro.mp3", "sha256": "…32 hex…", "bytes": 48213},
  "script": "Meet Euclid. Geometry, proved step by step.",
  "scriptHash": "9c1e04aa",
  "duration": 3.912,
  "source": "provider:elevenlabs",
  "estimated": false,
  "confidence": 0.98,
  "words": [
    {"text": "Meet",   "start": 0.104, "end": 0.341, "chars": [0, 4]},
    {"text": "Euclid", "start": 0.362, "end": 0.903, "chars": [5, 11]},
    {"text": "Geometry", "start": 1.390, "end": 1.948, "chars": [13, 21], "confidence": 0.9}
  ],
  "sentences": [ {"start": 0.104, "end": 0.903, "words": [0, 1], "chars": [0, 12]},
                 {"start": 1.390, "end": 3.701, "words": [2, 6], "chars": [13, 43]} ] }
```

**Fields**
- `source` is one of:
  - `provider:elevenlabs` or `provider:google`: the provider's own timing
  - `local:<package>/<speaker>`: the model's duration predictor
  - `aligned`: offline forced alignment (§4)
  - `transcribed:openai` or `transcribed:elevenlabs`: opt-in provider transcription (§4.6)
  - `estimate`: syllable estimate (§5)
- `estimated: true` only for `estimate`. It is a separate flag so that a reader never parses `source` to learn whether it may trust the times.
- `confidence` is 0–1 for the take as a whole. A word's own `confidence` is written only where it differs from the take's (aligned takes, and words that were interpolated).
- `words[i].text` is the script's token with its outer punctuation removed (§1.2). `chars` is `[start, end)` in **Unicode scalar indices into `script`**: the unit ElevenLabs' `characters` array uses and Swift's `unicodeScalars` view counts. Times are in the take's **own source seconds**, rounded to 1 µs, as in beats.
- `words[i].interpolated: true` marks a script word the source never voiced on its own. A provider may say "$49" as four words, and a word may get no match (§2.5). Its times are shared out within its neighbours' gap.
- `sentences[i].words` is `[first, last]` inclusive, as word indices.
- `prominence` (optional, 0–1 per word) is computed from the audio for every source (S8, §8.2). It is a cue for generators and never a timing.

**Units and numbering.** On the wire, a word index is **1-based**, matching `{"beat": 1}`. In the record, `words` is a plain array (0-based in code); the CLI's tables and the labels print 1-based.

### 1.2 Words and sentences from a script, one tokenizer

`promo_model::speech::tokenize(script) -> Script { words, sentences }` is the
only tokenizer. Providers, the aligner, the estimator, the resolver and the
reveal mapping all go through it.

- **Words** are maximal runs of non-whitespace, the same units `promo-text` reveals by word. Leading and trailing punctuation (`"“”‘’«»()[]{}.,;:!?…—–-`) is stripped into the token's margins. Inner apostrophes and hyphens stay ("don't", "step-by-step").
- **A token with no letter or digit left** ("—", "&") is no word. A caption unit made of one takes its neighbour's time (§7.1).
- **SSML scripts** (Google, text starting with `<speak`): words are tokenized from text nodes only. Tags are skipped, and `chars` index into the raw SSML string.
- **Sentences** end at `.`, `!`, `?` or `…` followed by whitespace or the end, or at a blank line. This is the boundary rule `local-tts` `paragraph::chunks` uses, plus an abbreviation list ("Dr.", "Mr.", "e.g.", "i.e.", "vs.", "etc."). A decimal never ends a sentence, because the rule requires whitespace after the point.
- **Scripts with no spaces** (CJK, Thai): v1 makes each Han, kana or Thai cluster a word. Their reveal is by character, so a caption still maps. Validate notes the coarser sync (§9).

### 1.3 Where it is stored: an authoritative cache keyed by audio, not the resource

**Decision:** the record lives in `<project>/Resources/.words/<audio32>-<script8>.json`.
- `audio32` is the first 32 hex digits of SHA-256 of the audio file's bytes. This is `beat_cache::source_of` with its (path, size, mtime) memo, generalised into `promo_model::media_cache`.
- `script8` is 8 hex digits of SHA-256 of the script text exactly as written.
- `promo_model::word_cache::{cache_path, lookup, store, takes_needing_timing}` is the only code that knows the naming, as `beat_cache` is for beats.

Why not inline in `speech`:
- **Size.** It is derived data, about 60 bytes a word: 25 KB for a three-minute voice-over, in a `metadata.json` that every save rewrites and Swift decodes.
- **Irreproducibility.** Provider timing cannot be recomputed, so it must be stored the moment the take is bought, next to the bytes it describes, even before the document is written (§3.1).
- **Keying.** Keyed by content, the record follows the take through renames, library moves and duplicate projects. A re-synthesised take is new bytes, so it gets a new key, and every dependent time re-times on the next resolve. That is the mechanism of "editing the script re-times everything".
- **The script half of the key.** Two silent placeholders of the same length are byte-identical. A recording re-aligned to a corrected script is the same audio with different words. So the script is part of the key.

**Lookup** for a resource, given its audio and its current script text:
1. `<audio32>-<script8>`, an exact match: used.
2. Otherwise, exactly one record for that audio under another script: the take is **stale**. The text was edited and no take was bought (no key, or not yet). The old record is used, because it describes what the take actually says, and validate warns that the take is stale (§9). Several stale records: the newest by `timingVersion`, then by file mtime.
3. Nothing: the take is in `missingWords`. On open, the CLI aligns it (§4.5). The app runs `WordAligner` (§8.1). Meanwhile resolution uses an estimate (§6.4).

**Authority.**
- Once a `provider:*`, `local:*` or `transcribed:*` record exists, it is never replaced except by an explicit `promo words --refresh` (which can only re-align) or by a new take.
- `aligned` and `estimate` records carry `alignerVersion`. A record from an older aligner is re-aligned on open, the precedent BEAT-SYNC's tracker-version notes set ("an older tracker's cached answer is measured again"), because alignment is offline, deterministic and free. validate notes "re-aligned with aligner 2" once.

**Hand corrections.**
- `speech.wordFixes` on the resource: `[{"word": 12, "start": 4.38}, {"word": 13, "end": 5.02}]`, 1-based, in source seconds. Applied over the record at resolve time.
- It is ignorable (rung 48), and it is the only timing data in `metadata.json`.
- Fixes name the script they were made for (`"scriptHash"`). A fix for another script is ignored, and validate says so.

**Recordings with no `speech`.** An imported voice-over (an audio resource, or a video resource with a voice track) states what it says in a new resource field, `transcript: "…"`. That is the script the aligner uses. It is ignorable (rung 48).
- It is not `speech.text`: `promo-speech::script()` defaults a missing provider to OpenAI, and settle would try to buy a take over the recording.
- A resource with both: `speech.text` wins, because it is what the take was bought from.

### 1.4 Rust shape

```rust
pub struct TakeTiming { pub timing_version: u32, pub audio: MediaSource, pub script: String,
    pub script_hash: String, pub duration: f64, pub source: TimingSource, pub estimated: bool,
    pub confidence: f64, pub words: Vec<Word>, pub sentences: Vec<Sentence>,
    pub aligner_version: Option<u32>, #[serde(flatten)] pub extra: Map<String, Value> }
pub struct Word { pub text: String, pub start: f64, pub end: f64, pub chars: [u32; 2],
    pub confidence: Option<f64>, pub interpolated: bool, pub prominence: Option<f64> }
pub struct SpokenWord { pub text: String, pub start: f64, pub end: f64,
    pub chars: Option<[u32; 2]> }   // what a source hands back, before §2.5
```

---

## 2. Where timing comes from, per provider

`promo-speech` gains a module, `timing.rs`. Its `Synth` trait gains a method
with a default, so every existing fake keeps compiling:

```rust
pub struct Take { pub audio: Vec<u8>, pub spoken: Option<Spoken> }
pub struct Spoken { pub words: Vec<SpokenWord>, pub source: TimingSource, pub against: SentText }
pub trait Synth {
    fn synthesize(&self, provider: &str, voice: &str, text: &str) -> Result<Vec<u8>, String>;
    fn synthesize_timed(&self, provider: &str, voice: &str, text: &str) -> Result<Take, String> {
        self.synthesize(provider, voice, text).map(|audio| Take { audio, spoken: None })
    }
}
```

`synthesize_with_key_timed(provider, voice, text, key) -> Take` is the client.
`synthesize_with_key` becomes `…_timed(..).audio`, so the bytes the apps get
are unchanged.

### 2.1 Stress spellings and offsets

Every provider is sent `marking_stress(text)`. That conversion maps an
apostrophe to U+0301 one for one, and a capital vowel to lowercase plus
U+0301, one char to two. `stress::marking_stress_mapped(text) -> (String,
Vec<u32>)` returns, for each sent char, the script char it came from. The
existing function becomes `.0` of it, and a test holds the two equal over the
stress vectors. Provider char offsets are mapped back through this table
before §2.5 runs.

### 2.2 ElevenLabs: `with-timestamps`

**Request:**
- `POST https://api.elevenlabs.io/v1/text-to-speech/{voice}/with-timestamps?output_format=mp3_44100_128`
- The same headers and body as today (`{"text", "model_id": "eleven_multilingual_v2"}`).

**Response:** JSON, not bytes:
```json
{ "audio_base64": "…", "alignment": { "characters": [...],
  "character_start_times_seconds": [...], "character_end_times_seconds": [...] },
  "normalized_alignment": { … } }
```
- The audio comes from the existing `base64_decode`.
- **`alignment`, not `normalized_alignment`.** `alignment` is over the characters we sent, so it maps straight back to script offsets. `normalized_alignment` is over ElevenLabs' own text normalization.
- The answer is about 1.35× the MP3's size. That is acceptable at narration lengths.

**Characters to words:**
1. Concatenate `characters` and check it equals the sent text.
   - If it does not (ElevenLabs has dropped or merged characters), run a char-level DP (Levenshtein with backpointers) to map each returned char to a sent char.
2. Map through §2.1 to script chars.
3. For each script word, the start is that of the first char inside `chars`, and the end is that of the last non-space char.

The result is `source: "provider:elevenlabs"`, with confidence 1.0, or 0.9 when the DP was needed.

The ElevenLabs path is still "UNVERIFIED against a live key", as
`SpeechSynthesis.swift` says. S3 proves it through the Xcode-run app's MCP
(`speech-tests-through-the-xcode-run-app`).

### 2.3 Google: `v1beta1` with `<mark>` before each word

**Request:**
- `POST https://texttospeech.googleapis.com/v1beta1/text:synthesize`
- The same voice and `audioConfig` as today, plus `"enableTimePointing": ["SSML_MARK"]`.
- The input is always SSML. `google_input` stays the untimed form, and `google_ssml_marked` is new:
  - Plain text: `<speak>` + for each token, its leading margin + `<mark name="w{k}"/>` + XML-escaped word + margin … + `</speak>`, where `k` is the script word index. Whitespace and newlines are kept as written.
  - A script that is already SSML: marks go into text nodes only. A word inside `<say-as>`, `<sub>` or `<phoneme>` gets one mark before the element (a mark inside them is invalid). Words in one such element share it, and their times are interpolated.
- **Response:** `{"audioContent", "timepoints": [{"markName": "w3", "timeSeconds": 0.812}], …}`.
- **Times:**
  - A word's start is its mark's time.
  - Its end is the next mark's time, pulled back to the end of voiced energy before it. A pause, a comma or a sentence break sits between marks. The pull-back uses the aligner's VAD (§4.2) on the decoded take, limited to the gap.
  - The last word ends at the end of voiced energy.
- The result is `source: "provider:google"`, confidence 0.95.

**Which voices** (decided by name, in `google_supports_marks(voice)`):

| class (the third name segment) | SSML and marks | what we do |
|---|---|---|
| `Standard`, `Wavenet`, `Neural2`, `News`, `Polyglot`, `Studio` | yes | the v1beta1 marked request |
| `Chirp3-HD`, `Chirp-HD`, `Journey`, anything unknown | no SSML, or marks unsupported | today's v1 plain request, then align (§4) |
| bare names (Gemini, Vertex-only) | no | unchanged; already refused with a key |

The memory note recommends `en-US-Chirp3-HD-<Name>`, so the popular voice gets
offline alignment. That is acceptable: the aligner does best on clean TTS
(§4.7).

**Runtime guards:**
- Fewer timepoints than marks: use the audio, discard the marks, align, and say so in the report line.
- A 400 that names SSML or timepoints: retry once with the plain v1 request. A refused request is not billed.
- To verify in S3: whether `<mark>` tags count toward billed characters. If they do, a mark costs about 14 characters a word, and the `promo_speak check` cost line says so.

### 2.4 OpenAI: no timing, so alignment

`/v1/audio/speech` (`gpt-4o-mini-tts`) returns audio only. Its takes get
`aligned` timing (§4) right after the bytes are written, in the same settle
step. The opt-in transcription in §4.6 is the paid upgrade.

### 2.5 Spoken words to script words (one mapper for every source)

`timing::attach(script: &Script, spoken: &[SpokenWord]) -> Vec<Word>` takes
what a source said and returns the script's words. It is used by local takes
(the engine reports normalized words: "$49.99" becomes "forty nine dollars
ninety nine cents"), by transcriptions, and by any provider answer whose chars
did not map.

- **The alignment:** a monotonic DP of script tokens against spoken tokens.
  - Moves: match or substitute (1:1); one script token to k spoken tokens (1:k, k ≤ 8: numbers, money, dates, acronyms spelled out); deletion (a script token nothing said); insertion (a spoken word not in the script).
  - Costs: 0 for an exact match after lowercasing and NFKD with accents stripped; 0.2 for a 1:k merge whose script token holds a digit, a currency sign, `%` or capitals; 0.5 for edit distance ≤ 1/3 of the length; otherwise 1 for a substitution, a deletion or an insertion.
  - Ties go to the lowest index.
- **Times:** a 1:k word spans its first to its last spoken word. A deleted word gets `interpolated: true` within its neighbours' gap, with confidence 0.3.
- **When it is a failure:** if over 25% of words are deleted or inserted, the result is a mismatch (the audio does not say this script). It is stored with confidence ≤ 0.3, and validate warns (§9).

---

## 3. The local-tts change (repo `/Users/alex/src/tts`, release v0.7.0)

### 3.1 The API

The change is additive. `SynthesisRequest` and `Synthesizer` are unchanged:
adding a field to the request struct would break every struct literal that
builds one, including `promo-speech/src/local.rs`. New, in
`crates/tts-core/src/lib.rs`:

```rust
/// One spoken word of the text as the engine read it — normalized words
/// ("forty", "nine", "dollars"), not the caller's spelling.
pub struct SpokenWord { pub text: String, pub start: f64, pub end: f64 }
pub struct TimedAudio { pub audio: Audio, pub words: Vec<SpokenWord>,
                        /// Seconds per duration unit the model predicted in.
                        pub frame_seconds: f64 }
pub trait TimedSynthesizer: Synthesizer {
    fn synthesize_timed(&mut self, request: &SynthesisRequest<'_>) -> Result<TimedAudio, Self::Error>;
}
```

`Kokoro` and `CompactEnglish` with `Kitten` implement it. `CompactEnglish`
with `Inflect` returns `Err("timing not supported for Inflect")`; PromoShot
then aligns (§3.4).

**Promise:** the waveform from `synthesize_timed` is the same bytes as from
`synthesize` for the same request. Tests pin this (§3.5).

### 3.2 What changes inside

1. **`english.rs`: phoneme spans.**
   - `phonemize_impl` already pushes each word's phones inside `flush`, starting at `let start = out.len()`. It records `PhonemeWord { spoken: word.clone(), range: start..out.len() }` there.
   - A new `phonemize_american_spans` / `phonemize_ipa_current_spans` returns `(String, Vec<PhonemeWord>)`. The existing functions call it and drop the spans.
   - The `connected_us` rewrite happens inside the same `start` window, so a span still covers its word. For `ipa()`'s vowel expansion (A becomes eɪ…), the map is applied span by span.
2. **`paragraph.rs`:** `chunks_with_offsets(text, limit) -> Vec<(String, usize)>` returns each chunk with its char offset in the input, after the trim. `chunks` delegates to it.
3. **`kokoro.rs`: expose the durations without a new download.**
   - `Kokoro::load` still checks the pinned file's SHA-256, as today.
   - It then builds the session from the bytes in memory, patched by a new `onnx_output::expose(bytes, "/encoder/Clip_output_0", "durations", Float)`. That function appends one `ValueInfoProto` to `GraphProto.output` (field 12) and re-encodes the two enclosing length prefixes. It is about 80 lines of protobuf with no `onnx` crate, and the result goes to `commit_from_memory`.
   - The file on disk and its pin are unchanged, so an installed package needs no reinstall.
   - `synthesize_phonemes_timed` reads `outputs["durations"]`: one count per token, including the leading and trailing pad 0s.
   - One unit is 600 samples at 24 kHz (25 ms): hexgrad's `KPipeline` doubles the predicted durations and divides by 80. This is **pinned by a test, not assumed**: `sum(d)·600 = samples.len()` within one unit.
   - If an export ever disagrees, the unit is `samples.len() / sum(d)`, which is exact arithmetic and still deterministic.
4. **`compact_english.rs` (Kitten): read `out[1]`, the existing `duration` output.**
   - Tokens are `[0] + ids(kitten_spacing(chunk)) + [10, 0]`. `kitten_spacing_mapped` returns the same string plus each output char's source index, so a phoneme span maps to token indices. A test holds it equal to `kitten_spacing`.
   - The 5000-sample tail that `samples.truncate` removes is subtracted. A word past it is clamped and flagged.
   - The unit is pinned the same way as Kokoro's.
5. **Words.** For each `PhonemeWord`, start = the cumulative frames before its first token, and end = the cumulative frames after its last token, times `frame_seconds`. Spaces and punctuation tokens fall in the gaps.
   - Chunks join as `paragraph::append` joins them: each later chunk is offset by the earlier audio plus 120 ms.
   - All of this is integer frames times one f64 constant: no transcendentals, and identical on every machine.

### 3.3 PromoShot's side

- `promo-speech/Cargo.toml` pins `tag = "v0.7.0"`.
- `local::synthesize_timed(voice, text, lexicon) -> (Vec<u8>, Vec<SpokenWord>)`. `LiveSynth`, `ProjectSynth` (in `promoshot-mcp/src/speak.rs`) and the settle walk call it.
- §2.5 maps the result to script words, with `source: "local:kokoro-en-q8/af_heart"` and confidence 0.97. It is not 1.0 because the predictor's durations are frame-quantized at 25 ms and coarticulation blurs edges.
- The lexicon is part of the receipt already (§0), so a pronunciation edit re-voices the take and re-times it.

### 3.4 Inflect, math voices

- **Inflect** (`duration.onnx`) emits only frame-expanded tensors. v0.7 does not expose its durations; its takes are aligned. A later local-tts release can expose the tensor before its `y_mask`/`CumSum` expansion the same way.
- **`math_speech.rs`** voices are concatenations of word clips, so their timing is known by construction. They are not PromoShot voices, and nothing is planned for them.

### 3.5 local-tts tests (in `crates/tts-core`, run where packages are installed)

- `timed_audio_equals_untimed_audio`: Kokoro q8/f32/q4 and Kitten, byte-identical samples over the short corpus
- `durations_sum_to_samples`: pins `frame_seconds` per model
- `phoneme_strings_unchanged_by_spans`: over `math_speech::corpus` and the narration fixtures
- `word_spans_cover_every_spoken_word`: no gaps or overlaps in token ranges
- `chunk_offsets_join_like_append`
- `onnx_expose_is_idempotent_and_keeps_the_pinned_digest`
- `timed_is_deterministic`: two runs give the same JSON

---

## 4. Offline forced alignment (recordings, OpenAI takes, old takes)

### 4.1 Where and how it is fed

- `promo-media/src/words.rs` holds a pure, chunk-fed meter shaped like `BeatMeter`:
  ```rust
  pub const ALIGNER_VERSION: u32 = 1;
  pub struct WordAligner { .. }
  impl WordAligner {
      pub fn new(channels: usize, sample_rate: f64, script: &str, options: AlignOptions) -> Option<Self>;
      pub fn push(&mut self, interleaved: &[f32]);
      pub fn finish(self) -> TakeTiming;   // source "aligned"
  }
  ```
  `AlignOptions` carries `language` (from `estimate::voice_language` or the resource) and `calibration` (estimate.rs).
- **No model and no download.** The tools are the beats DSP, which moves into `promo-media/src/dsp.rs` and is shared: the 64-tap windowed-sinc resampler (to 16 kHz), the radix-2 FFT and `libm` transcendentals.
- **Determinism:** the BEAT-SYNC §3.4 rules apply unchanged. Sequential f64 reductions, ties to the lowest index, times rounded to 1 µs, and the core resamples, so the CLI (ffmpeg) and the app (AVFoundation) give identical JSON from identical PCM.

### 4.2 Features (10 ms hop, 25 ms Hann window, 16 kHz mono)

- log energy, and an energy envelope smoothed over 30 ms
- voicing: the normalised autocorrelation peak for 70–400 Hz lags
- spectral flux over 24 log bands from 100 Hz to 7.6 kHz
- zero-crossing rate, so sibilants count as speech
- **VAD** with hysteresis:
  - the noise floor is the 10th percentile of energy
  - speech is above floor + 12 dB, or above floor + 6 dB and voiced
  - a hangover of 50 ms, and a pause is at least 80 ms
- **Syllable nuclei** as in de Jong & Wempe (2009): intensity peaks at least 2 dB above the surrounding dip, voiced, and at least 60 ms apart.

### 4.3 The expectation from the script

`estimate.rs` exposes `word_weights(script, language) -> Vec<WordWeight
{syllables, pause_after}>`. These are the same syllable counts and
punctuation pauses the length estimate uses (`word_syllables`,
`number_syllables`, `pause_for`, made per word).

The expected length of word i is `syllables_i / rate`. The rate is fitted so
that the total expected speech time equals the VAD's total speech time,
which keeps the per-word priors relative.

### 4.4 The alignment: a DP over candidate boundaries

1. **Candidate boundaries:** every VAD edge, plus every local minimum of the smoothed envelope at least 3 dB below both neighbouring peaks, plus a 30 ms lattice inside speech runs longer than 400 ms with no minimum. That is typically 3–6 candidates per word.
2. **The state** is (the word, the candidate where it starts). A transition places word i over [b_j, b_k), optionally followed by a pause segment [b_k, b_l) that must be non-speech under the VAD.
3. **The cost of word i on [b_j, b_k):**
   - duration: `(ln(len / expected_i))² / (2·0.35²)` (log-normal, σ = 0.35)
   - nuclei: `0.5·|nuclei in span − syllables_i|`
   - edges: `−0.3` per edge on a VAD edge or a deep dip, `+0.6` per edge in the middle of a voiced, high-energy region (a cut through a vowel)
   - silence inside: `+2·(the fraction of the span that is non-speech)`
4. **The cost of a pause after word i:** `−0.4` when `pause_after_i > 0` (the script puts punctuation there), `+0.4` otherwise, plus the duration prior of `pause_after_i`.
5. **Search:** Viterbi with backpointers. A word is limited to at most 4× its expected length, so the cost is O(words × candidates × window). A five-minute voice-over is under 100 ms of compute.
6. **Confidence:** for each word, from the margin between the best and second-best boundary on each edge, mapped through `1 − e^(−margin)`. The take's confidence is the mean, times a fit factor (the nuclei count over all words against the expected total).

### 4.5 When it runs

- **CLI open** (`Project::open_with`, beside `measure_grids`): every take that a speech value, a reveal `sync` or a speech generator in the project needs, and that has no record, is decoded and aligned, and the record is stored. As for beats, `--no-analyse` reads the cache only.
- **`promo speak`** (settle), after writing an OpenAI or Chirp take: aligns at once, so the record exists before the receipt is persisted.
- **`promo words … --refresh`**: re-aligns an `aligned` or `estimate` record. A `provider:*` or `local:*` record is never replaced; refresh then says "provider timing is authoritative; re-voice to replace".
- **The app:** `WordAligner.swift` (§8.1).

### 4.6 Provider transcription: allowed, opt-in, never automatic

**Decision:** `promo words <project|audio> --transcribe openai|elevenlabs` (and MCP `promo_words {"transcribe": "openai"}`).
- It spends the person's key: whisper-1 at about $0.006 a minute, Scribe at about $0.40 an hour. It is not deterministic, and it is never needed for sync to work. So it is a choice a person makes, never something an open does.
- Its result is stored as `transcribed:<provider>`, mapped through §2.5. It is authoritative like provider timing.
- **OpenAI:** `POST /v1/audio/transcriptions`, multipart: `model=whisper-1`, `response_format=verbose_json`, `timestamp_granularities[]=word`, and `prompt=<script>` to bias spelling. The response's `words[]` are `{word, start, end}`. `whisper-1` is the model that returns word timestamps. A small multipart builder is needed, since `ureq` 2 has none.
- **ElevenLabs:** `POST /v1/speech-to-text`, multipart `model_id=scribe_v1`. The response's `words[]` are `{text, start, end, type}`, and `type == "spacing"` entries are skipped.
- Keys come from `KeyStore`, exactly as for synthesis. `check: true` reports the cost estimate first.

### 4.7 Accuracy to expect, and when to warn

| material | word start, median / p95 | sentence boundaries |
|---|---|---|
| synthetic fixture (§10, S5) | ≤ 10 ms / ≤ 20 ms | ≤ 10 ms |
| clean TTS takes (OpenAI, Chirp, Kokoro as a check) | 40–70 ms / ~150 ms | ≤ 40 ms |
| a human read in a quiet room | 60–120 ms / ~250 ms | ≤ 60 ms |
| voice over a music bed, noisy rooms, ad-libs | unreliable | usually fine |

Sentence and comma boundaries are reliable because pauses are clear. Word
boundaries inside connected speech are the weak point: two words with no dip
between them are split by the syllable prior. That is good enough to cut or to
pulse on a word, and visibly loose for a karaoke highlight on fast speech. The
editors therefore show aligned timing as "aligned" (§8.2) and offer
`--transcribe`.

**Warn** (§9) when the take's confidence is under 0.6, or under 0.45 for any
word a value names. Warn too when the VAD's speech time against the script's
estimate is outside [0.6, 1.7] ("the recording may not say this script"), or
when §2.5 called the result a mismatch.

---

## 5. Estimates and placeholders (same format, flagged)

`estimate::estimate_words(script, duration, language, calibration) ->
TakeTiming` produces `source: "estimate"`, `estimated: true` and confidence
0.3:
- `PADDING/2` (0.2 s) of lead-in, and the same at the end
- the remainder split between words in proportion to `word_weights.syllables`
- `pause_after` honoured, scaled by the same factor

**Who writes it**
- `placehold` writes it beside the silent MP3, for the MP3's own length (`silent_mp3_seconds`), so the times fit the file the timeline plays.
- The app's placeholder door gets a twin: `promo_speech_placeholder_write_timed(project_dir, filename, provider, voice, text, project_json)` writes both files and returns the length. The old function stays for the old app.
- A placeholder's record is **replaced** when the real take lands: new bytes, a new key. The old record is removed with the old file, under the existing rule "a silent stand-in this walk wrote under another name is the walk's own file".
- `narration-placeholders` stays true: placeholder ⇒ no receipt. The record is not a receipt.

---

## 6. Speech values in time fields

### 6.1 The value

`promo-model/src/musical.rs` `TimeValue` gains a third arm, and `MusicalTime`
stays as it is:

```rust
pub enum TimeValue { Seconds(f64), Musical(MusicalTime), Speech(SpeechTime) }
pub struct SpeechTime { pub at: SpeechPoint, pub of: Option<String>, pub edge: Option<Edge>,
    pub offset: Option<TimeOffset>, pub quantize: Option<SnapUnit>, pub guard: Option<String> }
pub enum SpeechPoint { Word(u32), WordText { text: String, nth: u32 }, Sentence(u32),
                       Snap { to: SpeechSnap, near: f64 } }
pub enum SpeechSnap { Word, WordEnd, Gap, Sentence }
pub enum Edge { Start, End }
```

**On the wire**, each a strict object:

| form | means |
|---|---|
| `{"word": 12, "of": "intro"}` | word 12 (1-based) of that take |
| `{"word": "Euclid", "of": "intro"}` / `…, "nth": 2` | the first (or nth) word whose stripped, case-folded text is "euclid" |
| `{"sentence": 2, "of": "intro"}` | sentence 2 |
| `{"snap": "word", "near": 4.2, "of": "intro"}` | the word start nearest 4.2 s; `wordEnd`, `gap` (the middle of the nearest inter-word gap, the safe place to cut) and `sentence` likewise |
| `"edge": "start"` or `"end"` | which edge of the word or sentence (defaults in §6.2) |
| `"offset": -0.1` or `{"beats": -0.5}` | added after the edge; seconds or a musical length |
| `"quantize": "beat"` (also `downbeat`, `bar`) | then snapped to the beat grid: "the beat nearest word 12" |
| `"text": "Euclid"` with an index | a guard: if word 12 is no longer "Euclid", resolution follows the guard to the nearest occurrence and warns (§9) |

**Rules:**
- `of` names a resource (a take) or an audio/video layer that plays one (§6.3). It may be left out when the scope has a `speechClock`.
- Decoding is strict, as for musical values: exactly one of `word`, `sentence` or `snap` (with `near`); `nth` only with a text `word`; `text` only with an index; any other key is refused with its path.
- **One object names one clock.** A speech value counts on its take. A `quantize` or a musical `offset` counts on the scope's beat clock, which is the only way the two meet inside one value.
- Encoding is canonical, with integers written as integers.
- Lift, fold, `timeSpecs`, `SetTime` and the editor commands that clear specs are the beat-sync machinery, unchanged. `lift` recognises the new keys and `label()` gains the speech wording.

### 6.2 Which fields accept it

The same fields as musical values (`LAYER_TIME_FIELDS`,
`KEYFRAME_TIME_FIELDS`, the sequence entries and `after.offset`). A speech
value is always a **position**:

| field | a speech position means | default edge |
|---|---|---|
| layer `startTime` | the layer starts there | start |
| layer `duration` | the layer ENDS there ("until sentence 2 ends") | end |
| keyframe `time` | that output instant, made layer-local ("zoom on Euclid") | start |
| `fadeIn`, `fadeOut`, `transition*.duration`, keyframe `transitionDuration` | refused: a length is required (a break; the seconds stand) | — |
| `after.offset` | refused | — |

**New layer field `during`:** `{"sentence": 2, "of": "intro", "lead": 0.15,
"hold": 0.25}` (or `{"word": …}`, or `{"words": [3, 9], "of": …}`) sets
`startTime` and `duration` together. The layer starts at the span's start −
lead and ends at its end + hold. Both entries are recorded as derived
`timeSpecs` (`rule: "during"`). The detach rules are those of `after`: a hand
move drops `during`, and a resize alone becomes a plain duration.

### 6.3 The speech clock

- **`speechClock`**, on `compositionSettings`, on a `Composition` (for its own scope) and on a layer (an override), is `{"layer": "<audio layer id>"}` or `{"resourceID": "<take>", "startTime": 0}`. A value without `of` counts on it.
- **Resolving `of`:**
  - A layer id means that layer.
  - A resource id means the first enabled audio (or video) layer in the scope that plays the resource, by start time. If several play it, validate warns and suggests the layer id.
  - With no layer playing it, `startTime` must be given (as in `beatClock`), or else it is a break.
- **The mapping.** A take's source second s maps to the output timeline through the **inverse of `mapping::source_time_for_layer`**, so trims, `speed` (layer and resource), media cuts, trim keyframes and `beyondEnd: loop` are honoured.
  - The inverse is built as a monotone piecewise map: the forward mapping is sampled at the project frame rate over the layer's window, and split wherever it jumps.
  - A word that falls in no played window (it was trimmed out) is a break: `word 12 "Euclid" is trimmed out of layer "VO"`.
  - On a loop, the first play is meant, with a warning.
- **No default.** Without `of` and without a clock: exactly one take in the scope means that take, with a warning; otherwise it is a break that asks which.
- **Nested compositions** count only on their own clock, as for beats.

### 6.4 Resolution: order, and the beats beside it

`resolve_timing(meta, inputs: &TimingInputs)`, where `TimingInputs { beats:
BeatGrids, words: TakeWords }`. `TakeWords` maps a resource id to its
`TakeTiming`, plus the list of missing takes. As in S2, every caller of
`resolve_timing` is moved, and the compiler finds them.

Changes to BEAT-SYNC §2.2, per scope:

1. **Clocks.** Beat grids as today. **Speech clocks** are built per (scope, clock layer), lazily, because their mapping needs the clock layer's start and trim, which may themselves be derived.
2. **Absolute starts.** Beat positions as today. A speech position resolves here when its clock layer's start is a stated number.
3. **The fixpoint loop** gains:
   - **d.** Speech positions whose clock layer's start became known this round, and `during`.
   - **e.** Clock rebuilds: a beat clock that follows an audio layer, or a speech clock, whose layer start moved this round is rebuilt, and dependents re-resolve.
   - The bound stays the layer count + 1. Whatever is still unknown is `Circular` and keeps its number. Example: the music enters at `{"word": 3, "of": "vo"}` while the voice-over starts at `{"beat": 5}`.
4. Lengths measured from an edge, unchanged.
5. **Generators expand** (beats and speech, §7), then captions (§7.2).
6. **Side entries**, with `resolved` rows carrying `clock: "speech"`, the word index and text, and the timing's `source`.

**Missing timing.** The take is in `missingWords`. Resolution lays out on `estimate_words` over the take's measured duration, with a warning per value.
- An export waits while `missingWords` is non-empty, exactly like `missingGrids` (`PromoTiming.exportWaitsForGrids` becomes `…ForClocks`).
- A placeholder's `estimate` record is timing, not missing, so a placeholder project renders, with validate's existing "exports silent" warning plus "estimated".

**Idempotent**, as before: resolving twice changes nothing, and fold-then-lift changes nothing. Both are pinned by tests.

### 6.5 Rung

A rung-49 reader decodes `{"word": 12}` through `MusicalTime::from_object`,
which refuses the unknown key, so the file cannot be opened. That is the
strict-kind case of `adding-a-wire-field-is-not-always-additive`. **Rung 50
is new**, `SPEECH_READER = 50`.

| a file carrying… | rung |
|---|---|
| a speech value in any time field; `during`; a sequence `until`/`per` entry (§7.3) | 50 |
| reveal `sync`, a speech `on`/`range` in `pulse`/`shake`/`flash`, `captions` (an older reader renders a different film) | 50 |
| `speechClock` alone (ignorable: with no speech value, nothing renders differently) | 48 |
| `transcript`, `speech.wordFixes` (ignorable) | 48 (`LOSSLESS_READER`) |
| none of these | unchanged |

- `READER_VERSION` = 50 in `promo-model/src/lib.rs`. `ProjectStore.currentFormatVersion` and `supportedFormatVersion` = 50. The pin `the_apps_reader_version_is_the_cores` moves them together.
- schema.md keeps its single version statement (`one-version-no-compat-docs`).
- The `.words/` cache is not part of `metadata.json` and carries no rung.

---

## 7. Captions, generators and sequences on speech

### 7.1 A caption reveal on the take's real word times

`captionStyle.reveal` (and `compositionSettings.subtitleReveal`) gains
`"sync": {"of": "intro"}`, with optional `"words": [5, 12]` or `"sentence": 2`
to pin the span.

**Mapping the caption's units to the take's words** (step 5, after the
layer's times are final):
- Tokenize the caption text (§1.2) and run a **local** version of the §2.5 DP. It finds the best-matching run of the take's words: a caption usually shows one sentence of a longer narration, so the match is a substring.
- A stated `words` or `sentence` range skips the search.
- With `by: "word"`, each caption unit gets its matched word's start. A punctuation-only unit gets the next word's start (the previous word's end, at the end of the line).
- With `by: "character"`, a word's characters share out its span by grapheme count.
- With `by: "line"`, a line takes its first word's start.
- An unmatched unit is interpolated, and validate warns (§9).

**The resolved answer is a new, derived field:** `reveal.unitTimes: [f64]`,
the arrival of each unit in **layer-local seconds**, plus `reveal.unitEnds`
(the spoken end of each unit, for highlight).
- Like `timeSpecs`, both are carried in the app form and stripped by `fold`, so they never reach a file.
- They are typed fields in Rust `TextReveal` and Swift `TextReveal`, a synthesized Codable struct, so adding them is two optional properties.
- `sync` itself is unread by Swift until S9, and `keep_unread` restores it.

**The engine change** (`reveal.rs::progress`), the only engine change in this design:
- With `unitTimes` present:
  - `shown` is the count of units whose time is ≤ the local time. At least one unit shows once the first time has passed, so the "never empty" rule holds from the first word.
  - `active` is the last arrived unit while the time is before its `unitEnds`. A karaoke highlight goes out between words and at the end, instead of sticking to the last word.
  - `fraction` is shown ÷ units.
  - The staggered modes start each unit's arrival at its own time and keep `unitSeconds`.
- Without `unitTimes`: the existing code path, byte for byte (AC4).
- `unitTimes` reaches all three hosts (the preview, the video export, the image export) as part of the layer model the app already encodes. That is the reason it is a model field and not a host input (`engine-inputs-three-paths`). The parity tests gain a synced-caption case (S7).

### 7.2 Captions generated from a narration

**New, on an audio or video layer that plays a take:**

```json
"captions": { "by": "sentence", "maxWords": 8, "maxSeconds": 3.5, "lead": 0.1, "hold": 0.25,
              "reveal": {"by": "word", "mode": "highlight", "highlightColorHex": "@accent"},
              "style": { …captionStyle… } }
```

- **Expansion** (step 5, `promo-timeline/src/speech_effects.rs`):
  - Sentences longer than `maxWords` or `maxSeconds` are split at the comma or word gap nearest the middle, recursively. `by: "phrase"` splits at every comma or semicolon first.
  - Each piece becomes a generated caption layer `speech-captions-<layer>-<k>`. Its `captionText` is the script slice (from `chars`). It starts at the first word's start − lead and ends at min(the last word's end + hold, the next piece's start). It carries the style and a reveal with `sync`, so `unitTimes` fill in.
- **Ownership** is the flash generator's (BEAT-SYNC §4 and the S6/S9 notes):
  - `generatedBy: "captions:<layer>:<sig>"`.
  - An unedited caption is stripped at save and regenerated, so a new take re-times all of them.
  - An edited caption becomes the person's, and the generator skips its span.
  - A deleted generated caption adds its index to `captions.skip` at fold, so the delete sticks.
- The ids are one namespace with every other id (the S9 lesson: the drawing id collision).

### 7.3 Generators firing on words; one slide per sentence

**Generators.** `pulse`, `shake` and `flash` take speech events in `on`/`every` and speech spans in `range`:

```json
"pulse": {"on": {"words": ["Euclid", "free"], "of": "intro"}, "property": "viewport", "amount": 0.05,
          "decay": 0.25}
"flash": {"on": {"sentences": "start", "of": "intro"}, "opacity": 0.4, "duration": 0.12}
"shake": {"on": {"words": "emphasised", "of": "intro", "top": 3}, "amplitude": 6}
```

- `words` takes `"all"`, a list of indices, a list of texts (every occurrence) or `"emphasised"` with `top`/`above` on `prominence` (§8.2). `sentences` takes `"start"` or `"end"`.
- `range` takes `{"sentence": 2, "of": …}` or `{"words": [3, 9], "of": …}`.
- Expansion is the beats code with a different point list: grid points are replaced by word times mapped to output time.
- Ids are `speech-<layer>-pulse<n>-<k>-<role>`. The tag is `auto: "speech:<sig>"`. `musical::strip_generated` and `is_unedited_beat_keyframe` generalise to the `beat:` and `speech:` tags.
- Decay and duration may be seconds or beats (they still count on the beat clock).

**Sequences.**
- A sequence entry gains a length form `"until": <speech or musical position>`. The entry ends there, and the next entry starts there.
- A sequence entry may also be a **per** entry:
  ```json
  {"layers": ["s1", "s2", "s3", "s4"], "per": "sentence", "of": "intro", "lead": 0.1}
  ```
  It expands to one entry per layer. Slide k lasts until sentence k+1's start − lead, and the last slide until the last sentence's end + `hold` (default 0.5 s). This is "one slide per sentence".
  - Fewer layers than sentences: the last slide holds through the rest, with a warning. More layers: the extras get no slot, with a warning.
- The first entry's start is its own (as in BEAT-SYNC's S3 notes). A start of `{"sentence": 1, "of": …, "offset": -0.1}` lands the first cut just before the voice.
- `quantize: "beat"` on `per` puts each sentence cut on the nearest beat, for music and voice together.

---

## 8. The apps

### 8.1 Storing and computing timing (core-private FFI, shared Swift)

**At synthesis:**
- The FFI job API gains `promo_speech_job_timing(job) -> char*`: the `Spoken` JSON, or NULL.
- `SpeechSynthesizer.generate` writes the take, then calls `promo_words_store_take(project_dir, filename, script, job)`. The core hashes the file, maps the timing to the script (§2.5) and writes the record.
- An OpenAI take, a Chirp take, or a store that failed lands in `missingWords` and is aligned by the next item.

**`WordAligner.swift`** (ReVoice/Services, both targets) has the `BeatAnalyzer` shape:
- AVAudioFile, decoded in chunks and pushed interleaved to `promo_word_aligner_new(channels, rate, script_json)`, `_push`, `_finish` and `promo_words_store`.
- It runs off the main thread, on the `missingWords` from `promo_project_opened` (whose envelope gains `missingWords` and `timingProblems` with speech cases), and after any re-take.
- It then reconciles, in the order the S8 notes fixed: the model first, then the document.

**Other FFI:**
- `promo_words_lookup(project_dir, filename, script)` returns the record or NULL.
- `promo_doc_set_project_dir` already exists; `Document::set_takes` stores `TakeWords` for every later `apply`, like `set_beat_grids`.
- Exports run `WordAligner.align(in:)` before their guard, so only a take that cannot be aligned holds an export.

**The app never computes a word time** (`swift-twin-is-not-the-renderer`,
`parity-comment-without-test`). Swift draws what the core answers.

### 8.2 UI (shared views, by size class)

- **The narration resource inspector** gets a "Words" section:
  - The status reads: "Timed by ElevenLabs · 42 words · 7 sentences", "Kokoro (local) timing", "Aligned · confidence 0.71" (in amber under 0.6, with "Transcribe with OpenAI…" when a key exists, showing its cost), "Estimated (placeholder)" or "Aligning…".
  - Actions: Align again, and Transcribe…
  - The section shows the script with each word tappable. A tap selects the word and offers "Start selected layer here", "End selected layer here" and "Copy reference".
- **The timeline:** the narration lane draws word boxes over the waveform and sentence brackets, all in output time, from `promo_project_speech_ticks(json, dir)`, a twin of `beat_ticks`.
  - Dragging a layer edge near a narration snaps to word starts and ends, and gaps are preferred when ⌥ is held. With a beat clock as well, beat snapping wins unless the edge is within 2 frames of a word gap.
  - Every snap goes through the core: `promo_project_speech_snap(json, dir, layer, edge, seconds, of)` returns `{"word": n, "text": "…", "of": id}`, which `applyCoreStep` writes with `setTime`, as one undo step.
- **The layer editor's Start and Duration rows** show the core's label: "word 12 ‘Euclid’ · 4.380 s", "ends with sentence 2 · 9.120 s". Menus: "Start at word…" (a searchable list), and "Snap Start/End to word / gap / sentence".
- **The caption inspector's Reveal section** gets "Follow narration" (a picker of takes), with a matched-span readout: "words 5–12 of Intro · 2 unmatched".
- **App MCP:** `promo_words` is a twin of the headless tool. It reads the cache, else runs `WordAligner`, and returns the core's JSON. `ServerContractTests` lists it on both servers.

---

## 9. validate, explain, inspect

**Findings,** with the existing prefixes, as `warn` or `break`:

| finding | severity |
|---|---|
| `layer "s3": startTime {"word":"Eucld","of":"intro"} — no word "Eucld" in "intro" (closest: word 2 "Euclid")` | break |
| a word index past the end, a `nth` past the last occurrence, a sentence past the end | break |
| `of` names an unknown resource, a non-audio resource, or a take nothing plays and no `startTime` places | break |
| the word is trimmed out of its clock layer | break |
| a speech position where a length is required (`fadeIn {"word":3}`) | break |
| `during` together with `sequence`, `after` or `timing.start` (who wins: sequence, then during, then after, as in S3) | break |
| a guard moved: `word 12 is now "the"; followed "Euclid" to word 14 — update the reference` | warn |
| `"intro" has no word timing yet — laid out on an estimate` (app or `--no-analyse` only) | warn |
| `"intro" timing is estimated (placeholder)`, once per take | warn |
| `"intro" was aligned with confidence 0.42 — word starts may be ±0.2 s; transcribe or fix by hand` | warn |
| `"intro"'s text changed since its take was voiced — times follow the old take` | warn |
| `the recording "vo.m4a" may not say its transcript (speech 14.2 s, the script reads about 31 s)` | warn |
| `layer "s4" at 6.31s: cuts mid-word "proved" (6.18–6.52 s of "intro") — the nearest gap is 6.55 s`. Checked for every visual edge in a scope that uses speech sync anywhere, against takes playing at that instant, ±1 frame | warn |
| a caption reveal `sync` with unmatched units, or one matching under 50% of its words | warn |
| a resource played by several layers named by resource id | warn |
| `the take is longer than its layer — words 31–40 are never heard` | warn |
| CJK or Thai scripts: `word sync for "intro" is by character` | note |

**`promo validate`.** Text mode adds:

```
Speech times
  layers[s3].startTime  {"word":12,"of":"intro"}    → word 12 "Euclid" at 4.380 s  (elevenlabs)
  layers[s4].duration   {"sentence":2,"of":"intro"} → ends with sentence 2 at 9.120 s  (aligned 0.72)
  layers[s5].startTime  per sentence 3 of "intro"   → 9.020 s
```

`--json` adds `resolvedTimes` rows with `"clock": "speech"`, `"word":
{"index", "text"}` and `"source"`, and a `speechClocks` key (scope, take,
layer, source, confidence, words, sentences, estimated, stale).

**`promo explain`.** `times` lists speech values in both forms. `generated`
counts speech keys and generated captions. A synced caption shows
`"reveal": {"sync": …, "units": 9, "matched": "words 5–12"}`.

**`promo inspect`.**
- A `words` section per take: file, source, confidence, the counts of words and sentences, coverage (the share of the take's speech covered by words), the first and last word, stale or not, and any fixes.
- Each layer's speech fields in both forms.
- The `credits` block gains transcription spend with `provenance` (`{"tool": "openai-whisper", "cost": …}` on the resource).

**CLI `promo words <audio | project> [--resource id] [--script text | --script-file f] [--json] [--timeline] [--refresh] [--transcribe openai|elevenlabs] [--no-analyse]`:**
- On an audio file, it aligns with the given script. On a project, it uses the named take, else the speech clock's, else the only one.
- It prints a 1-based table (`#  start  end  conf  word`) and the sentences.
- `--timeline` maps the times to the output timeline through the clock layer.
- `--json` is the record, plus `fromCache` in project mode.

**MCP `promo_words`** is headless and in the app (§8.2), with the same
arguments. The tool-surface ceiling (`the_tool_surface_stays_small`) rises
for it, as it did for `promo_beats`.

---

## 10. Build order

Each slice compiles, passes `check-all.sh` (and both Xcode suites when Swift
is touched), and is committed on its own. **AC** marks the owner's acceptance
criteria.

**S1 — Format, tokenizer and cache (core).**
- Build: `promo_model::speech` (the types and `tokenize`), `promo_model::media_cache` (split from `beat_cache`), `word_cache`, the `transcript` and `speech.wordFixes` fields (rung 48), and the schema JSON.
- `promo words <audio> --script` prints the estimate only.
- Tests:
  - `tokenizer_words_are_reveal_units`: against promo-text's word units over the caption fixtures
  - `chars_are_scalar_offsets`: emoji, combining marks, Cyrillic stress
  - `sentences_skip_abbreviations_and_decimals`
  - `ssml_words_come_from_text_nodes`
  - `cache_key_is_audio_and_script`
  - `stale_take_finds_its_old_record`
  - `transcript_and_word_fixes_stamp_48`
  - `unread_sweep` stays green

**Slice 1 as built** (2026-10-21):
- **Modules.** `promo_model::speech` (the record, `tokenize`, `script_hash`,
  `WordFix`, `apply_fixes`, `take_script`), `promo_model::word_cache`
  (`cache_path`, `lookup`, `store`, `records_for`, `takes`,
  `takes_needing_timing`, `choose_take`, `words_answer`) and
  `promo_model::media_cache` (`MediaSource`, `source_of` with its memo,
  `content_key`). `musical::BeatSource` is now an alias of `MediaSource` —
  the same JSON, so the beat cache and its goldens are unchanged;
  `beat_cache::source_of` delegates.
- **`estimate_words` moved into S1** (with `word_weights`), because
  `promo words` prints it. `WordWeight` has a third field, `pause_within`
  (a pause inside a word, e.g. an inner dash), so the per-word weights sum
  to the whole-script count exactly (`estimate_words_sum_matches_estimate`).
  The words are read as the whole script reads (one English/script
  decision), not word by word. An estimate record carries
  `alignerVersion: 1` (`ESTIMATE_VERSION`).
- **The tokenizer** exposes the reveal units (`Script::units`: every
  whitespace run, tags counting as whitespace in SSML) beside the words;
  each word carries its unit, its stripped `chars`, its `token` span with
  margins (what a sentence spans) and, in SSML, the `<` of the outermost
  `<say-as>`/`<sub>`/`<phoneme>` it sits in (`atomic`, for S3's marks).
  An SSML entity (`&amp;`) is one character of a word. Additions to §1.2:
  `。`, `！` and `？` end a sentence wherever they stand; closing quotes and
  brackets may stand between a terminator and its whitespace; a
  terminator the next word leans on (`costs .5`) ends nothing (found by a
  break-test); the abbreviation list adds "Mrs." and "Ms.". A Thai word is a
  base character with its marks.
- **`transcript`** is a typed `ProjectResource` field (in the machine
  schema); the apps' model does not read it, so `unread::carry_field`
  keeps it through their save. **`speech.wordFixes`** stays inside the
  `speech` bag, and each fix may carry its own `scriptHash`. Both stamp 48
  (`speech::carries_ignorable_speech_fields`). `speech` is a key the apps'
  model reads and writes, so the Swift `SpeechSpec` gains `wordFixes` with
  S2's app change; until then an app save drops it.
- **`takes_needing_timing`** is every take (audio or video with
  `speech.text` or a `transcript`) whose file is there with no record at
  all; S6 narrows it to the takes a value needs.
- **`promo words`** (S1 form): on a project, the named take or the only
  one; its record (`fromCache: true`, `stale: true` when it follows an
  older script), else an estimate over the take's measured `duration` (or
  ffprobe's), not stored (`fromCache: false`). The take's `wordFixes` are
  applied, and what was ignored is `fixNotes`. On a file, `--script` or
  `--script-file` is required and the answer is an estimate. `--timeline`,
  `--refresh`, `--transcribe` and `--no-analyse` come with the aligner (S5).
  `promo-cli` now links `promo-speech` without its keyring.
- **Docs.** No schema.md entries yet: nothing renders from the new fields
  until S6/S7 (the same choice as BEAT-SYNC's slice 1); the kitchen sink
  leaves `transcript` unset.
- **Tests:** `tokenizer_words_are_reveal_units` lives in
  `promo-text/tests/speech_units.rs` (promo-text depends on promo-model, so
  the comparison is the real reveal layout over every caption in
  `examples/`, `demos/` and `fixtures/`); the rest are in `speech.rs`,
  `word_cache.rs`, `promo-model/tests/speech.rs`, `estimate.rs` and
  `promo-cli/tests/words.rs`.

**S2 — Estimates and placeholders.**
- Build: `estimate::word_weights` and `estimate_words`; `placehold` writes records; the FFI `promo_speech_placeholder_write_timed`; the Swift placeholder path calls it.
- Tests:
  - `estimate_words_fill_the_placeholder_exactly`: the last word ends at duration − 0.2
  - `estimate_words_sum_matches_estimate`
  - `placeholder_record_is_flagged_estimated`
  - `real_take_replaces_placeholder_record`
  - Swift: `testPlaceholderWritesWordTiming`
  - **AC6** (estimates are in the same format, flagged)

**Slice 2 as built** (2026-10-21):
- **One placeholder door.** `promo_speech::placeholder_take(resources_dir,
  filename, provider, voice, text, doc)` writes the silent MP3 and, in the
  `.words` cache beside it, `estimate_words` for exactly that file's length
  (`silent_mp3_seconds`). `placehold` (headless) and the FFI's
  `promo_speech_placeholder_write_timed` both call it. A record that cannot
  be stored is a report line (stderr on the FFI), never a failed stand-in.
- **The FFI takes the resources folder**, not the project folder:
  `promo_speech_placeholder_write_timed(resources_dir, filename, provider,
  voice, text, project_json)`. `SpeechSynthesizer.generate` knows only the
  folder it writes takes into (in the tests, a scratch folder with no
  project around it), so `word_cache` gained `*_in(resources)` variants of
  `cache_dir`, `cache_path`, `records_for`, `lookup` and `store`; the
  project forms call them with `<project>/Resources`. A name that is not
  one plain component is refused (-1).
- **Retiring records.** Whenever a write replaces a file's bytes — a
  stand-in re-estimated, a real take over a stand-in under its own name or
  under another, a re-voiced take — the records of the old bytes go
  (`word_cache::retire_in`), except those under a script another resource's
  file with the same bytes still says: two stand-ins of one length are
  byte-identical, so one audio key. Without the project (the FFI given no
  `project_json`) nothing is retired; a leftover record is keyed by bytes
  no file has.
- **The apps.** `SpeechSpec.wordFixes` (`[SpeechWordFix]`, carried, no
  editor yet) closes slice 1's gap: `speech` is the app's key, so the
  field must be in its model to survive a save. The placeholder path calls
  the timed door; the untimed `promo_speech_placeholder_write` stays for
  older builds.
- **Tests:** `placeholder_record_is_flagged_estimated`,
  `real_take_replaces_placeholder_record` (the byte-identical sibling kept),
  `the_timed_placeholder_door_writes_the_estimate_beside_the_take` (FFI);
  Swift `testPlaceholderWritesWordTiming`,
  `testWordFixesSurviveAResourceRoundTrip`. `estimate_words_*` landed in
  S1.

**S3 — Provider timing (ElevenLabs, Google) and the script mapper.**
- Build: `marking_stress_mapped`, the `Synth::synthesize_timed` default, `synthesize_with_key_timed`, `timing::attach`, settle storing records before persisting receipts, `google_supports_marks`, and the FFI `promo_speech_job_timing` and `promo_words_store_take`.
- Fixtures: committed provider answers (`fixtures/speech/elevenlabs-with-timestamps.json`, `google-timepoints.json`) and their MP3s. No live calls in tests.
- Tests:
  - `stress_map_matches_marking_stress`
  - `elevenlabs_alignment_maps_to_script_chars`, including stress-marked Russian
  - `elevenlabs_char_mismatch_falls_back_to_dp`
  - `google_ssml_has_one_mark_per_word_and_escapes`
  - `google_ssml_script_marks_text_nodes_only`
  - `google_missing_timepoints_falls_back_to_alignment`
  - `chirp_voices_take_the_plain_request`
  - `attach_merges_spelled_numbers` ("$49.99" to 6 spoken words)
  - `timing_is_stored_before_the_receipt`
  - Then one manual live check per provider through the Xcode-run app's MCP (`speech-tests-through-the-xcode-run-app`), recording the measured offset of word 1 against the energy onset (the MP3 priming risk, §11.1).

**Slice 3 as built** (2026-10-21):
- **Types.** `promo_speech::timing::{Take, Spoken}`. `Take` has a `note`:
  why a take that could have been timed was not (a report line, never an
  error). `Spoken` carries its `confidence`, and `against` is the text the
  synthesizer was GIVEN, which its words' `chars` index (the design's
  `SentText`). The `Synth` default, `synthesize_with_key_timed`, and
  `synthesize_with_key` = `…_timed(..).audio` are as §2 says.
  `ProjectSynth` (promoshot-mcp) forwards `synthesize_timed`; the trait
  default would have dropped the timing.
- **ElevenLabs.** The answer's `characters` are flattened to scalars,
  because a vowel and its acute may come back as one string. They are
  compared with the sent text: equal means one for one (confidence 1.0);
  otherwise a Levenshtein alignment with backpointers runs (accent-folded
  equality, ties to the diagonal, confidence 0.9). Then they go through
  `marking_stress_mapped` to script characters. A word spans its first
  character's start to its last non-space character's end. No usable
  alignment: the audio is kept, and the note says the take will be aligned.
- **Google.** `google_supports_marks` needs a locale-prefixed name with a
  fourth segment and a class in the list (Standard, Wavenet, Neural2,
  News, Polyglot, Studio). `google_ssml_marked` builds the SSML from the
  stress-converted text, XML-escaped, with a mark before each word's first
  stripped character (`<now>` is a word, so its mark comes before
  `&lt;`). In an SSML script, marks go before words in text nodes, and
  once before a `say-as`/`sub`/`phoneme` element whose other words share
  it (and are interpolated). A word ends at the next mark's time and the
  last word at the take's end. **The pull-back to the end of voiced energy
  waits for the aligner's VAD (S5)**, so until then a word's end includes
  the pause after it. A 400 that names SSML, marks or time pointing is
  retried once as the plain v1 request, with a note. Fewer timepoints than
  marks: the audio is kept, there is no record, and the note says so.
  Whether marks are billed is still unverified.
- **The mapper** (`timing::record`). The chars path runs when `against`
  is the script, or the script with its stress converted (the apps send
  that). Otherwise the §2.5 word alignment runs, with one addition: a 1:k
  merge costs 0 when the spoken words concatenate to the script word (a
  compound said apart). Open ends are filled with the next start, else
  the take's end. Times are clamped monotonic. Unvoiced runs share their
  gap by letter count. A word's confidence 0.3 is written only where it
  differs from the take's.
- **Settle** stores the record before `persist` writes the receipt
  (`timing_is_stored_before_the_receipt`). A store that fails is a report
  line, and the receipt still stands. The report says "timed by
  provider:elevenlabs (42 words)", or gives the note.
- **The apps.** The FFI is `promo_speech_job_timing(job)` (Spoken JSON or
  NULL) and `promo_words_store_take(resources_dir, filename, script,
  spoken_json, duration)`. It takes the timing JSON rather than the job
  (the job is freed inside the synthesis wrapper), the resources folder
  (as in S2), and the measured length. Swift: `SpeechProvider.
  synthesizeTimed` (the default answers no timing; the three core-backed
  providers answer the core's), `PromoCore.synthesizeTimed`,
  `wordsStoreTake`. `SpeechSynthesizer.generate` stores the record before
  it returns, so before any caller keeps the receipt. The app's
  `promo_speak` goes through `generate`, so it times too.
- **Fixtures** (`fixtures/speech/`): `elevenlabs-with-timestamps.json`,
  `elevenlabs-stress-ru.json` and `google-timepoints.json`, with their
  MP3s. They are **built in the providers' documented answer shapes, not
  recorded**: no key is readable headlessly here (`promoshot-mcp key
  status`: none for all three), and the keyed path is the Xcode-run app,
  which runs this code only after its owner rebuilds it.
  `make_fixtures.py` regenerates them (each MP3 has a tone exactly where
  its words are).
- **Not done: the live checks.** That means one line per provider through
  the Xcode-run app's MCP, the word-1 offset against the energy onset
  (§11.1, MP3 priming), and whether marks are billed. They wait for a
  keyed run.
- **Tests:** `stress_map_matches_marking_stress`,
  `elevenlabs_alignment_maps_to_script_chars` (Russian included, and the
  apps' pre-converted text), `elevenlabs_char_mismatch_falls_back_to_dp`,
  `character_alignment_survives_drops_and_inserts`,
  `google_ssml_has_one_mark_per_word_and_escapes`,
  `google_ssml_script_marks_text_nodes_only`,
  `google_missing_timepoints_falls_back_to_alignment`,
  `chirp_voices_take_the_plain_request`, `attach_merges_spelled_numbers`,
  `unvoiced_words_interpolate_and_a_different_text_is_a_mismatch`,
  `a_near_spelling_wins_over_a_filler`,
  `stress_marked_chars_map_onto_the_script`,
  `timing_is_stored_before_the_receipt`,
  `the_tool_keeps_a_providers_word_timing` (promoshot-mcp),
  `a_takes_timing_is_stored_through_the_doors` (FFI); Swift
  `testSynthesisStoresProviderTiming`.

**S4 — local-tts v0.7.0 and local timing** (repo `/Users/alex/src/tts`; tag, then bump the pin).
- Build: §3.2 items 1–5 in local-tts; the §3.5 tests there; then `local::synthesize_timed` in promo-speech.
- Tests:
  - `local_take_writes_local_timing`
  - `lexicon_edit_retimes_local_take`
  - **AC3**: `local_timing_agrees_with_alignment`. Over 20 corpus lines with Kokoro q8 and Kitten, word starts within 60 ms median and 120 ms p95 of the S5 aligner, and every sentence boundary within 40 ms. It runs where the packages are installed and skips with a note otherwise. It lands in S5 when the aligner exists; S4 lands `durations_sum_to_samples` and `timed_audio_equals_untimed_audio`.

**Slice 4 as built** (2026-10-21) — local-tts v0.7.0
(https://github.com/GarAlex/local-tts/releases/tag/v0.7.0, commit 61efef0):
- **§3.2 as written, with these differences.**
  - The extra Kokoro output is an `Identity` node from
    `/encoder/Clip_output_0` to a new output `durations` (float), appended
    to the graph with its `ValueInfoProto`. The tensor itself stays where
    it was.
  - The patched session's audio is **bit-identical to the unpatched
    file's** on F32, Q8 and Q4 (`timed_audio_equals_untimed_audio` loads
    both sessions; `Kokoro::load_unpatched` is a hidden test door). Risk 3
    did not happen.
  - Measured: 600 samples a frame on every Kokoro export, and on Kitten.
    Timing still uses each chunk's exact samples ÷ frames.
- **Kitten is not bit-reproducible, before or after.** Its graph draws
  noise, so two plain calls differ sample by sample (they already did in
  0.6). What its test pins is the same length and the same words on every
  call. The design's "byte-identical for every family" holds for Kokoro
  only.
- **Spans.** `PhonemeWord { spoken, range }` (byte ranges in the phoneme
  string, after the trim and the IPA vowel expansion).
  `chunks_with_offsets` places each chunk. A word that two chunks split (a
  spelled acronym's letters may be split) is merged back by its index.
  Inflect answers "timing not supported for Inflect".
- **PromoShot.** `promo-speech` pins `tag = "v0.7.0"`.
  `local::synthesize_timed` falls back to the untimed audio when a voice
  cannot time itself. `synthesize_local_timed` gives `Spoken` with
  `source: local:<voice>`, confidence 0.97 and no chars, so the §2.5 word
  alignment maps the engine's normalized words. `LiveSynth` and the MCP
  server's `ProjectSynth` (with the project's lexicon) call it.
- **Tests:** in local-tts, `crates/tts-core/tests/timing.rs` (ignored
  without packages, as the repo's model tests are) and unit tests for the
  offsets, the patch and the spacing map. In promoshot-mcp,
  `local_take_writes_local_timing` and `lexicon_edit_retimes_local_take`.
  These run where kokoro-en-q8 is installed (`PROMOSHOT_VOICES`) and skip
  with a note otherwise; here they ran on APFS clones of the local-tts
  repo's own packages. AC3 lands with S5.

**S5 — The aligner, the CLI and MCP.**
- Build: `promo-media/src/dsp.rs` (shared with beats; the beats goldens stay byte-identical), `words.rs`, `Project::open_with` aligning missing takes, the full `promo words`, MCP `promo_words`, `--transcribe` (multipart, opt-in, with `check`).
- Fixture: `fixtures/speech/synthetic-60w.wav`, made by `tests/support/speech_synth.rs`. Each syllable is a voiced burst (a 140 Hz harmonic stack under two formant bumps, with an attack and release envelope), with 5–40 ms gaps between some words, 300 ms at commas and 600 ms at sentence ends. The truth is known.
- Tests:
  - **AC5** `aligner_finds_synthetic_words`: every start within 20 ms of the truth
  - `aligner_matches_its_golden` (macOS and the Linux Docker image, debug and release)
  - `chunked_push_equals_one_push`
  - `aligner_warns_when_audio_does_not_say_script`
  - `cache_is_authoritative_for_provider_records`
  - `older_aligner_records_are_realigned`
  - `promo_words_json_shape`
  - **AC7** `tools_list_includes_promo_words`, `promo_words_returns_the_record`
  - `transcribe_is_never_automatic`
  - AC3 (see S4)

**Slice 5 as built** (2026-10-21):
- **`promo-media/src/dsp.rs`.** The beat tracker's resampler (now taking a
  target rate), FFT, downmix and PCM readers moved out unchanged; the beat
  goldens are byte-identical. `promo-media` links `promo-speech` (no
  keyring) for the script's words and syllables.
- **The aligner** (`promo-media/src/words.rs`, `ALIGNER_VERSION` = 1, kept
  in `promo_model::speech` so readers that never align know it). §4 with
  these differences, each measured:
  - **The detector is relative to the voice, not only to the floor.** A
    synthesized take's floor is digital silence (−120 dB), so the floor is
    max(10th percentile, 90th − 50 dB). Speech is loud (floor + 12 dB and
    within 28 dB of the speech level), or voiced (floor + 6 and within
    34 dB): Kitten's breaths and pauses are noise only 15–30 dB under its
    voice. There is **no hangover**: it held every word 50 ms past its end.
  - **The length prior** is syllables + 0.25 × letters, with function
    words at 0.6. A mid-vowel cut costs 1.0 (not 0.6); pauses cost −1.0
    after punctuation, +1.0 elsewhere (not ±0.4); a word never begins or
    ends inside a pause (0.15 a frame). Nuclei at 1.0 instead of 0.5, and
    a spectral-flux edge cue, were tried and made agreement worse, so
    they are out (there is no flux feature).
  - **Edges go onto the waveform:** within ±45 ms of the programme's edge,
    the nearest crossing of 18 dB under the local peak of a 10 ms centred
    RMS (a 4 ms window rippled at the pitch period). An edge with a plain
    attack or release (15 dB or more) has confidence 0.95; one inside
    connected speech gets the programme's margin, 1 − e^(−Δ).
  - A recording whose speech time is outside [0.6, 1.7] of the script's
    estimate gets `extra.warnings` and a confidence of at most 0.3. A take
    too short for its words is spread evenly, confidence 0.1.
- **Measured accuracy.**
  - Synthetic truth (`fixtures/speech/synthetic-60w.wav`, 60 words):
    starts median 1.9 ms, p95 2.5 ms, worst 2.8 ms; every end within
    30 ms (AC5).
  - The provider fixtures' MP3s (a tone per word, decoded by ffmpeg):
    starts within 3–5 ms.
  - The golden matches on macOS (debug and release) and in
    `rust:1.96-bookworm` (linux/arm64, release).
  - **AC3** (20 lines, local timing against the aligner on the same
    audio): Kokoro Q8 start median 31.5 ms, p95 129.5 ms, worst sentence
    boundary 96.5 ms. Kitten start median about 32 ms, p95 170–190 ms,
    worst sentence boundary about 245 ms (its waveform draws noise, so it
    varies run to run). **The medians meet AC3's 60 ms; its p95 of 120 ms
    and its 40 ms for sentences are not met.** The test asserts the median
    and guards the rest just above the measured level. What remains is
    boundaries inside connected speech, where neither side is ground
    truth: of the disagreements read against the waveform, some were the
    aligner's mistake (an unvoiced /z/ given to the next word) and some
    the predictor's (onsets after a pause); only a few were read.
- **Local timing is snapped onto the voice (a change to S4).** The
  predictor's frames are the model's alignment, not what is heard.
  Kokoro holds a sentence's last word 190–310 ms into silence (its final
  lengthening) and starts a word after a pause 20–80 ms late, measured
  against the waveform. So `local::snap_to_voice` moves any edge that
  borders silence to the voice's own onset or release (the first or last
  10 ms window within 28 dB of the take's peak, after or before 30 ms
  under it). Edges inside connected speech keep the prediction. Before
  the snap, AC3's sentence boundaries were 307 ms apart; after it, 97 ms.
- **The cache's rule is one function**, `word_cache::usable`: authoritative
  timing stands, and `refresh` on it is refused; an aligned or estimate
  record from the current version, or a stale one, stands; anything else
  is aligned again. `words::timing_for` (CLI, open) and the FFI's
  `promo_words_lookup` (apps) both use it.
- **The CLI's open** (`Project::open_with`) aligns every take whose file
  is there and that has no usable record, and stores the record (as it
  measures beat songs). S6 will narrow this to the takes a value needs.
  `--no-analyse` reads the cache only. Takes still without timing are
  listed in `Project::missing_words`.
- **`promo words`**:
  - **Project:** the cache's answer; else the take is aligned and stored
    (`fromCache: false`). `--refresh` aligns again. `--no-analyse` reads
    the cache, else answers an estimate (not stored).
  - **File:** aligned against `--script`.
  - `--timeline` maps through the first enabled layer that plays the take
    (`promo_timeline::speech`, shared with the apps).
  - `--transcribe openai|elevenlabs [--check]`: the CLI reads keys from a
    secrets file only (it links no keyring); the headless MCP server
    transcribes in-process with its keyring. Neither hands a key to a
    child process.
  - `--prune` removes records whose audio no file has.
  - Not built: `promo words --timeline` for nested compositions (S6's
    speech clock).
- **Transcription** (`promo_speech::transcribe`): a hand-built multipart
  body, whisper-1 with word timestamps and the script as the prompt, and
  Scribe with its spacing entries skipped, both mapped by
  `timing::record`. The record keeps `cost` in `extra`. Provenance on the
  resource (§9's credits) waits for S6/S7. **No live transcription was
  made:** no key is readable headlessly. The fixtures
  (`openai-transcription.json`, `elevenlabs-scribe.json`) are built in
  the documented shapes.
- **MCP `promo_words`**, on both servers (one contract entry, so no host
  difference). The tool surface ceiling is now 47,000 bytes (28 tools).
  - **The app's twin:** AVFoundation decodes (`WordAligner.swift`, the
    `BeatAnalyzer` shape) and the core does the rest. The FFI is
    `promo_word_aligner_{version,new,push,finish,free}` and
    `promo_words_{plan,lookup,store,timeline,transcribe_cost,transcribe}`.
    The app transcribes with its own Keychain key, its cost said first.
  - In file mode the app answers the record with an empty `audio`. The
    CLI fills `audio` in file mode, so the two answers differ there.
  - The skill (and the app's bundled copy) teaches `promo_words`.
- **Tests:**
  - promo-media: `aligner_finds_synthetic_words` (AC5),
    `aligner_matches_its_golden`, `chunked_push_equals_one_push`,
    `aligner_warns_when_audio_does_not_say_script`,
    `aligner_finds_the_provider_fixtures_words`,
    `local_timing_agrees_with_alignment` (AC3, where voices are
    installed).
  - promo-cli: `promo_words_json_shape` (AC7),
    `cache_is_authoritative_for_provider_records`,
    `older_aligner_records_are_realigned`,
    `the_open_aligns_takes_with_no_timing`, `transcribe_is_never_automatic`.
  - promoshot-mcp: `tools_list_includes_promo_words` and
    `promo_words_returns_the_record` (AC7),
    `transcription_spends_only_when_asked_and_is_stored`.
  - promo-speech: the transcription and snap tests.
  - FFI: `ffi_aligner_matches_cli_alignment`,
    `the_words_doors_follow_the_clis_rules`.
  - Swift: `testAlignerMatchesCoreGolden`,
    `testAlignmentWritesTheCoreCache` (both suites),
    `testPromoWordsAlignsCachesAndAnswers` (Mac).

**S6 — Speech values, the clock and resolution (rung 50).**
- Build: `TimeValue::Speech` with lift, fold and labels; `speechClock`; `during`; `TimingInputs` with every caller moved; the clock rebuild in the loop; `missingWords` with the export guard (CLI, app); validate, explain and inspect output; schema.md and schema-quick.md entries.
- Tests:
  - `speech_values_round_trip_byte_for_byte`
  - `malformed_speech_value_names_its_path`
  - `speech_file_stamps_50`
  - **AC4** `seconds_only_and_beat_projects_are_unchanged`: every repo fixture, demo, example and the BeatCut recipe, byte-identical, with the rung unchanged and `resolve_timing` equal to its pre-slice answer
  - `word_by_index_text_nth_and_sentence`
  - `edges_and_offsets_including_beats`
  - `quantize_lands_on_the_beat_grid`
  - `clock_follows_trim_speed_and_cuts`
  - `trimmed_out_word_is_a_break`
  - `clock_layer_that_is_itself_sequenced_resolves_in_the_loop`
  - `speech_beat_cycle_is_circular`
  - `guard_follows_a_moved_word_and_warns`
  - `missing_timing_lays_out_on_estimate_and_holds_render`
  - **AC2** `editing_a_line_retimes_dependent_cuts`: a FakeSynth makes deterministic synthetic speech plus timing; one narration's text changes and settle re-voices it. Only layers that name that take move, each to its word's new time, and the JSON is otherwise byte-identical.
  - `validate_lists_speech_times`
  - `cut_mid_word_warns_with_the_gap`
  - `every_loader_gives_the_same_numbers` (speech case)

**Slice 6 as built** (2026-10-21):
- **The value.** `TimeValue` keeps its two arms; speech values travel as
  `timeSpecs` entries like musical ones (`TimeSpec::speech()`,
  `speech_value`, `spec_object`), and `TimeSpec::value()` stays
  musical-only, so no beat code sees a speech spec. `SetTime` takes
  `FieldTime {Seconds, Musical, Speech}`, and speech only for `startTime`,
  `duration` and a keyframe's `time`. `after.offset` and sequence lengths
  stay `TimeValue` and so refuse speech by decoding.
- **`TimingInputs` is `BeatGrids`** (a type alias) with a new `words:
  TakeWords` field (`word_cache::take_words`: records by resource id plus
  the stale set), filled by `BeatGrids::with_words(dir, meta)`. Every
  caller keeps its signature; the loaders that have a folder add
  `.with_words`: the CLI open, `explain`, the FFI's `cached_grids` (open,
  resolve, warnings, the document's project directory). The headless
  editor document resolves without words (as it does without beat
  analyses).
- **Resolution** (`promo-timeline/src/speech_time.rs`) runs inside each
  scope's fixpoint loop, after the beat positions and before `sequence`/
  `after`/attachments, and only in a scope that uses speech. Players are
  read from the round's numbers, so a sequenced voice-over settles first
  and its words follow on the next round. The map is the closed-form
  inverse `mapping::timeline_output_time_for_source` through
  `resource_for_cut`, cut off at the layer's end — not the sampled
  piecewise inverse of §6.3. It honours trims, trim keyframes, pauses,
  resource speed and cuts; a loop maps to its first play, with no
  warning yet.
- **Circular.** A scope whose loop runs out of rounds while still moving
  (a beat clock and a speech clock placing each other) gives every layer
  the last round moved its stored numbers back, and a `Circular` break.
- **`during`:** sequence wins (`SequenceWins` over "during"), then
  `during` (`DuringWins` over `after` or `timing.start`, a break). A hand
  move of either edge drops `during` and both numbers stand, in the fold
  and in `settle_edits` alike — simpler than "a resize alone becomes a
  plain duration".
- **Missing timing.** A take with no record lays out on
  `estimate_words` over its duration, with `wordFixes` applied; it is
  named once per take (`SpeechEstimated`, not one warning per value) and
  listed in `TimingReport::missing_words`. `promo still|frames|video|gif`
  refuse while it is non-empty. The FFI answers `missingWords` (open and
  resolve). The app's export guard (`exportWaitsForGrids`, name kept)
  also waits on it, and `BeatAnalyzer.gridsBeforeExport` first runs
  `WordAligner.timeMissing`, which aligns those takes and reconciles.
- **The open aligns only when speech is used:** the CLI aligns takes
  only when some layer or composition uses a speech value or `during`
  (`speech_time::project_uses_speech`), and does it before resolving.
- **Findings** as §9, except: per-word low confidence is not reported (the
  take's confidence is); the take-longer-than-layer, by-character and
  may-not-say findings read the record (`extra.warnings`).
- **Output.** validate's text has a "Speech times" block (`at  spec  →
  label at instant s  (source)`). Its JSON adds speech rows to
  `resolvedTimes` (`clock: "speech"`, `time`, `label`, `take`, `source`,
  `word {index, text}`) and a `speechClocks` key. explain's `times` rows
  carry `clock` and `label`, and inspect has a `words` section. Labels
  use curly quotes: `word 4 “Primes”`, `ends with sentence 2`.
- **Rung 50:** `READER_VERSION` 50, `ProjectStore` 50, and the pin
  moved. `speechClock` alone stays 48. schema.md has a Speech topic and
  schema-quick.md a line; the kitchen sink sets `speechClock` (settings,
  composition, layer) and `during`.
- **AC4:** the resolved app form, the problems and the file form of all
  141 repository projects were snapshotted before the slice and diffed
  after it: identical. The committed test checks that words beside every
  take change nothing, the rung stays under 50, and no speech output
  appears; the existing seconds-only tests stand.
- **Tests:** promo-model `speech_values_round_trip_byte_for_byte`,
  `malformed_speech_value_names_its_path`, `speech_file_stamps_50`;
  promo-timeline (tests/speech_time.rs) the §10 list plus
  `during_places_a_span_and_yields_to_the_sequence`,
  `speech_in_a_length_field_is_a_break`; promo-cli
  `editing_a_line_retimes_dependent_cuts` (AC2, a deterministic
  FakeSynth through `promo_speech::settle`),
  `every_loader_gives_the_same_numbers`,
  `a_render_waits_for_missing_words`; FFI
  `opened_and_resolved_count_on_the_cached_words`; Swift
  `testSpeechTimedProjectWaitsThenAligns`.

**S7 — Caption reveal sync and generated captions.**
- Build: reveal `sync`, `unitTimes` and `unitEnds` (Rust and Swift), the `progress` change, the `captions` generator, and their fold stripping.
- Tests:
  - `synced_reveal_maps_caption_units_to_take_words`, including punctuation units, a caption that is a substring of the script, and a pinned range
  - `highlight_goes_out_between_words`
  - `reveal_without_unit_times_is_unchanged` (a pixel diff over every reveal fixture)
  - **AC1** `captions_reveal_each_word_within_one_frame`: the ElevenLabs fixture take, a highlight reveal, rendered through the CLI. The arrival of each word is found from pixel colour per unit box (an independent oracle). Each is within ±1 frame at 30 fps of the provider's word start.
  - `generated_captions_split_long_sentences`
  - `edited_generated_caption_becomes_the_persons`
  - `deleted_generated_caption_stays_deleted`
  - `generated_captions_retime_with_a_new_take`
  - Swift: `testUnitTimesSurviveTheAppForm`, and synced-caption cases in `DemoPreviewExportParityTests` (both suites) and `CrossPathRenderTests` (Mac)

**Slice 7 as built** (2026-10-21):
- **The model.** `TextReveal` gains `sync` (`RevealSync {of, words,
  sentence}`), `unitTimes` and `unitEnds`; a layer gains `captions`
  (`SpeechCaptions {by, maxWords, maxSeconds, lead, hold, reveal, style,
  skip, made}`). A reveal `sync` or `captions` stamps 50. The fold drops
  every `unitTimes`/`unitEnds` and a generator's `made` (`strip_derived`).
  The signature skips them at any depth, so derived times are never an
  edit. Swift's `TextReveal` decodes the two time lists; `sync` and
  `captions` are carried for it (`unread::carry_style_sync`, `carry_field`).
  The FFI's open, which hands back the file's own text with the resolved
  numbers copied in, copies these too (`musical::copy_derived`): without
  it the app's model never saw a word time.
- **Matching** (`promo-timeline/src/speech_effects.rs`). Units are the
  caption's whitespace words, the rasterizer's word units. Each is
  tokenized (a hyphenated unit may be several words) and matched to the
  take by a local alignment: equal words +2, a skip or mismatch −1. It is
  a separate small DP, not §2.5's mapper, which lives in promo-speech
  above the timeline. A unit with no letters takes the next word's start;
  a word the take does not say sits between its neighbours, by index.
  Times are layer-local, rounded to 1 µs. `by: "character"` and
  `"line"` take the time of the word their unit falls in, by position
  (`unit_at`) — an approximation where the counts differ, since the
  timeline does not know the wrap.
- **Where it applies:** a caption layer's own `captionStyle.reveal`, or
  its caption resource's (computed for the first layer that shows it; a
  resource shown by several layers counts on the first). Not
  `compositionSettings.subtitleReveal`: per-caption times cannot live
  there. Inside compositions, only a layer's own style is synced.
- **The engine** (`reveal::progress`): with `unitTimes`, a unit has
  arrived once its time has passed. `shown` stays at least one (the
  never-empty rule); `active` is the last arrived unit until its
  `unitEnds`, else none. The staggered modes start each unit on its own
  time and take `unitSeconds` (0.25 s when absent). `Progress` gained
  `local`. Without `unitTimes` the old path is untouched: all 81
  repository projects with a reveal render pixel-identical frames (six
  samples each, `promo frames`) with the S6 release binary and this one.
- **Captions** (`expand_captions`, project scope only, like the flash):
  - Pieces are sentences, or phrases (every comma or semicolon); a
    piece over `maxWords` (default 10) or `maxSeconds` (default 4) splits
    at the comma nearest its middle, else at the gap nearest it.
  - Each piece is a caption layer `speech-captions-<layer>-<k>` with the
    script's slice, its style, and its reveal (default word highlight)
    synced to its words. It runs from the first word − `lead` to the last
    + `hold`, never past the next piece's start.
  - It is tagged `generatedBy: captions:<layer>:<sig>`: stripped when
    unedited, kept as the person's when edited (its piece is then left
    alone).
  - The expansion writes `made` (app form only). A save that finds fewer
    of the made pieces adds the missing ones to `skip`
    (`retire_deleted_captions`), so a delete sticks.
- **Findings:** `RevealUnmatched` (a warning: how many matched, the span,
  and a note under half) and `RevealSync` (no match at all: the reveal
  walks at its own pace). explain's `reveal` units/matched block is not
  built.
- **AC1** (`promo-cli/tests/speech_captions.rs`): the ElevenLabs fixture
  take, its record made by the settle's own mapper, rendered with
  `promo frames` at 30 fps. Each word's first lit frame is read from the
  red ink's centre, and all seven land on the frame of the provider's
  start (within ±1 allowed). The pause after "Euclid." is dark.
- **Tests:** promo-timeline `synced_reveal_maps_caption_units_to_take_words`,
  `highlight_goes_out_between_words`, `reveal_without_unit_times_is_unchanged`
  (the progress for unsynced rules equals the old formula),
  `generated_captions_split_long_sentences`,
  `edited_generated_caption_becomes_the_persons`,
  `deleted_generated_caption_stays_deleted`,
  `generated_captions_retime_with_a_new_take`, and
  `a_synced_stagger_starts_each_unit_on_its_word`; promo-model
  `synced_reveal_and_captions_stamp_50_and_save_no_derived_numbers`;
  promo-cli `captions_reveal_each_word_within_one_frame` (AC1). The
  Swift tests are `testUnitTimesSurviveTheAppForm` and
  `testSyncedCaptionPreviewsAsItExports` in both suites, in a class of
  their own: preview against still export, word by word. No case was
  added to `DemoPreviewExportParityTests`, which walks the demos'
  references, or to `CrossPathRenderTests`.

**S8 — Generators on words, sequences per sentence, prominence.**
- Build: speech `on` and `range` for pulse, shake and flash; sequence `until` and `per`; per-word `prominence` (a z-score over the take's words of RMS dB, voiced F0 range and duration ratio against the estimate, squashed to 0–1), computed by the aligner pass for every take, including provider and local ones, since it reads the audio.
- Tests:
  - `pulse_on_named_words`
  - `flash_on_sentence_starts`
  - `emphasised_top_n_is_deterministic`
  - **AC8** `one_slide_per_sentence`: 4 stills `per` sentence on the fixture take; each cut lands at sentence start − lead within ±1 frame (a rendered pixel oracle)
  - `per_with_fewer_layers_holds_the_last`
  - `quantized_sentence_cuts_land_on_beats`

**Slice 8 as built** (2026-10-21):
- **Generators** (`beat_effects.rs`). `on`/`every` and `range` stay free
  JSON on `pulse`, `shake` and `flash`. A trigger with `words` or
  `sentences` — or a range with `sentence` or `words` — is speech:
  - `speech_times` reads the take through the scope's `Speech` (built
    on the final numbers and handed in through `Context`). Each time is
    mapped through the layer that plays the take and made a grid index,
    so the beat code places it unchanged: decay and duration stay
    seconds or beats.
  - `words: [texts]` takes every occurrence; an index or text the take
    lacks is a break.
  - `emphasised` takes `top` (3) by prominence, a tie going to the
    earlier word, or those `above` a value; with no prominence it is a
    break that says how to get it.
  - Ids are `speech-<layer>-pulse<n>-<k>-<role>` (and `…-shake…`), with
    the tag `auto: "speech:<sig>"`. `is_generator_tag` replaces the
    `beat:` checks in the strip, `carries_specs`, settle and explain. The
    app's `isBeatGenerated` reads both tags.
  - A flash keeps its `beat-flash-<n>` layer.
- **Sequences.** `SequenceEntry` gains `layers`, `until`, `per`, `of`,
  `lead`, `hold` and `quantize`; `layer` may be empty for a `per` entry.
  - `until` takes seconds, a musical position or a speech position.
    `Plan::place` now takes the round's `Speech`.
  - A `per` entry expands before the plan (`expand_per`) into one entry
    per layer. Slide k gets `until` sentence k+2 with `offset: -lead`;
    the last gets the last sentence's end plus `hold` (default 0.5).
    `quantize` is copied into each value. Either way, a slide count that
    differs from the sentence count is `PerSlots` (a warning), and extra
    layers keep their own numbers.
  - A first member whose start is a speech value is not snapped to the
    beat.
  - A per member moved or resized by hand leaves the entry, both in the
    fold and in `settle_edits`.
- **Prominence** (`promo_media::words::prominence`): the z-scores over
  the take's words of their loudness, their pitch range (semitones;
  `frames` now records each voiced frame's autocorrelation pitch) and
  their length against the estimate. These are averaged, squashed by a
  logistic at 1.5 and rounded to 0.001.
  - It is computed in `timing_for` (the CLI open and `promo words`) for
    every record whose words lack it, provider and local ones included,
    and the record is stored again. `align_file` and the golden are
    unchanged.
  - **Not built for the apps:** the app's FFI aligner writes no
    prominence. In the app, `emphasised` works only on a record the CLI
    has measured, and otherwise says why.
- **Rung:** a speech trigger or range on any generator, and any `until`
  or `per`, stamps 50 (`needs_speech_reader`). The open aligns when
  `needs_speech_reader` holds.
- **AC8** (`promo-cli/tests/speech_slides.rs`): four solid stills `per`
  sentence of the synthetic take (eight sentences), rendered with `promo
  frames` at 30 fps. The first frame of each colour is within ±1 frame
  of its sentence's start less the lead; the last slide holds, and
  validate says "4 slides for the 8 sentences". The open stored each
  word's prominence.
- **Tests:** promo-timeline `pulse_on_named_words`,
  `flash_on_sentence_starts`, `emphasised_top_n_is_deterministic`,
  `sequence_until_ends_on_a_word`, `per_with_fewer_layers_holds_the_last`
  and `quantized_sentence_cuts_land_on_beats`; promo-media
  `prominence_marks_the_stressed_word`; promo-cli `one_slide_per_sentence`
  (AC8); the rung cases in promo-model `speech_file_stamps_50`.

**S9 — The apps.**
- Build: the FFI aligner and stores, `WordAligner.swift`, `missingWords` in the open and the export guard, then the §8.2 UI and the app's `promo_words` twin.
- Tests:
  - Rust `ffi_aligner_matches_cli_alignment` (the same PCM gives identical JSON)
  - `bindings` up to date
  - Swift: `testSynthesisStoresProviderTiming` (a fake provider with a fixture answer), `testAlignerMatchesCoreGolden` (the synthetic WAV through AVAudioFile), `testOpeningMarksNothingDirty`, `testSnapToWordUsesTheCoresSpec`, `testTimingRowShowsTheCoresLabel`, `testExportWaitsForWords`
  - Checked in the simulator at phone width and on iPad, and on the Mac through the DEBUG tour door (`mac-ui-tour-captures`)

**Slice 9 as built** (2026-10-21):
- **Most of this slice's build list was already there.** The FFI aligner
  and stores, `WordAligner.swift` and the app's `promo_words` came in S5,
  and `missingWords` with the export guard in S6. S9 adds:
  - **Prominence everywhere.** `WordAligner::finish` writes each word's
    prominence (the golden `synthetic-60w.json` is regenerated, the WAV
    unchanged). `score(record)` reads it for a record another source
    timed, exposed to the app as `promo_word_aligner_score`. The app
    measures takes with none (`WordAligner.measureProminence`) on open
    and before an export, but only when the project names `emphasised`
    words. Its `promo_words` adds prominence to a cached record that lacks
    it, as the CLI's `timing_for` does.
  - **The app adopts generated keyframes the core makes later.**
    `PromoTiming.reconcile` brings a layer home when the RESOLVED layer
    carries generated keyframes, not only the stored one. Without this, a
    pulse whose words got their prominence after the open never reached
    the model.
- **Core questions for the editors** (`speech_time.rs`):
  - `snap_speech(meta, inputs, layer, edge, word|gap|sentence)` gives the
    value to write, its seconds (an end's as a length) and the core's
    label. A word is written with its text as the guard and an end edge
    as `edge: "end"`; a gap as `{"snap": "gap", "near": t}`. The take is
    the layer's or the scope's speech clock, else the one playing at
    the edge, else the only one.
  - `snap_speech_time` is the timeline drag's version, and `word_ticks`
    the lane's words.
  - FFI: `promo_project_speech_snap`, `_speech_snap_time` and
    `_word_ticks`.
  - A resolved gap's label is now "the gap after word n “…”".
- **App UI** (shared by Mac and iOS):
  - **Words section:** audio resources with a `speech` spec get it in
    the inspector (`SpeechSyncViews.swift`): where the times came from,
    the confidence, counts, a stale or unsure note, hand fixes (read from
    the source document), every word (# start end conf word; prominent
    words bold, unsure ones orange), and "Time the Words" when a take has
    none. Recordings that only carry a `transcript` get no section: the
    Swift model has no `transcript`.
  - **Word ticks:** on the voice-over's bar in the list timeline, and a
    Words lane under its audio lane in the lanes timeline. Sentence starts
    are taller; unsure words are dimmed.
  - **Snapping:** the start/duration menus gain "Snap to Speech" (word,
    gap, sentence) whenever the project has a spoken take. Without a beat
    clock the menu reads "Start on Words" / "End on Words". The row shows
    the core's label. The timeline's snap menu gains "Snap to Words" and
    "Snap to Sentences" for drags (`TimelineSnapper`); the voice-over
    itself never snaps to its own words.
  - **DEBUG tour steps:** `resource.audio`, `words.list`/`words.close`,
    `timeline.lanes`/`timeline.list`.
- **Leftovers from S8:** explain shows a synced caption's `reveal`
  (`{"sync", "units", "matched", "words": "words a–b"}`, or `error`), via
  `speech_effects::reveal_match`. A value landing on a word whose own
  confidence is under 0.5 warns (`SpeechWordUnsure`).
- **Checked on screen:**
  - Mac, through the DEBUG tour door (snapshot step; the screen was
    locked to `screencapture`): the Words section with its list, the
    timeline with word ticks under the voice-over, and the Title layer's
    rows reading "word 3 “Geometry”" and "ends with sentence 2", with
    "Start on Words".
  - iPhone 18 Pro simulator: the ticks on the voice-over's bar; the
    editor's "Start on Words" menu; "To Nearest Word" writing "word 1
    “Meet”" at 0.616 s.
- **Tests:**
  - promo-timeline `snap_to_word_gap_and_sentence`,
    `word_ticks_follow_the_layer`, `a_value_on_an_unsure_word_warns`.
  - promo-cli: explain's `reveal` block, inside the AC1 test.
  - promo-media golden regenerated.
  - FFI `prominence_snap_and_ticks_doors`.
  - Swift `SpeechEditingTests`: `testOpeningMarksNothingDirty`,
    `testSnapToWordUsesTheCoresSpec`, `testTimingRowShowsTheCoresLabel`,
    `testWordLaneAndDragSnapAreTheCores` and
    `testEmphasisedWordsGetProminenceInTheApp`.
  - Every new test was break-tested.

**S10 — Recipe and docs.**
- Build: a schema.md section "Speech sync — word, sentence, of, speechClock, during, sync, captions, transcript, words" with cross-references from Timing, Reveal and Beats; schema-quick.md's sixth recipe, "narrated slideshow: one slide per sentence, karaoke captions" (`examples/Narrated.promo`, a placeholder take, so it renders with no key and is honestly estimated); SKILL.md entries for `promo_words` and the recipe (and the app's bundled copy).
- Tests:
  - **AC9** `narrated_recipe_validates_and_renders`, `the_examples_are_the_recipes`
  - `schema_md_mentions_every_authorable_wire_field` (the kitchen sink carries every speech field)

**Acceptance criteria:**
1. **AC1.** Captions reveal each word within ±1 frame of the provider's word start (S7).
2. **AC2.** Changing a line's text re-times the dependent cuts and nothing else (S6).
3. **AC3.** Local takes' word starts are within 60 ms median and 120 ms p95 of the forced alignment, and the durations account for every sample (S4/S5).
4. **AC4.** Seconds-only and beat-only projects are unchanged, and the rung moves only for the new forms (S6, S7).
5. **AC5.** The aligner is deterministic across machines, and within 20 ms on the synthetic truth (S5).
6. **AC6.** Placeholders carry estimated word times in the same format, flagged (S2).
7. **AC7.** `promo_words` and `promo words` exist, with the JSON shape pinned (S5).
8. **AC8.** One slide per sentence cuts within ±1 frame (S8).
9. **AC9.** The recipe validates and renders, and the docs cover every key (S10).

---

## 11. Risks

1. **MP3 priming.** ElevenLabs and Google times are relative to their decoded PCM. AVFoundation and ffmpeg trim LAME priming only when the gapless header is present, so the whole take may shift by about 25 ms. That is close to AC1's one frame.
   - Measure it in S3 against the energy onset of word 1.
   - If it is systematic, apply a per-provider constant (stored with the record as `audioOffset`).
   - The S5 cross-check (aligner against provider) catches drift.
2. **Aligner accuracy on real recordings** (§4.7). Word-level karaoke on fast human speech will sometimes be visibly loose. Mitigations: confidence warnings, `--transcribe`, `wordFixes`, and cutting on gaps rather than word starts. The tests prove determinism and synthetic truth, not real-world taste. Hand-check three real recordings before S6 ships.
3. **The Kokoro graph patch.** Exposing `/encoder/Clip_output_0` can change ORT's fusion choices, so the waveform could differ in the last bits.
   - `timed_audio_equals_untimed_audio` guards it.
   - If it fails: run the unpatched session for audio and the patched session only for durations (twice the encoder cost, still deterministic), or pin a timestamped export as a new variant (a new download and a new pin).
   - The tensor name is an export detail. Re-check it whenever a Kokoro variant pin changes; the test fails loudly.
4. **Google.** The voice families supporting marks, and whether marks are billed, are taken from documentation and must be verified live (S3). Chirp3-HD, the recommended voice, gets alignment only.
5. **Index fragility.** `{"word": 12}` silently means another word after an edit. The `text` guard, the editors writing it, and the guard warning contain this, and the docs recommend `{"word": "Euclid"}` for hand-written files.
6. **Swift drops the derived fields.** A copy, paste or rebuild path that constructs a fresh `TextReveal` loses `unitTimes`, and the preview then types uniformly while exports agree with the core. The three-path parity tests are the tripwire, plus model tests per path (BEAT-SYNC risk 4, again).
7. **Placeholder records and re-takes.** The record must go with the stand-in file. A leftover record is harmless, because it is keyed by bytes no file has, but `.words/` grows. `promo words --prune` removes records with no file (S5).
8. **Normalization drift.** local-tts and the providers normalize numbers, dates and URLs differently, so §2.5's merges must be generous. A wrong 1:k merge mis-times one word and never shifts the rest, because the DP is monotonic.
9. **CJK and Thai.** Whitespace words do not exist there, so v1 syncs by character (and sentences). It is noted, not hidden.
10. **The dot-folder in `Resources/`.** The scanners already skip `.beats` (the BEAT-SYNC risk 8 audit). Add `.words` to the same test.
11. **Rung 50 locks out shipped apps** (3.2 and older) for files that use speech sync. This was accepted for 49, and the same rule applies. Seconds-only and beat files are unaffected (AC4).
12. **Cost.** Timing adds no provider calls: ElevenLabs and Google return it in the same request. Only `--transcribe` spends extra, and only when asked.
