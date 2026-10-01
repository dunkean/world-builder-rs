#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
if ! command -v cargo >/dev/null; then
  echo "Rust absent : installez rustup depuis https://rustup.rs" >&2
  exit 1
fi
if [[ ! -f web/pkg/world_viewer_bg.wasm ]]; then
  ./scripts/build-web.sh
fi
if [[ ! -f data/latest ]]; then
  cargo run --release -p world-app -- preview --seed 42 --resolution 64
fi
exec cargo run --release -p world-app -- serve --host "${WORLD_HOST:-127.0.0.1}" --port "${WORLD_PORT:-8001}"
