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
  directories are followed and the result is then subject to containment. The CLI
  checks the directory argument of `mds build`, `mds check`, `mds fmt`, `mds lint`
  and `mds watch` by the same rule before it is walked, however it is typed (`link/`
  and `link/.` included), and refuses a symlinked one, or the filesystem root, as
  `mds::io`, exit 2 (#413). `mds watch` re-checks the path it watches before every
  rebuild — the entry in file mode, the directory argument and each source below
  it in directory mode — and refuses the rebuild once a symbolic link on that path
  has been retargeted (#417, #413).
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
  error instead of producing corrupted output. A resolved path that is not valid
  UTF-8 — reached through a symbolic link into such a directory — is refused too,
  never turned into a lossy key, which would name a different, unchecked file.
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
  `mds build` and `mds watch` refuse an output that is the entry file itself —
  however `-o`, `--out-dir`, `build.output_dir` or the default output name it —
  before anything is written or any directory created (`mds::io`, exit 2, #425).
  Consequence: hard links, ACLs, xattrs, and owner/group of a pre-existing target
  are not preserved (permission bits are, on Unix) — see spec §7.2 "Output writing".

The symlink, containment, NUL-byte, forbidden-character, path-encoding and
segment-count rules above are specified normatively — with their error codes, the
place each check runs, and the tests that pin them — in `spec.md` §4.6
"Filesystem constraints"; this section is the overview.

### Resource limits

| Limit | Value | Location |
|-------|-------|----------|
| Max file size | 10 MiB (10,485,760 bytes) per source file (file, virtual module, or in-memory string); a file is never read more than one byte past it — one over it when it is opened is refused unread, and one that grows past it while it is read is read no further ([#428](https://github.com/dean0x/mdscript/issues/428)) | `limits.rs` (`MAX_FILE_SIZE`) |
| Max frontmatter size | 1 MiB per block | `limits.rs` (`MAX_FRONTMATTER_SIZE`) |
| Max frontmatter YAML nodes | 200,000 per block (alias expansion counted) | `limits.rs` (`MAX_FRONTMATTER_NODES`) |
| Max frontmatter flow-nesting depth | 1024 (checked pre-parse) | `limits.rs` (`MAX_FRONTMATTER_FLOW_DEPTH`) |
| YAML parser nesting depth | 128 | serde_yaml_ng (reported as `mds::yaml`) |
| Max `mds.json` size | 1 MiB (1,048,576 bytes), read no more than one byte past it | `mds-cli/src/build.rs` (`MAX_CONFIG_SIZE`) |
| Max call depth | 128 | `evaluator.rs` (`MAX_CALL_DEPTH`) |
| Max iterations per loop | 100,000 | `evaluator.rs` (`MAX_LOOP_ITERATIONS`) |
| Max total iterations | 1,000,000 | `evaluator.rs` (`MAX_TOTAL_ITERATIONS`) |
| Max output size | 50 MiB (52,428,800 bytes) per output buffer, checked before every append; it does not bound a compile's total memory, since nested blocks, function results and imported modules each hold a buffer of their own ([#420](https://github.com/dean0x/mdscript/issues/420)) | `limits.rs` (`MAX_OUTPUT_SIZE`) |
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

The three binding crates — `mds-napi`, `mds-wasm` and `mds-python` — and `mds-cli`
declare an off-by-default `debug-panics` Cargo feature (`crates/mds-napi/Cargo.toml`,
`crates/mds-wasm/Cargo.toml`, `crates/mds-python/Cargo.toml`,
`crates/mds-cli/Cargo.toml`). `mds-core` has none. In a binding the feature surfaces
the raw Rust panic payload as `err.detail` on `mds::internal` errors thrown at the
binding boundary; in the CLI it prints the panic's message and location after the
internal-compiler-error text below. Both exist to help diagnose unexpected panics
during local development.

**The CLI's panic output (#389).** A panic in `mds` prints these two lines on stderr and
nothing else, and the run exits 101:

```
mds: internal compiler error
note: this is a bug in mds; please report it at https://github.com/dean0x/mdscript/issues
```

The panic's message and its source location are never shown: a message can carry a
template author's text or the build machine's absolute paths. The two lines are one
fixed text, written once with its write error ignored, so a closed or failing stderr
loses the text and the run still exits 101, not by a signal. A panic on a thread other
than the one running the command — a `mds watch` helper thread, say — ends the process
at once with the same text and exit 101, and so does a second panic while the first is
still unwinding, which Rust would otherwise turn into an abort. Exit 101 wins over every
other exit code, `mds watch`'s included. With `RUST_BACKTRACE` set to anything but `0`
(`full` for every frame), a backtrace of the panicking thread follows the two lines,
each line escaped as a status line is. It still shows no message, but its frames name
the functions and source files the binary was built from, with the build machine's
paths; leave `RUST_BACKTRACE` unset where that matters. A panic that Rust cannot unwind
at all — one of the undefined-behaviour checks a debug build compiles in — prints the
text and then aborts. `crates/mds-cli/tests/panic_hook.rs` pins this output, and pins
the panic hook's code to one write of the fixed text.

**Never enable `debug-panics` in a published or production build.** Panic messages
can contain absolute filesystem paths and other internal details that should not be
exposed to template authors or end users. The feature is off unless opted into
explicitly: none of the four crates lists it in a `default` feature set
(`mds-python`'s default is `extension-module` only), and the commands that build the
published artifacts — `napi build --release` in `release.yml` for the addon, the
`@mdscript/mds-wasm` build script (`wasm-pack build ../../crates/mds-wasm --target
nodejs …` and `--target web …`) for the WASM package, and `maturin` with
`pyproject.toml`'s `features = ["pyo3/abi3-py311"]` for the wheels — pass no
`--features debug-panics`. For `mds-cli` alone, `crates/mds-cli/tests/panic_hook.rs`
fails when a `default` feature or any other feature turns it on; the build sites are
checked by reading them. `mds-cli` is published to crates.io, so
`cargo install mds-cli --features debug-panics` builds a CLI that prints panic
messages: build one only for your own debugging.
