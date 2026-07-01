# MDS (Markdown Script)

Composable LLM prompt template compiler. Rust core (`crates/`) with WASM, native Node.js (napi-rs), and native Python (PyO3) bindings, plus npm packages (`packages/`).

## Build and test

```bash
cargo test --workspace                        # 590+ Rust tests
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
npm ci && npm run build -w @mdscript/mds-wasm && npm run build --workspaces --if-present
npm test --workspaces --if-present

# Python bindings (crates/mds-python) — 0 Rust tests by design; test via pytest:
python -m venv .venv && . .venv/bin/activate     # maturin develop needs a venv
pip install "maturin==1.13.3" pytest mypy pyright
maturin develop -m crates/mds-python/Cargo.toml && pytest crates/mds-python/tests -q
```

## Release

All packages ship as a single coordinated release at the same version, driven by
`release.yml`. **Release via tag-push** (the `workflow_dispatch -f version=` path is
currently blocked by branch protection — see #127):

```bash
node scripts/bump-version.mjs X.Y.Z   # bump all manifests + stamp CHANGELOG
# land the bump on main via PR (CI-gated), then:
git tag -a vX.Y.Z -m vX.Y.Z && git push origin vX.Y.Z
```

Pushing the `vX.Y.Z` tag triggers `release.yml` (prepare is skipped): build 7 native
targets + WASM, A3 name-gate, publish to crates.io and npm (with provenance), create a
GitHub Release. Run `gh workflow run release.yml` (no version) first for a dry-run that
validates the build + A3 gate and publishes nothing.

> The `workflow_dispatch -f version=X.Y.Z` "one command" path is **currently broken**
> (#127): its prepare job can't push the release commit to protected `main` (GH006),
> so it leaves an orphaned tag and publishes nothing. Use tag-push until #127 is fixed.

See @RELEASING.md for the full runbook.

## Gotchas

- Workspace panic strategy must stay `unwind` — catch_unwind at the JS boundary requires it
- `mds-wasm/Cargo.toml` has explicit (non-inherited) license/repo fields because older wasm-pack parsers fail on workspace inheritance
- aarch64 Linux cross-builds use system gcc (gnu) and zig (musl) instead of napi `--use-napi-cross` because the macOS-generated lockfile doesn't resolve `@napi-rs/tar` linux binaries
- `cargo publish -p mds-cli --dry-run` fails locally because mds-cli has a path+version dep on mds-core — this is expected; CI publishes mds-core first
- `scripts/verify-napi-names.mjs` (A3 gate) is critical — if the hand-written `crates/mds-napi/index.js` loader drifts from generated platform packages, the universal package silently fails to load native binaries at runtime
- `NPM_CONFIG_ACCESS=public` is required for first-time publishes of scoped `@mdscript/*` packages with provenance
- `debug-panics` Cargo feature must never ship enabled (all three binding crates) — it attaches raw panic payloads (may contain filesystem paths) to errors
- Local WASM builds require Binaryen v129+ for wasm-opt — `brew install binaryen` (macOS) or `apt install binaryen` (Linux)
- `crates/mds-python` (PyO3): test with **pytest, not `cargo test`** — 0 Rust tests by design (`[lib] test = false`). `abi3-py311` is always-on and `extension-module` is the default feature, so `cargo build/clippy/test --workspace` compile the cdylib without linking libpython; pyo3's abi3 forward-compat tolerates an older `python3` on PATH (repo default is 3.9)
- `crates/mds-python/build.rs` emits a cdylib-scoped `-undefined dynamic_lookup` so bare `cargo build` links the extension on macOS (Linux allows undefined cdylib symbols; maturin passes the flag itself when it builds the wheel)
- Local Python dev: `maturin develop` needs an active **virtualenv** + `python3` on PATH; CI has no venv so it uses `pip install ./crates/mds-python` (the maturin PEP 517 backend). Wheels are `cp311-abi3` (one per platform)
- `crates/mds-python` is free-threading ready (frozen result classes, `#[pymodule(gil_used = false)]`, GIL released around each compile); the `cp314t` free-threaded wheel is a separate ABI and is deferred with the wheel matrix + PyPI publishing (follow-up to #59)
