#!/bin/sh
# Put the pair from THIS checkout on PATH — for working locally, where the
# working tree is the install. Builds target/release/promo and
# promoshot-mcp and links them into ~/.local/bin (or --into DIR), so every
# `cargo build --release` is already what runs. Nothing is downloaded: the
# release page and the image are the routes for everyone else (README,
# "Connect an agent").
#
#   scripts/install-local.sh [--into DIR] [--copy] [--no-build]
#
#   --into DIR   where the links go (default ~/.local/bin)
#   --copy       copy the binaries instead of linking them (stable until
#                the next run, instead of following the build)
#   --no-build   link what target/release already holds
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
into="$HOME/.local/bin"
mode=link
build=yes
while [ $# -gt 0 ]; do
  case "$1" in
    --into) into="$2"; shift 2 ;;
    --copy) mode=copy; shift ;;
    --no-build) build=no; shift ;;
    -h|--help) sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "install-local: unknown argument $1" >&2; exit 2 ;;
  esac
done
if [ "$build" = yes ]; then
  (cd "$here" && cargo build --release -p promo-cli -p promoshot-mcp)
fi
mkdir -p "$into"
for bin in promo promoshot-mcp; do
  src="$here/target/release/$bin"
  [ -x "$src" ] || { echo "install-local: $src is missing — build first" >&2; exit 1; }
  rm -f "$into/$bin"
  if [ "$mode" = link ]; then ln -s "$src" "$into/$bin"; else cp "$src" "$into/$bin"; fi
  echo "$into/$bin -> $src"
done
case ":$PATH:" in
  *":$into:"*) ;;
  *) echo "note: $into is not on PATH — add it in your shell profile" ;;
esac
for bin in promo promoshot-mcp; do
  found=$(command -v "$bin" 2>/dev/null || true)
  if [ -n "$found" ] && [ "$found" != "$into/$bin" ]; then
    echo "note: $bin on PATH resolves to $found, ahead of $into/$bin"
  fi
done
echo "Claude Code, headless:  claude mcp add promoshot-headless -- $into/promoshot-mcp"
