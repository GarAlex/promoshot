"""Do the cuts in a rendered video land on the song's beats? An oracle that
never reads the project's timing: it finds the cuts in the PICTURE (a
sudden change out of a settled frame, far larger than a punch-in) and the
beats in the SOUND (`promo beats` on the song, and again on the video's
own audio track, which also says whether the music plays in sync).

    python3 demos/beat_cuts.py <video.mp4> <song.wav>

A cut at beat time b shows first on frame ceil(b * fps), 0 to one frame
after the beat; a cut counts as on the beat within one frame either side.
score.py runs it for a rubric with `onBeat` on the video the agent rendered.
"""
import json, os, subprocess, sys, tempfile
FFMPEG = '/opt/homebrew/bin/ffmpeg'
FFPROBE = '/opt/homebrew/bin/ffprobe'
PROMO = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), 'target', 'release', 'promo')
W, H = 96, 54

def frames(video):
    raw = subprocess.run([FFMPEG, '-v', 'error', '-i', video, '-vf', f'scale={W}:{H}:flags=area,format=gray',
                          '-f', 'rawvideo', '-'], capture_output=True, check=True).stdout
    n = W * H
    return [raw[i:i + n] for i in range(0, len(raw) - n + 1, n)]

def fps_of(video):
    r = subprocess.run([FFPROBE, '-v', 'error', '-select_streams', 'v:0', '-show_entries', 'stream=r_frame_rate',
                        '-of', 'csv=p=0', video], capture_output=True, text=True).stdout.strip()
    try:
        a, b = r.split('/')
        return float(a) / float(b)
    except Exception:
        return 30.0

def beats_of(path):
    return json.loads(subprocess.run([PROMO, 'beats', path, '--json', '--refresh'], capture_output=True,
                                     text=True, check=True).stdout)

def measure(video, song):
    fps = fps_of(video)
    fr = frames(video)

    # Per frame against the one before: the largest change in any 8x6
    # block (two screenshots of one app differ only in a chart), and the
    # least brightening of the four corners — the canvas round a framed
    # picture: a flash brightens all four, a cut to a lighter picture not.
    def compare(k):
        a, b = fr[k], fr[k - 1]
        best = 0.0
        for by in range(0, H, 6):
            for bx in range(0, W, 8):
                s = sum(abs(a[y * W + x] - b[y * W + x]) for y in range(by, by + 6) for x in range(bx, bx + 8))
                best = max(best, s / 48)
        corner = [sum(a[y * W + x] - b[y * W + x] for y in range(by, by + 6) for x in range(bx, bx + 8)) / 48
                  for bx, by in ((0, 0), (W - 8, 0), (0, H - 6), (W - 8, H - 6))]
        return best, min(corner)
    stats = [(0.0, 0.0)] + [compare(k) for k in range(1, len(fr))]
    block = [s[0] for s in stats]
    flashes = [k for k in range(1, len(fr)) if stats[k][1] > 10 and stats[k - 1][1] <= 10]
    flashy = set(f + d for f in flashes for d in range(16))       # a flash and its fade
    # A cut: a sudden change out of a settled picture, much larger than
    # the change into the frame before (a transition or a punch-in grows).
    cuts = [k for k in range(1, len(fr)) if block[k] > 8 and block[k - 1] < 0.5 * block[k]
            and (k not in flashy or abs(stats[k][1]) <= 5)]
    song_beats = beats_of(song)
    with tempfile.TemporaryDirectory() as tmp:
        track = os.path.join(tmp, 'track.wav')
        subprocess.run([FFMPEG, '-v', 'error', '-i', video, '-vn', '-ac', '1', '-c:a', 'pcm_s16le', track], check=True)
        try:
            video_beats = beats_of(track)
        except Exception:
            video_beats = None
    grid = song_beats['beats']
    first = song_beats['beats'].index(song_beats['downbeats'][0]) if song_beats['downbeats'] else 0
    # before the first beat is the lead-in (a title fading or zooming up)
    cuts = [k for k in cuts if k / fps >= grid[0] - 1 / fps]
    rows = []
    for k in cuts:
        t = k / fps
        b = min(grid, key=lambda b: abs(b - t))
        n = grid.index(b) - first
        rows.append({"frame": k, "time": round(t, 3), "bar": n // 4 + 1, "beat": n % 4 + 1,
                     "error_ms": round((t - b) * 1000, 1), "on": -1 / fps - 1e-6 <= t - b <= 1 / fps + 1e-6})
    sync = None
    if video_beats and video_beats.get('beats'):
        shift = [v - min(grid, key=lambda b: abs(b - v)) for v in video_beats['beats']]
        sync = {"bpm": video_beats['bpm'], "beats": len(shift),
                "min_ms": round(min(shift) * 1000, 1), "max_ms": round(max(shift) * 1000, 1)}
    flash_rows = []
    for k in flashes:
        t = k / fps
        d = min(song_beats['downbeats'] or grid, key=lambda b: abs(b - t))
        flash_rows.append({"frame": k, "time": round(t, 3), "from_downbeat_ms": round((t - d) * 1000, 1)})
    return {"fps": fps, "song": {k: song_beats[k] for k in ('bpm', 'confidence', 'offset', 'grid')} |
            {"beats": len(grid), "sections": [(s['label'], s['startBar'], s['bars']) for s in song_beats['sections']]},
            "audio": sync, "cuts": rows, "on_beat": sum(1 for r in rows if r['on']), "flashes": flash_rows}

def main():
    m = measure(sys.argv[1], sys.argv[2])
    s = m['song']
    print(f"song: {s['bpm']} BPM, confidence {s['confidence']}, {s['beats']} beats ({s['grid']} grid), "
          f"first downbeat {s['offset']:.3f} s, sections "
          + ", ".join(f"{label} bars {a}-{a + n - 1}" for label, a, n in s['sections']))
    a = m['audio']
    print(f"the video's own audio: {a['bpm']} BPM, {a['beats']} beats, {a['min_ms']:+.1f} to {a['max_ms']:+.1f} ms "
          "from the song's beats" if a else "the video's own audio: no beats found")
    print(f"{len(m['cuts'])} cuts found in the picture at {m['fps']:g} fps; {m['on_beat']} on the beat "
          f"(within a frame; a frame grid shows a cut 0 to {1000 / m['fps']:.1f} ms after its beat)")
    for r in m['cuts']:
        print(f"  frame {r['frame']:4d}  {r['time']:7.3f} s  bar {r['bar']:2d} beat {r['beat']}  "
              f"{r['error_ms']:+6.1f} ms{'' if r['on'] else '  OFF'}")
    for f in m['flashes']:
        print(f"  flash from frame {f['frame']} ({f['time']:.3f} s), {f['from_downbeat_ms']:+.1f} ms from the nearest downbeat")
    sys.exit(0 if m['cuts'] and m['on_beat'] == len(m['cuts']) else 1)

if __name__ == '__main__':
    main()
