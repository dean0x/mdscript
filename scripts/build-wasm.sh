#!/bin/bash
set -euo pipefail
if ! node "$(dirname -- "${BASH_SOURCE[0]}")/check-wasm-tools.mjs"; then
  printf '%s\n' 'WASM tool drift warning: could not run the Node check' >&2
fi
# One codegen unit for every crate in the WASM build, as in CI and the release (#491).
wasm-pack build crates/mds-wasm --target nodejs --out-dir pkg -- --config profile.release.codegen-units=1
wasm-pack build crates/mds-wasm --target web --out-dir pkg-web -- --config profile.release.codegen-units=1
