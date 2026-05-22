#!/bin/bash
set -euo pipefail
wasm-pack build crates/mds-wasm --target nodejs --out-dir pkg
wasm-pack build crates/mds-wasm --target web --out-dir pkg-web
