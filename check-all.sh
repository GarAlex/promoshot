#!/bin/bash
# promo-core: the whole truth in one command (fmt, lints, tests, release build).
set -euo pipefail
cd "$(dirname "$0")"
echo "== fmt ==";    cargo fmt --all --check
echo "== clippy =="; cargo clippy --workspace --all-targets -- -D warnings
echo "== test ==";   cargo test --workspace
echo "== release build =="; cargo build --workspace --release
echo "ALL GREEN"
