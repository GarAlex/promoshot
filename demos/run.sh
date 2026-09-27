#!/bin/zsh
# One demo, one FRESH agent: a workspace with the demo's media, the
# public skill as a project skill, and the headless MCP fenced to that
# folder — no repository, no memory, no reference — then the score and
# contact sheets of the result and the reference at the same moments.
#
#   demos/run.sh <demo dir> [model]
#   CLAUDE_BIN=… overrides the Claude Code binary.
#
# A SCENARIO (review 2026-09-27, P2-35) is a demo with a `scenario.json`
# saying how the agent reaches PromoShot, because every run before this
# pre-registered the headless server and pre-installed the skill:
#   "mode": "headless" (the default above), "path" (the CLI on PATH and
#   nothing registered) or "app" (the app's server over --mcp-stdio,
#   PROMOSHOT_APP=… for the binary);
#   "path": true puts the CLI on PATH in app mode too;
#   "start": a project folder copied to out.promo before the run;
#   "open": true opens out.promo in the app first;
#   "requires": {"automation": true|false} — read from the app's defaults,
#   and the run refuses rather than changing a person's settings;
#   "ask": lines the person must do by hand first (printed, then confirmed
#   at the prompt, or by SCENARIO_CONFIRMED=1).
# A scenario's transcript is kept whole (agent.jsonl) and
# scenario_check.py judges what the agent DID, not only what it made.
set -e
DEMO="${1:A}"; MODEL="${2:-}"
[ -f "$DEMO/prompt.md" ] || { echo "no prompt.md in $DEMO"; exit 2 }
HERE="${0:A:h}"; CORE="${HERE:h}"
CLI="${CLAUDE_BIN:-$(ls -d "$HOME/Library/Application Support/Claude/claude-code/"*/claude.app/Contents/MacOS/claude 2>/dev/null | sort -V | tail -1)}"
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
SCENARIO="$DEMO/scenario.json"
sc() { [ -f "$SCENARIO" ] && python3 -c "import json,sys;v=json.load(open('$SCENARIO')).get('$1');print('' if v is None else (json.dumps(v) if isinstance(v,(dict,list)) else v))" || echo ""; }
MODE="$(sc mode)"; MODE="${MODE:-headless}"
APP="${PROMOSHOT_APP:-/Applications/PromoShot.app/Contents/MacOS/PromoShot}"
TOOLS=("Skill" "Read" "Write" "Edit" "Glob" "Grep" "Bash(cp:*)" "Bash(mkdir:*)" "Bash(ls:*)" "Bash(cat:*)" "Bash(python3:*)")
case "$MODE" in
  headless)
    cat > "$WS/.mcp.json" <<JSON
{"mcpServers": {"promoshot": {"command": "$MCP", "args": ["--workspace", "$WS", "--root", "$WS", "--log", "$RUN/mcp.log"]}}}
JSON
    TOOLS+=("mcp__promoshot__*") ;;
  path)
    echo '{"mcpServers": {}}' > "$WS/.mcp.json" ;;
  app)
    [ -x "$APP" ] || { echo "no PromoShot app at $APP (PROMOSHOT_APP=…)"; exit 2 }
    WANT="$(sc requires | python3 -c "import json,sys;t=sys.stdin.read().strip();print(json.loads(t).get('automation','') if t else '')")"
    HAVE="$(defaults read com.writea.revoice PromoMCPEnabled 2>/dev/null || echo 0)"
    if [ -n "$WANT" ]; then
      [ "$WANT" = "True" ] && WANT=1; [ "$WANT" = "False" ] && WANT=0
      [ "$HAVE" = "$WANT" ] || { echo "this scenario wants Automation $([ "$WANT" = 1 ] && echo ON || echo OFF) in PromoShot's Settings; it is $([ "$HAVE" = 1 ] && echo on || echo off) — switch it, then run again"; exit 3 }
    fi
    cat > "$WS/.mcp.json" <<JSON
{"mcpServers": {"promoshot": {"command": "$APP", "args": ["--mcp-stdio"]}}}
JSON
    TOOLS+=("mcp__promoshot__*") ;;
  *) echo "unknown scenario mode $MODE"; exit 2 ;;
