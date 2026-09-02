#!/bin/zsh
# One demo, one FRESH agent: a workspace with the demo's media, the
# public skill as a project skill, and the headless MCP fenced to that
# folder — no repository, no memory, no reference — then the score and
# contact sheets of the result and the reference at the same moments.
#
#   demos/run.sh <demo dir> [model]
#   CLAUDE_BIN=… overrides the Claude Code binary.
set -e
DEMO="${1:A}"; MODEL="${2:-}"
[ -f "$DEMO/prompt.md" ] || { echo "no prompt.md in $DEMO"; exit 2 }
HERE="${0:A:h}"; CORE="${HERE:h}"
CLI="${CLAUDE_BIN:-$HOME/Library/Application Support/Claude/claude-code/2.1.255/claude.app/Contents/MacOS/claude}"
MCP="$CORE/target/release/promoshot-mcp"; PROMO="$CORE/target/release/promo"
[ -x "$MCP" ] && [ -x "$PROMO" ] || { echo "build first: cargo build --release -p promoshot-mcp -p promo-cli"; exit 2 }
export PATH=/opt/homebrew/bin:$PATH
TS="$(date +%Y%m%d-%H%M%S)"; RUN="$DEMO/runs/$TS"; WS="$RUN/ws"
mkdir -p "$WS/resources" "$WS/.claude/skills/promoshot"
# The media: the demo's own files, plus shared ones named by the rubric.
cp -R "$DEMO/resources/." "$WS/resources/" 2>/dev/null || true
for f in $(python3 -c "import json;print(' '.join(json.load(open('$DEMO/rubric.json'))['media']))"); do
  [ -e "$WS/resources/$f" ] || cp "$HERE/_media/$f" "$WS/resources/$f" 2>/dev/null || echo "missing media $f"
done
cp "$CORE/skill/SKILL.md" "$WS/.claude/skills/promoshot/SKILL.md"
cat > "$WS/.mcp.json" <<JSON
{"mcpServers": {"promoshot": {"command": "$MCP", "args": ["--workspace", "$WS", "--root", "$WS", "--log", "$RUN/mcp.log"]}}}
JSON
PROMPT="$(cat "$DEMO/prompt.md")"
echo "== $(basename "$DEMO") → runs/$TS"
START=$(date +%s)
( cd "$WS" && "$CLI" --print "$PROMPT" \
    --mcp-config "$WS/.mcp.json" --strict-mcp-config \
    --allowedTools "mcp__promoshot__*" "Skill" "Read" "Write" "Edit" "Glob" "Grep" \
      "Bash(cp:*)" "Bash(mkdir:*)" "Bash(ls:*)" "Bash(cat:*)" "Bash(python3:*)" \
    --output-format json ${MODEL:+--model "$MODEL"} \
    > "$RUN/agent.json" 2> "$RUN/agent.err" || true )
END=$(date +%s)
python3 - "$RUN/agent.json" "$RUN/summary.txt" $((END-START)) <<'PY'
import json, sys
try: j = json.load(open(sys.argv[1]))
except Exception as e: j = {"error": str(e)}
line = f"turns={j.get('num_turns')} cost=${j.get('total_cost_usd', 0):.2f} secs={sys.argv[3]} result={str(j.get('result', j.get('error')))[:400]}"
open(sys.argv[2], 'w').write(line + "\n"); print(line)
PY
OUT="$WS/out.promo"
if [ -f "$OUT/metadata.json" ]; then
  python3 "$HERE/score.py" "$DEMO" "$OUT" | tee "$RUN/score.txt"
  python3 "$HERE/score.py" "$DEMO" "$OUT" --json > "$RUN/score.json"
  # The reference, materialised from reference.json and the same media.
  REF="$RUN/reference.promo"; mkdir -p "$REF/Resources"
  cp "$DEMO/reference.json" "$REF/metadata.json"; cp -R "$WS/resources/." "$REF/Resources/" 2>/dev/null || true
  DUR=$(python3 -c "import json;print(json.load(open('$OUT/metadata.json')).get('videoDuration') or 10)")
  for side in agent reference; do
    SRC="$OUT"; [ "$side" = reference ] && SRC="$REF"
    mkdir -p "$RUN/frames-$side"
    for f in 0.08 0.25 0.42 0.6 0.78 0.95; do
      T=$(python3 -c "print(f'{$DUR*$f:.2f}')")
      "$PROMO" still "$SRC" --out "$RUN/frames-$side/t$T.png" --time $T --size 640x400 > /dev/null 2>&1 || true
    done
    ffmpeg -v error -y -pattern_type glob -i "$RUN/frames-$side/*.png" -filter_complex "tile=3x2" "$RUN/contact-$side.png" 2>/dev/null || true
  done
  # 1280 wide in the canvas's aspect: the copy the site and the page link to.
  SIZE=$(python3 -c "import json;cs=json.load(open('$OUT/metadata.json'))['compositionSettings'];w=cs.get('canvasWidth') or 1440;h=cs.get('canvasHeight') or 900;print(f'1280x{int(round(1280*h/w/2))*2}')")
  "$PROMO" video "$OUT" --out "$RUN/agent.mp4" --size $SIZE > /dev/null 2>&1 || true
else
  echo "no out.promo produced" | tee "$RUN/score.txt"
fi
