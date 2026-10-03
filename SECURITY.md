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
- **Replace-by-rename writes, below an anchor**: `mds fmt`, `mds lint --fix`, `mds init`,
  and `mds build`/`mds watch` outputs and `.map` sidecars are written to a
  same-directory temp file and renamed over the target (`mds-cli/src/write.rs`,
  `atomic_write_file`; enforced by `crates/mds-cli/tests/write_funnel.rs`), so a
  crash never leaves a truncated target. Every write is made below an anchor —
  the parent of a file argument or of `-o`, `--out-dir`, a directory argument's root,
  or, for `build.output_dir`, the directory that contains `mds.json` — which is
  resolved by path, so a symlinked anchor the user named is followed: as typed, or,
  for a directory-mode `--out-dir` and the anchors `mds watch` derives from its
  entry or directory argument, as resolved once when the run starts. `mds watch`
  checks its out-dir before each write below it: a write whose out-dir, as the user
  named it, now leads to a different directory than at startup — a symlink
  retargeted, the out-dir replaced by a link — is refused (`mds::io`, restart to
  follow it) rather than followed, while a deleted out-dir is created again at the
  same path. The check goes by path, and so does the write's open of its anchor, so
  the check also finds the directory the write is anchored at — the out-dir, the
  nearest directory above it once it has been deleted (the out-dir is then created
  below that one without following a symlink), or, for `build.output_dir`, the
  directory that contains `mds.json` — and the write refuses an anchor it opens that
  is another directory, and one gone by then, which it does not make again: on Unix
  compared on the descriptor it opened, so a link swapped onto the path between the
  check and the write is refused rather than followed. Nothing below the anchor is
  followed (#160): on Unix each directory below the anchor is opened from the one above
  without following a symlink (`openat` with `O_NOFOLLOW`), and the temp file is
  created (`O_CREAT | O_EXCL | O_NOFOLLOW`), given a replaced file's mode on its own
  descriptor, and renamed (`renameat`) in the last one, so a symlink below the anchor
  — planted before the run or swapped in while a write runs — and a symlink at the
  target are refused (`mds::io`, exit 2) and never written through; one that appears
  at the target during the write is replaced by the rename, not followed. On Unix a
  FIFO, a socket or a device at the target is refused (`not a regular file`) without
  being opened. `build.output_dir` comes from the repository, not from the user: it must be
  a relative path (an absolute one is refused before anything is written), and its own
  directories lie below the anchor, so a symlink committed there, such as
  `dist -> ~/elsewhere`, is refused rather than followed.
  `mds fmt` and `mds lint --fix` write only over the bytes they read (#160): before a
  rewrite the file is read again below its anchor, without following a symlink, and
  must hold the bytes formatted or fixed; on Unix the directory it is in stays open and
  the rewrite is renamed into that directory, whatever its path leads to by then, so a
  directory swapped after the read never receives another file's content. Just before
  the rename the file is looked at again in that directory and compared with a stamp
  taken when it was read — on Unix device and inode, size, and modification and
  status-change times; on Windows size, and modification and creation times — and a
  file edited in between is left as edited and the rewrite refused
  (`mds::io`, `"<path>" changed since it was read; not written`). Residuals: an edit
  that lands between that comparison and the rename is replaced; on a filesystem whose
  clock is coarser than the time between two writes (one-second timestamps, say), an
  edit that keeps the file's size within the same tick is not seen; and on Windows the
  second read, the comparison and the rename all go by path, and an edit that keeps the
  file's size and sets its modification time back is not seen.
  `mds init` without `--force` never replaces a file (#160): the starter is given its
  name only where nothing has it at that moment — by a rename that never replaces on
  Linux, Android and Apple platforms, else by a hard link, and on Windows by a move
  without replace — so a file that appears after its existence check is left as it is
  and refused as one already there is; on a filesystem without hard links the starter
  is written in place into a file created exclusively, which keeps that guarantee but
  not atomicity.
  A stale output, a stale `.map` sidecar and, in `mds watch`, a deleted source's output
  are removed below their anchor in the same way (#160), only as a regular file, never
  through a symlink nor one at the file, and, in `mds watch`, only below the out-dir it
  checked: on Unix the file is opened in the directory the walk reached without
  following a symlink, proven, and unlinked (`unlinkat`) from that directory only while
  its name is still the device and inode opened. `mds watch` removes an output — of a
  deleted source, or of the old kind after a change of kind — only when the session
  wrote that file and it still holds exactly the bytes written (#160): a file it did not
  write, hand-written or left by another run, and one changed since are kept with a
  notice. The proof is the content, so a file holding exactly those bytes is removed as
  the session's own. After a change of kind, `mds watch` writes the new kind's output
  only where nothing has that name at the commit — the same never-replacing commit as
  `mds init` — or over the file the session wrote there while it still holds exactly
  those bytes, by the same stamped replace as `mds fmt` (#160): anything else there,
  hand-written, left by another run or changed since, is kept with a notice.
  `mds build <dir>` removes the stale `.json` of a source whose kind
  changed only when it holds exactly what mds writes for a messages output — read
  no further than 10 MiB, parsed and written back to the same bytes — and never a stale
  `.md`: anything else at that name, a symlink, a FIFO or a directory included, is kept
  with a warning (#160). The proof is the content here too: a file holding exactly a
  messages output, copied there by hand, is removed as mds's own.
  **Windows residual**: the standard library has no descriptor-relative walk on
  Windows. Each directory below the anchor is checked and refused when it is a
  symlink or a junction, and the write — or the removal — then goes by path, so a
  directory that another process replaces with a link between that check and the
  write is followed; a link
  in place before the write is refused. The anchor `mds watch` checked is compared by
  path there, just before that walk, so a link swapped onto the out-dir's path after
  the comparison is followed too. Other reparse points, such as a cloud-sync
  placeholder, are written to.
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

**The CLI's panic output (#389).** A panic in `mds` is reported with these two lines on
stderr, and the run exits 101:

```
mds: internal compiler error
note: this is a bug in mds; please report it at https://github.com/dean0x/mdscript/issues
```

The panic's message and its source location are never shown: a message can carry a
template author's text or the build machine's absolute paths. The two lines are one
fixed text, printed once per panic and written with its write error ignored, so a closed
or failing stderr loses the text and the run still exits 101, not by a signal.

A panic compiling one file of a batch — a file of `mds build`, `mds check`, `mds fmt` or
`mds lint` given a directory, or any compile of an `mds watch` session — is caught, and
the batch goes on without that file. Only the file's compile, format or analysis runs
inside the catch, not the write or delete of an output, so what the panic abandons is
the compile's own work. The file counts as failed in the run's summary (`mds lint`:
under "with errors"), and the run exits 101 once it has finished; `mds watch` keeps
watching, rebuilds on the next edit, and exits 101 when it is stopped. A compile that
panics again prints the two lines again. No error line names the file.
`mds lint --format json` records it as a directory entry whose error is
`{"code": "mds::internal", "message": "internal compiler error", "help": null, "span": null}`;
for a panic in the `--fix` pipeline of a file argument, the run's one error document
carries that error. Neither holds anything of the panic itself.

A panic anywhere else on the thread running the command ends the run: it unwinds to the
end, which removes an output's temporary file on the way, and exits 101. A panic on
another thread — `mds watch`'s file-event callback or its Ctrl-C handler, each on a
thread of its own — ends the process at once with the same text and exit 101, and so
does a second panic while the first is still unwinding, which Rust would otherwise turn
into an abort. Ending the process at once runs no destructors, so an output being
written at that moment can be left part-way: its temporary file (`.mds-tmp-….tmp`) can
stay beside it, and stdout's reader can get part of the product. Exit 101 wins over
every other exit code, `mds watch`'s included.

With `RUST_BACKTRACE` set to anything but `0`, a backtrace of the panicking thread
follows the two lines — the same frames for every value, from where the hook captured
them, with each frame's address added for `full` — each line escaped as a status line
is. It still shows no message, but its frames name the functions and source files the
binary was built from, with the build machine's paths and the Rust toolchain's; leave
`RUST_BACKTRACE` unset where that matters. A panic that Rust cannot unwind at all and
that follows no other — one of the undefined-behaviour checks a build with debug
assertions compiles in — can print the text and then abort.
`crates/mds-cli/tests/panic_hook.rs` pins this output, and pins the panic hook's code to
one write of the fixed text. It also pins where each per-file catch sits, and that each
wraps one compile call which, through the functions of `mds-cli` it calls, changes no
file or directory, writes nothing to stdout and does not end the process. That check is
lexical: it follows calls by name, and does not see into a macro, `mds-core`, or a call
through a function pointer or a trait object.

A build with debug assertions — `cargo install --debug`, or a profile that turns them
on — also compiles in a test-only trigger: with the environment variable
`MDS_TEST_PANIC` set to one of these values, `mds` panics on purpose, to exercise the
output above:

- `main`: in the command's dispatch;
- `thread`: in a thread the dispatch starts and waits for;
- `compile:<file stem>`: in the per-file catch, compiling a file with that stem;
- `notify`: in `mds watch`'s file-event callback;
- `ctrlc`: in `mds watch`'s Ctrl-C handler.

The trigger is compiled only under `cfg(debug_assertions)`, which `panic_hook.rs` pins.
A release build therefore does not contain it — the variable is never read — unless its
profile turns debug assertions on. Build with debug assertions for development, not for
anything you ship.

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
fails when a line of its manifest names the feature in quotes outside a comment — how a
`default` feature or any other feature turns it on, on one line or several — or when the
manifest declares it as anything but `debug-panics = []`; the build sites are checked by
reading them. `mds-cli` is published to crates.io, so
`cargo install mds-cli --features debug-panics` builds a CLI that prints panic
messages: build one only for your own debugging.
