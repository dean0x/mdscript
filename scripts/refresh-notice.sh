#!/bin/bash
set -euo pipefail

cargo_about=$1
notice_raw=$2

# Read dependency metadata for the NOTICE. Do not build any crates (#136).
"$cargo_about" about generate --frozen --workspace --all-features --fail \
  --target aarch64-apple-darwin --target x86_64-apple-darwin \
  --target x86_64-unknown-linux-gnu --target x86_64-unknown-linux-musl \
  --target aarch64-unknown-linux-gnu --target aarch64-unknown-linux-musl \
  --target x86_64-pc-windows-msvc --target wasm32-unknown-unknown \
  --config about.toml --output-file "$notice_raw" about.hbs
node scripts/generate-notice.mjs < "$notice_raw"