esac
if [ "$MODE" = path ] || [ "$(sc path)" = True ]; then
  # The binaries on PATH and nothing registered: the skill's second rung.
  export PATH="$CORE/target/release:$PATH"
  TOOLS+=("Bash(promo:*)" "Bash(command -v:*)" "Bash(which:*)" "Bash(promoshot-mcp:*)")
fi
START_PROJECT="$(sc start)"
[ -n "$START_PROJECT" ] && cp -R "$DEMO/$START_PROJECT" "$WS/out.promo"
cp -R "$WS/out.promo" "$RUN/before.promo" 2>/dev/null || true
if [ -n "$(sc ask)" ]; then
  echo "Before this scenario, by hand:"
  python3 -c "import json;[print('  -', a) for a in json.load(open('$SCENARIO'))['ask']]"
  if [ "$(sc open)" = True ]; then open -a "${APP:h:h:h}" "$WS/out.promo"; fi
  if [ "${SCENARIO_CONFIRMED:-}" != 1 ]; then
    read -r "?Done? [y/N] " OK; [ "$OK" = y ] || { echo "not run"; exit 3 }
  fi
fi
PROMPT="$(cat "$DEMO/prompt.md")"
echo "== $(basename "$DEMO") [$MODE] → runs/$TS"
START=$(date +%s)
if [ -f "$SCENARIO" ]; then
  # The whole transcript: a scenario is judged on what the agent did.
  ( cd "$WS" && "$CLI" --print "$PROMPT" \
      --mcp-config "$WS/.mcp.json" --strict-mcp-config \
      --allowedTools "${TOOLS[@]}" \
      --output-format stream-json --verbose ${MODEL:+--model "$MODEL"} \
      > "$RUN/agent.jsonl" 2> "$RUN/agent.err" || true )
  python3 -c "
import json,sys
last={}
for line in open('$RUN/agent.jsonl'):
    try: m=json.loads(line)
    except Exception: continue
    if m.get('type')=='result': last=m
json.dump(last, open('$RUN/agent.json','w'))"
else
  ( cd "$WS" && "$CLI" --print "$PROMPT" \
      --mcp-config "$WS/.mcp.json" --strict-mcp-config \
      --allowedTools "${TOOLS[@]}" \
      --output-format json ${MODEL:+--model "$MODEL"} \
      > "$RUN/agent.json" 2> "$RUN/agent.err" || true )
fi
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
  # The reference, materialised from reference.json and the same media —
  # a creative run has none, and shows only what the agent made.
  SIDES="agent"
  if [ -f "$DEMO/reference.json" ]; then
    REF="$RUN/reference.promo"; mkdir -p "$REF/Resources"
    cp "$DEMO/reference.json" "$REF/metadata.json"; cp -R "$WS/resources/." "$REF/Resources/" 2>/dev/null || true
    SIDES="agent reference"
  fi
  DUR=$(python3 -c "import json;print(json.load(open('$OUT/metadata.json')).get('videoDuration') or 10)")
  for side in ${=SIDES}; do
    SRC="$OUT"; [ "$side" = reference ] && SRC="$REF"
    mkdir -p "$RUN/frames-$side"
    for f in 0.08 0.25 0.42 0.6 0.78 0.95; do
      T=$(python3 -c "print(f'{$DUR*$f:.2f}')"); TAG=$(python3 -c "print(f'{$DUR*$f:06.2f}')")
      "$PROMO" still "$SRC" --out "$RUN/frames-$side/t$TAG.png" --time $T --size 640x400 > /dev/null 2>&1 || true
    done
    ffmpeg -v error -y -pattern_type glob -i "$RUN/frames-$side/*.png" -filter_complex "tile=3x2" "$RUN/contact-$side.png" 2>/dev/null || true
  done
  # 1280 wide in the canvas's aspect: the copy the site and the page link to.
  SIZE=$(python3 -c "import json;cs=json.load(open('$OUT/metadata.json'))['compositionSettings'];w=cs.get('canvasWidth') or 1440;h=cs.get('canvasHeight') or 900;print(f'1280x{int(round(1280*h/w/2))*2}')")
  "$PROMO" video "$OUT" --out "$RUN/agent.mp4" --size $SIZE > /dev/null 2>&1 || true
else
  echo "no out.promo produced" | tee "$RUN/score.txt"
fi
if [ -f "$SCENARIO" ]; then
  python3 "$HERE/scenario_check.py" "$DEMO" "$RUN" | tee "$RUN/scenario.txt"
fi
