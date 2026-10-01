#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
rustup target add wasm32-unknown-unknown
cargo build --release -p world-viewer --target wasm32-unknown-unknown
version=$(awk '/name = "wasm-bindgen"/ {getline; gsub(/"/, "", $3); print $3; exit}' Cargo.lock)
if ! command -v wasm-bindgen >/dev/null || [[ $(wasm-bindgen --version) != "wasm-bindgen $version" ]]; then
  cargo install wasm-bindgen-cli --version "$version" --locked
fi
wasm-bindgen --target web --out-dir web/pkg --out-name world_viewer target/wasm32-unknown-unknown/release/world_viewer.wasm
echo "Visualiseur compilé dans web/pkg"
