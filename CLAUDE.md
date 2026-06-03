# MDS (Markdown Script)

Composable LLM prompt template compiler. Rust core (`crates/`) with WASM and native Node.js bindings, plus npm packages (`packages/`).

## Build and test

```bash
cargo test --workspace                        # 590+ Rust tests
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
npm ci && npm run build -w @mdscript/mds-wasm && npm run build --workspaces --if-present
npm test --workspaces --if-present
```

## Release

All packages ship as a single coordinated release at the same version. One command:

```bash
gh workflow run release.yml -f version=X.Y.Z
```

This bumps all manifests, stamps CHANGELOG, commits, tags, builds 7 native targets + WASM, publishes to crates.io and npm (with provenance), and creates a GitHub Release. Without the version input it runs a dry-run.

Manual alternative: `node scripts/bump-version.mjs X.Y.Z`, commit, `git tag vX.Y.Z`, push.

See @RELEASING.md for the full runbook.

## Gotchas

- Workspace panic strategy must stay `unwind` — catch_unwind at the JS boundary requires it
- `mds-wasm/Cargo.toml` has explicit (non-inherited) license/repo fields because older wasm-pack parsers fail on workspace inheritance
- aarch64 Linux cross-builds use system gcc (gnu) and zig (musl) instead of napi `--use-napi-cross` because the macOS-generated lockfile doesn't resolve `@napi-rs/tar` linux binaries
- `cargo publish -p mds-cli --dry-run` fails locally because mds-cli has a path+version dep on mds-core — this is expected; CI publishes mds-core first
- `scripts/verify-napi-names.mjs` (A3 gate) is critical — if the hand-written `crates/mds-napi/index.js` loader drifts from generated platform packages, the universal package silently fails to load native binaries at runtime
- `NPM_CONFIG_ACCESS=public` is required for first-time publishes of scoped `@mdscript/*` packages with provenance
- `debug-panics` Cargo feature must never ship enabled — it leaks filesystem paths in panic messages
- Local WASM builds require Binaryen v129+ for wasm-opt — `brew install binaryen` (macOS) or `apt install binaryen` (Linux)
