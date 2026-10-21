"""Builds the provider-answer fixtures for docs/SPEECH-SYNC.md slice 3.

These are NOT recorded from a live key: no provider key is readable
headlessly on the build machine. They are the providers' documented
answer shapes — ElevenLabs `POST /v1/text-to-speech/{voice}/with-timestamps`
(`audio_base64`, `alignment`, `normalized_alignment`) and Google
`v1beta1/text:synthesize` with `enableTimePointing: ["SSML_MARK"]`
(`audioContent`, `timepoints[{markName, timeSeconds}]`) — filled with word
times chosen here, and an MP3 that has energy exactly there (a 180 Hz
tone per word), so a later aligner check has something to find.

    python3 fixtures/speech/make_fixtures.py   (needs ffmpeg)
"""
import base64, json, subprocess, tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = "Meet Euclid. Geometry, proved step by step."
# (word, start, end) in seconds: what each fixture says.
WORDS = [("Meet", 0.12, 0.38), ("Euclid", 0.42, 0.90), ("Geometry", 1.40, 1.96),
         ("proved", 2.16, 2.52), ("step", 2.56, 2.80), ("by", 2.84, 2.98),
         ("step", 3.02, 3.40)]
DURATION = 3.7
RU_SCRIPT = "Это стоИт денег."
RU_SENT = "Это стои́т денег."
RU_WORDS = [("Это", 0.10, 0.40), ("стои́т", 0.46, 0.95), ("денег", 1.02, 1.50)]
RU_DURATION = 1.8


def mp3(words, duration, out):
    expr = "+".join(f"between(t,{a},{b})*0.4*sin(2*PI*180*t)" for _, a, b in words)
    subprocess.run(["ffmpeg", "-loglevel", "error", "-y", "-f", "lavfi", "-i",
                    f"aevalsrc='{expr}':s=44100:d={duration}", "-ac", "1", "-b:a", "48k",
                    str(out)], check=True)
    return out.read_bytes()


def characters(sent, words):
    """Per-character times over `sent`: a word's letters share its span,
    whatever stands between words sits in the gap."""
    chars, starts, ends = [], [], []
    spans = []
    cursor = 0
    for text, a, b in words:
        at = sent.index(text, cursor)
        spans.append((at, at + len(text), a, b))
        cursor = at + len(text)
    previous_end = 0.0
    k = 0
    i = 0
    while i < len(sent):
        if k < len(spans) and i == spans[k][0]:
            at, stop, a, b = spans[k]
            step = (b - a) / (stop - at)
            for j in range(at, stop):
                chars.append(sent[j])
                starts.append(round(a + (j - at) * step, 3))
                ends.append(round(a + (j - at + 1) * step, 3))
            previous_end = b
            i = stop
            k += 1
            continue
        nxt = spans[k][2] if k < len(spans) else previous_end + 0.05
        chars.append(sent[i])
        starts.append(round(previous_end, 3))
        ends.append(round(max(previous_end, nxt), 3))
        i += 1
    return {"characters": chars, "character_start_times_seconds": starts,
            "character_end_times_seconds": ends}


def main():
    eleven = mp3(WORDS, DURATION, HERE / "elevenlabs-with-timestamps.mp3")
    alignment = characters(SCRIPT, WORDS)
    (HERE / "elevenlabs-with-timestamps.json").write_text(json.dumps({
        "audio_base64": base64.b64encode(eleven).decode(),
        "alignment": alignment, "normalized_alignment": alignment}, indent=1) + "\n")

    ru = mp3(RU_WORDS, RU_DURATION, HERE / "elevenlabs-stress-ru.mp3")
    alignment = characters(RU_SENT, RU_WORDS)
    # ElevenLabs may hand a vowel and its combining acute back as ONE
    # character string; the parser reads scalars either way.
    merged = {"characters": [], "character_start_times_seconds": [],
              "character_end_times_seconds": []}
    for c, s, e in zip(alignment["characters"], alignment["character_start_times_seconds"],
                       alignment["character_end_times_seconds"]):
        if c == "́":
            merged["characters"][-1] += c
            merged["character_end_times_seconds"][-1] = e
            continue
        merged["characters"].append(c)
        merged["character_start_times_seconds"].append(s)
        merged["character_end_times_seconds"].append(e)
    (HERE / "elevenlabs-stress-ru.json").write_text(json.dumps({
        "audio_base64": base64.b64encode(ru).decode(),
        "alignment": merged, "normalized_alignment": merged}, indent=1,
        ensure_ascii=False) + "\n")

    google = mp3(WORDS, DURATION, HERE / "google-timepoints.mp3")
    (HERE / "google-timepoints.json").write_text(json.dumps({
        "audioContent": base64.b64encode(google).decode(),
        "timepoints": [{"markName": f"w{k}", "timeSeconds": a}
                       for k, (_, a, _) in enumerate(WORDS)],
        "audioConfig": {"audioEncoding": "MP3", "speakingRate": 1, "pitch": 0,
                        "volumeGainDb": 0, "sampleRateHertz": 24000}}, indent=1) + "\n")


def transcriptions():
    """The opt-in transcription answers (§4.6), in their documented shapes:
    OpenAI `audio/transcriptions` verbose_json with word timestamps, and
    ElevenLabs `speech-to-text` (Scribe) with its spacing entries. The
    recogniser's own spellings: punctuation on the words."""
    said = ["Meet", "Euclid.", "Geometry,", "proved", "step", "by", "step."]
    openai = {"task": "transcribe", "language": "english", "duration": DURATION,
              "text": SCRIPT,
              "words": [{"word": w, "start": a, "end": b}
                        for w, (_, a, b) in zip(said, WORDS)]}
    (HERE / "openai-transcription.json").write_text(json.dumps(openai, indent=1) + "\n")
    words = []
    for k, (w, (_, a, b)) in enumerate(zip(said, WORDS)):
        if k:
            words.append({"text": " ", "start": WORDS[k - 1][2], "end": a, "type": "spacing"})
        words.append({"text": w, "start": a, "end": b, "type": "word"})
    scribe = {"language_code": "en", "language_probability": 0.99, "text": SCRIPT,
              "words": words}
    (HERE / "elevenlabs-scribe.json").write_text(json.dumps(scribe, indent=1) + "\n")


if __name__ == "__main__":
    main()
    transcriptions()
