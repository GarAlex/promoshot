#!/bin/zsh
# Every demo in order, each a fresh agent, then a table in summary.md.
#   demos/run_all.sh [--skip-done] [model]
HERE="${0:A:h}"; MODEL=""; SKIP=0
for a in "$@"; do case "$a" in --skip-done) SKIP=1;; *) MODEL="$a";; esac; done
OUT="$HERE/summary.md"
{ echo "# Demo runs — $(date '+%Y-%m-%d %H:%M')"; echo; echo "| demo | score | turns | cost | secs |"; echo "|---|---|---|---|---|"; } > "$OUT"
for d in "$HERE"/[0-9][0-9]-*; do
  [ -d "$d" ] || continue
  if [ "$SKIP" = 1 ] && ls "$d"/runs/*/score.json > /dev/null 2>&1; then
    echo "== $(basename "$d"): scored already, skipping"
  else
    "$HERE/run.sh" "$d" $MODEL
  fi
  R=$(ls -d "$d"/runs/* 2>/dev/null | tail -1)
  SCORE=$(head -1 "$R/score.txt" 2>/dev/null | grep -oE '[0-9]+%' | head -1)
  SUM=$(cat "$R/summary.txt" 2>/dev/null)
  echo "| $(basename "$d") | ${SCORE:-—} | $(echo "$SUM" | grep -oE 'turns=[0-9]+' | cut -d= -f2) | $(echo "$SUM" | grep -oE 'cost=\$[0-9.]+' | cut -d= -f2) | $(echo "$SUM" | grep -oE 'secs=[0-9]+' | cut -d= -f2) |" >> "$OUT"
done
cat "$OUT"
