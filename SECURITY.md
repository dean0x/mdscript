# Security Policy

## Reporting a vulnerability

**Please do not report security vulnerabilities through public GitHub issues,
discussions, or pull requests.**

Report vulnerabilities privately through GitHub's
[private vulnerability reporting](https://github.com/dean0x/mdscript/security/advisories/new).
This routes the report to the maintainers privately and lets us collaborate on a
fix and coordinated disclosure.

Please include, where possible:

- A description of the issue and its impact
- The affected component (CLI, `mds-core`, WASM, native addon, a bundler plugin)
- Steps to reproduce, or a minimal `.mds` template / input that triggers it
- The version (crate or npm package) and your platform

We aim to acknowledge reports within a few days and will keep you updated as we
investigate.

## Supported versions

MDS is pre-1.0. Security fixes are applied to the latest released minor series
only; please upgrade to the newest release before reporting.

## Security model & built-in controls

MDS treats template sources, imported modules, and runtime variables as untrusted
input. The compiler enforces several defense-in-depth controls:

### Filesystem boundary (`crates/mds-core/src/fs.rs`, `resolver.rs`)

- **Path-traversal prevention**: import paths and the `output_dir` config value
  are rejected if they escape the project root (`..` traversal). The project root
  is the nearest ancestor directory containing a `.git` or `.mdsroot` marker,
  found by walking upward from the compiled file's directory. Placing a `.mdsroot`
  file is therefore a security-relevant decision: it sets the containment boundary
  for all imports, and placing it closer to the input narrows that boundary while
  placing it farther away widens it.
- **Symlink rejection**: symlinked import paths are refused. Resolution is
  written to be TOCTOU-safe (the resolved target is validated, not just the
  pre-resolution path).
- **Null-byte rejection**: paths containing NUL bytes are rejected at the API
  boundary rather than being passed to the OS.
- **Non-UTF-8 paths** are rejected at the public API boundary with an explicit
  error instead of producing corrupted output.
- **Source-map anchors are byte-faithful**: the project root and `source_map_base`
  used to decide whether a `sources[]` entry is inside the project are never
  derived from a lossy string. A root that is empty or not valid UTF-8 is treated
  as "no root" — entries degrade to basenames — so it can never make the
  containment check vacuous.
- **Replace-by-rename writes**: `mds fmt`, `mds lint --fix`, `mds init`, and `mds
  build`/`mds watch` outputs and `.map` sidecars are written to a same-directory
  temp file and renamed over the target after a final symlink re-check (`mds-cli/src/output.rs`,
  `atomic_write_file`; enforced by `crates/mds-cli/tests/write_funnel.rs`), so a
  crash never leaves a truncated target and a symlinked output path is refused.
  Consequence: hard links, ACLs, xattrs, and owner/group of a pre-existing target
  are not preserved (permission bits are, on Unix) — see spec §7.2 "Output writing".

### Resource limits

| Limit | Value | Location |
|-------|-------|----------|
| Max file size | 10 MB per source file (file, virtual module, or in-memory string) | `limits.rs` (`MAX_FILE_SIZE`) |
| Max frontmatter size | 1 MiB per block | `limits.rs` (`MAX_FRONTMATTER_SIZE`) |
| Max frontmatter YAML nodes | 200,000 per block (alias expansion counted) | `limits.rs` (`MAX_FRONTMATTER_NODES`) |
| Max frontmatter flow-nesting depth | 1024 (checked pre-parse) | `limits.rs` (`MAX_FRONTMATTER_FLOW_DEPTH`) |
| YAML parser nesting depth | 128 | serde_yaml_ng (reported as `mds::yaml`) |
| Max `mds.json` size | 1 MB | `mds-cli/src/main.rs` (`MAX_CONFIG_SIZE`) |
| Max call depth | 128 | `evaluator.rs` (`MAX_CALL_DEPTH`) |
| Max iterations per loop | 100,000 | `evaluator.rs` (`MAX_LOOP_ITERATIONS`) |
| Max total iterations | 1,000,000 | `evaluator.rs` (`MAX_TOTAL_ITERATIONS`) |
| Max output size | 50 MB | `evaluator.rs` (`MAX_OUTPUT_SIZE`) |
| Max warnings | 1,000 | `evaluator.rs` (`MAX_WARNINGS`) |
| Max import depth | 64 | `resolver.rs` (`MAX_IMPORT_DEPTH`) |
| Max block nesting depth | 64 | `limits.rs` (`MAX_NESTING_DEPTH`) |
| Max @elseif branches per @if | 256 | `limits.rs` (`MAX_ELSEIF_BRANCHES`) |
| Max value (YAML/JSON) nesting depth | 64 | `value.rs` (`MAX_VALUE_DEPTH`) |
| Max dot-path segments | 32 | `limits.rs` (`MAX_DOT_SEGMENTS`) |

These guard against adversarial input causing stack overflow, unbounded memory
growth, or non-termination.

## ⚠️ The `debug-panics` feature must never ship enabled

`mds-core`, `mds-wasm`, and `mds-napi` expose an off-by-default `debug-panics`
Cargo feature. It surfaces the raw Rust panic payload (as `err.detail` on
`mds::internal` errors) to help diagnose unexpected panics during local
development.

**Never enable `debug-panics` in a published or production build.** Panic
messages can contain absolute filesystem paths and other internal details that
should not be exposed to template authors or end users. All release builds and
published artifacts are built with the feature disabled.
