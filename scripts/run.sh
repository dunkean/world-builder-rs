#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
# Driver is mounted here by WSL; NVRTC comes from the CUDA Toolkit.
export LD_LIBRARY_PATH="/usr/lib/wsl/lib:/usr/local/cuda/lib64:${LD_LIBRARY_PATH:-}"
if ! command -v cargo >/dev/null; then
  echo "Rust absent : installez rustup depuis https://rustup.rs" >&2
  exit 1
fi
# Incremental compilation prevents a stale WASM/API pair after an update.
./scripts/build-web.sh
if [[ ! -f data/latest ]]; then
  cargo run --release -p world-app -- preview --seed 42 --resolution 128
fi
exec cargo run --release -p world-app -- serve --host "${WORLD_HOST:-127.0.0.1}" --port "${WORLD_PORT:-8001}"
