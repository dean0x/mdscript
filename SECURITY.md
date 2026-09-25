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

- **Path-traversal prevention**: import paths are rejected if they resolve outside
  the project root (`..` traversal, or a symlinked parent directory leading out),
  and an `mds.json` `build.output_dir` containing any `..` component is refused
  (`mds::io`, exit 2). The project root
  is the nearest ancestor directory containing a `.git` or `.mdsroot` marker,
  found by walking upward from the compiled file's directory. Placing a `.mdsroot`
  file is therefore a security-relevant decision: it sets the containment boundary
  for all imports, and placing it closer to the input narrows that boundary while
  placing it farther away widens it.
- **Symlink rejection**: an entry file, import target or `--vars` file whose final
  path component is a symbolic link (on Windows also a junction) is refused, and so
  is a base directory passed to `ModuleCache::resolve_source*`. The check reads the
  file type of the final component itself (not following it), then requires its
  canonical path to lie in the canonical parent directory. Symbolic links in parent
  directories are followed and the result is then subject to containment.
- **Null-byte rejection**: paths containing NUL bytes are rejected at the API
  boundary rather than being passed to the OS.
- **Forbidden path characters are refused at input, not only escaped on output**
  (#265): a path carrying any of the 80 codepoints of `mds::is_forbidden_path_char`
  — every C0 control including TAB and LF, DEL, every C1 control, and the bidi,
  line/paragraph-separator and BOM hazards — is refused before the file it names is read:
  import strings (`mds::import`); entry paths, virtual entry keys, base
  directories and resolved canonical paths (`mds::io`); and on the CLI the
  `-o`/`--out-dir`/`build.output_dir` output locations and the `mds init`
  filename (`mds::io`, exit 2). `@mdscript/mds`'s WASM backend applies the same
  class in its JS pre-scanner before it reads a file. The resolver runs these
  checks before a `FileSystem` backend is called, so a custom backend passed to
  `ModuleCache::with_fs` is covered for every path its caller supplies; a path the
  backend produces itself (a key it rewrites, a link it follows) is its own
  responsibility, and its contract requires it to apply `mds::is_forbidden_path_char`.
  Output escaping stays in place as the second layer: diagnostics escape control,
  bidi and separator characters (spec §7.5 "Sanitization invariant"), so a hostile
  name that reaches one is displayed as `\uXXXX` text instead of being interpreted
  by the terminal.
- **Non-UTF-8 paths** are rejected at the public API boundary with an explicit
  error instead of producing corrupted output.
- **Path segment cap**: an entry path or import path of more than 256 segments is
  refused (`mds::resource_limit`) on both built-in backends.
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

The symlink, containment, NUL-byte, forbidden-character, path-encoding and
segment-count rules above are specified normatively — with their error codes, the
place each check runs, and the tests that pin them — in `spec.md` §4.6
"Filesystem constraints"; this section is the overview.

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
| Max path segments | 256 per entry or import path | `fs.rs` (`MAX_PATH_SEGMENTS`) |
| Max block nesting depth | 64 | `limits.rs` (`MAX_NESTING_DEPTH`) |
| Max @elseif branches per @if | 256 | `limits.rs` (`MAX_ELSEIF_BRANCHES`) |
| Max value (YAML/JSON) nesting depth | 64 | `value.rs` (`MAX_VALUE_DEPTH`) |
| Max dot-path segments | 32 | `limits.rs` (`MAX_DOT_SEGMENTS`) |

These guard against adversarial input causing stack overflow, unbounded memory
growth, or non-termination.

## ⚠️ The `debug-panics` feature must never ship enabled

The three binding crates — `mds-napi`, `mds-wasm` and `mds-python` — declare an
off-by-default `debug-panics` Cargo feature (`crates/mds-napi/Cargo.toml`,
`crates/mds-wasm/Cargo.toml`, `crates/mds-python/Cargo.toml`). `mds-core` and
`mds-cli` have no such feature: the CLI installs no panic hook, so a panic there is
a plain Rust panic (exit code 101) with no error object to attach a payload to. When
enabled, the feature surfaces the raw Rust panic payload as `err.detail` on
`mds::internal` errors thrown at the binding boundary, to help diagnose unexpected
panics during local development.

**Never enable `debug-panics` in a published or production build.** Panic messages
can contain absolute filesystem paths and other internal details that should not be
exposed to template authors or end users. The feature is off unless opted into
explicitly: none of the three crates lists it in a `default` feature set
(`mds-python`'s default is `extension-module` only), and the commands that build the
published artifacts — `napi build --release` in `release.yml` for the addon, the
`@mdscript/mds-wasm` build script (`wasm-pack build ../../crates/mds-wasm --target
nodejs …` and `--target web …`) for the WASM package, and `maturin` with
`pyproject.toml`'s `features = ["pyo3/abi3-py311"]` for the wheels — pass no
`--features debug-panics`. No automated gate asserts this; it is checked by reading
those three build sites.
