# MDS (Markdown Script)

Composable LLM prompt template compiler. Rust core (`crates/`) with WASM, native Node.js (napi-rs), and native Python (PyO3) bindings, plus npm packages (`packages/`).

## Build and test

```bash
cargo test --workspace                        # 590+ Rust tests
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p mds-core --no-deps   # CI rustdoc gate: catches broken intra-doc links
npm ci && npm run build -w @mdscript/mds-wasm && npm run build --workspaces --if-present
# The native addon is NOT built by `--workspaces` (its script is `build:native`, not
# `build`); needs the Rust toolchain. Without it @mdscript/mds silently falls back to WASM.
npm run build:native -w @mdscript/mds-napi
npm test --workspaces --if-present

# Python bindings (crates/mds-python) — 0 Rust tests by design; test via pytest:
python -m venv .venv && . .venv/bin/activate     # maturin develop needs a venv
pip install "maturin==1.13.3" pytest mypy pyright
maturin develop -m crates/mds-python/Cargo.toml && pytest crates/mds-python/tests -q
```

## Release

All packages ship as a single coordinated release at the same version, driven by
`release.yml`. **Release via tag-push:**

```bash
node scripts/bump-version.mjs X.Y.Z   # bump all manifests + stamp CHANGELOG
# land the bump on main via PR (CI-gated), then:
git tag -a vX.Y.Z -m vX.Y.Z && git push origin vX.Y.Z
```

Pushing the `vX.Y.Z` tag triggers `release.yml`: build 7 native targets + WASM,
A3 name-gate, publish to crates.io and npm (with provenance), create a GitHub Release.
Run `gh workflow run release.yml` (no inputs) for a dry-run that validates the build +
A3 gate and publishes nothing.

See @RELEASING.md for the full runbook.

## Gotchas

- Workspace panic strategy must stay `unwind` — catch_unwind at the JS/Python FFI boundary requires it
- `mds-wasm/Cargo.toml` has explicit (non-inherited) license/repo fields because older wasm-pack parsers fail on workspace inheritance
- Linux cross-builds: only x86_64-gnu uses napi `--use-napi-cross`; aarch64-gnu uses the apt cross gcc; BOTH musl legs use zig cc wrappers, because `--use-napi-cross` has no musl toolchain and napi forces `-C target-feature=-crt-static` on musl, so the host gcc silently links glibc (v0.1.0-v0.3.0 shipped a glibc-linked linux-x64-musl addon). release.yml gates every musl artifact with readelf.
- `cargo publish -p mds-cli --dry-run` fails locally because mds-cli has a path+version dep on mds-core — this is expected; CI publishes mds-core first
- `scripts/verify-napi-names.mjs` (A3 gate) is critical — if the hand-written `crates/mds-napi/index.js` loader drifts from generated platform packages, the universal package silently fails to load native binaries at runtime
- Stale `.node` files silently serve old behavior — `crates/mds-napi/` can hold multiple addon vintages (`mds-napi.node` from `build:native`, platform-suffixed `mds-napi.<triple>.node` from `napi build --platform`), and the test harness loads the base `mds-napi.node` by name. After any Rust change, rebuild with `npm run build:native -w @mdscript/mds-napi` or the suite exercises an old binary (PF-035)
- `NPM_CONFIG_ACCESS=public` is required for first-time publishes of scoped `@mdscript/*` packages with provenance
- `debug-panics` Cargo feature must never ship enabled (all three binding crates) — it attaches raw panic payloads (may contain filesystem paths) to errors
- `startup-race-probe` Cargo feature (`mds-cli`) must never ship enabled — it injects a 200 ms sleep into `mds watch` startup as the positive control for the arm-before-publish ordering (#317). It is non-default, but `mds-cli` publishes to crates.io, so `cargo install mds-cli --features startup-race-probe` would ship the delay to a user. It is exercised only by the `Watch startup race (probe)` CI job
- Local WASM builds do NOT require system Binaryen — wasm-pack auto-downloads wasm-opt (v117) into its cache on first use. Install Binaryen v129+ (`brew install binaryen` / `apt install binaryen`) only for offline builds, to override a stale wasm-opt on PATH, or to reproduce CI's exact release optimizer (the setup-wasm action pins v129)
- `crates/mds-python` (PyO3): test with **pytest, not `cargo test`** — 0 Rust tests by design (`[lib] test = false`). `abi3-py311` is always-on and `extension-module` is the default feature, so `cargo build/clippy/test --workspace` compile the cdylib without linking libpython; pyo3's abi3 forward-compat tolerates an older `python3` on PATH (repo default is 3.9)
- `crates/mds-python/build.rs` emits a cdylib-scoped `-undefined dynamic_lookup` so bare `cargo build` links the extension on macOS (Linux allows undefined cdylib symbols; maturin passes the flag itself when it builds the wheel)
- Local Python dev: `maturin develop` needs an active **virtualenv** + `python3` on PATH; CI has no venv so it uses `pip install ./crates/mds-python` (the maturin PEP 517 backend). Wheels are `cp311-abi3` (one per platform)
- `crates/mds-python` is free-threading ready (frozen result classes, `#[pymodule(gil_used = false)]`, GIL released around each compile); the `cp314t` free-threaded wheel is a separate ABI and is deferred with the wheel matrix + PyPI publishing (follow-up to #132)
- **Source hygiene gate** (#288): `node scripts/verify-no-control-bytes.mjs` scans tracked source for hazardous codepoints (C0, C1, bidi, BOM). BSD grep has no `-P` (exits 2, empty output reads as clean) — never use grep to verify absence of control bytes; the gate uses pure Node codepoint iteration. When writing codepoints in source or docs, use numeric notation (U+202E, 0x202e) rather than `\uXXXX` escapes — the edit tooling decodes 4-hex `\uXXXX` to live bytes (PF-018).
- **Pre-merge check verifier** (#289, PF-017): a CANCELLED GitHub Actions run reads as "not failing" to `gh pr merge --admin`, which can merge an unverified head. Before any `--admin` merge, run `node scripts/verify-pr-checks.mjs <pr-number>` and use the `gh pr merge --squash --admin --match-head-commit <sha>` command it emits verbatim. This verifies all required contexts are `completed+success` and pins the SHA.
