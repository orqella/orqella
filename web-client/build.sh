#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/wasm"
cargo build --target wasm32-unknown-unknown --release
mkdir -p ../pkg
wasm-bindgen --target web --out-dir ../pkg --out-name orqella_web_client \
  target/wasm32-unknown-unknown/release/orqella_web_client_wasm.wasm
