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

MDS treats template sources, imported modules, local data files (`data:` entries,
[spec §4.12](spec.md#412-data-sources-data)), and runtime variables as untrusted
input. The compiler enforces several defense-in-depth controls:

### Filesystem boundary (`crates/mds-core/src/fs.rs`, `resolver/import_paths.rs`, `resolver/imports.rs`, `resolver/extends.rs`)

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
  `mds::io`, exit 2 (#413).
- **Native source reads**: `NativeFs::read` accepts only a key that the same instance
  resolved as an entry or import. Before it reads bytes, it compares the opened file
  with that resolve record: device and inode on Unix, the handle's final path on Windows.
  A mismatch returns `mds::io`: `cannot read <display>: the file changed while it was opened`.
  An absent or unreadable record returns `mds::io`: `cannot read <display>: it was not resolved first`.
  Other targets refuse native source reads. `mds::lint` applies its rules to the entry text
  that the check gate read. Imported-module reads can still select a same-named file from
  another directory. On Windows, a replacement regular file at the same path is still read.
  Some overlay or network filesystems report different identities and refuse the read.
  A source open can still wait indefinitely for a named pipe.
- **Local data files**: a `data:` file path must start with `./` or `../` and end in exactly
  `.json`, `.yaml` or `.yml`. It must stay inside the project root, and its final component
  must not be a symbolic link. No segment of it may start with `.` (other than `.` and `..`)
  or be `node_modules`, in any ASCII case. MDS checks these segment rules again on the
  resolved path below the project root. The native read does not wait on a named pipe, and
  it reads only the regular file that resolution found, at most 10 MiB of UTF-8
  (`crates/mds-core/src/data/path.rs`; tests in `crates/mds-core/src/data/path_tests.rs`
  and `crates/mds-core/tests/data_paths.rs`).
  In `@mdscript/mds`, the WASM backend of `compileFile` applies the same rules before it reads
  a data file, and `checkFile` and `lintFile` read no data file
  (`packages/mds/__test__/data-scanner.spec.mjs`).
  A template can output any `.json`, `.yaml` or `.yml` file that these rules allow below the
  project root, the nearest directory with `.git` or `.mdsroot`.
  `mds check`, `mds lint`, `mds fmt` and the check and lint calls of the Rust API, the Python
  binding, the napi binding and the WASM binding read no data file (tests: `D-6` in
  `crates/mds-napi/__test__/index.spec.mjs`, `check_and_lint_read_no_data_file` in
  `crates/mds-wasm/tests/web.rs`). This release sends no network request: a URL entry fails
  with `mds::network_denied` unless a runtime value replaces it. `mds build` and `mds watch`
  never write an output over a data file that the run reads (`mds::io`, exit 2).
- **Directory and watch entries**: a directory build, check, lint or fmt compares each
  entry with its walked canonical path, plus the walk's device and inode on Unix.
  `mds watch` compares the directory argument with its watched canonical directory before each rebuild.
  It compares each entry or source's canonical parent with its watched parent before the compile
  and again during entry resolution. A case-only file-name change within that parent is not a move.
  A path lookup error, a different root or a different parent refuses the compile.
  Watch reports the rebuild error and keeps the session active.
  Watch uses the parent path, not a stored file identity; same-parent replacements remain possible.
- **Vars file reads**: `--vars` accepts a regular file or a named pipe, with the same 10 MiB cap.
  Regular files use the opened-file comparison above. Named pipes must have that type at lookup and open;
  their device and inode are not compared. Other types and type mismatches return `mds::io`.
  Process substitution works on macOS; Linux refuses `/dev/fd` links.
  A watch vars path can still read a same-named file from another directory.
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
  `atomic_write_file`; enforced by `crates/mds-cli/tests/write_funnel.rs`), so on a
  local filesystem a crash or kill of the run never leaves a truncated target (a
  network or FUSE filesystem can report a full disk only when the file is closed, which
  the write does not check). Every write is made below an anchor —
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
  without following a symlink (`openat` with `O_NOFOLLOW`) — for search alone
  (`O_PATH`) on Linux, and to read on other Unix systems, so there a directory the user
  may write to but not list refuses the write — and the temp file is
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
  directory swapped after the read never receives another file's content. That
  directory is then synced, which makes the rename durable — at once for a file
  argument, and in a directory run once for all its rewrites there, when the run is past
  the last of its files, the directory held open until then — so the sync is always made
  on the directory the renames went into, never on one swapped in at its path, and a
  rewrite's `Formatted:` or `Fixed:` line is shown only after it. Just before
  the rename the file is looked at again in that directory and compared with a stamp
  taken when it was read — on Unix device and inode, size, and modification and
  status-change times; on Windows size, and modification and creation times — and a
  file edited in between is left as edited and the rewrite refused
  (`mds::io`, `cannot write <path>: changed since it was read; not written`). Residuals: an edit
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
  not atomicity: a write that fails part-way removes the file again while its name is
  still the file it created, but a crash or kill while it is written can leave it
  partly written. `mds watch`'s write of a source's output after a change of kind
  (below) uses the same commit, with the same residual.
  A stale output, a stale `.map` sidecar and, in `mds watch`, a deleted source's output
  are removed below their anchor in the same way (#160), only as a regular file, never
  through a symlink nor one at the file, and, in `mds watch`, only below the out-dir it
  checked: on Unix the file is opened in the directory the walk reached without
  following a symlink, proven, and unlinked (`unlinkat`) from that directory only while
  its name is still the file opened, unchanged — the same device and inode, size, and
  modification and status-change times, so the same file written over after the proof
  read it is kept; the residuals are a rewrite's: a change in the instant between that
  last look and the removal, and on a filesystem whose clock is coarser than the time
  between the read and an edit (one-second timestamps, say), an edit that keeps the
  file's size. `mds watch` removes an output — of a
  deleted source, or of the old kind after a change of kind — only when the session
  last wrote that file for that source and it still holds exactly the bytes written
  (#160): a file it did not write, hand-written or left by another run, one it wrote for
  another source of the same output name, and one changed since are kept with a
  notice. The proof is the content, so a file holding exactly those bytes is removed as
  the session's own. After a change of kind, `mds watch` writes the new kind's output
  only where nothing has that name at the commit — the same never-replacing commit as
  `mds init` — or over the file the session wrote there for that source while it still
  holds exactly those bytes, by the same stamped replace as `mds fmt` (#160): anything
  else there, hand-written, left by another run, written for another source or changed
  since, is kept with a notice.
  `mds build <dir>` removes the stale `.json` of a source whose kind
  changed below an out-dir only — beside its sources, where another source's output
  can have that name, it looks at none — and only when it holds exactly what mds
  writes for a messages output — read
  no further than 10 MiB, parsed and written back to the same bytes — and never a stale
  `.md`: anything else at that name, a symlink, a FIFO or a directory included, is kept
  with a warning (#160). The proof is the content here too: a file holding exactly a
  messages output, copied there by hand, is removed as mds's own.
  **Windows residual**: the standard library has no descriptor-relative walk on
  Windows. Each directory below the anchor is checked and refused when it is a
  symlink or a junction, and the write — or the removal — then goes by path, so a
  directory that another process replaces with a link between that check and the
  write is followed; a link
  in place before the write is refused. A file to be removed is proven, closed again,
  looked at by path — the same size, and modification and creation times, or it is
  kept — and then removed by its name, so a file another process puts at that name
  after that look, or one with the same size and times, is removed in its place. The
  anchor `mds watch` checked is compared by
  path there, just before that walk, so a link swapped onto the out-dir's path after
  the comparison is followed too. Other reparse points, such as a cloud-sync
  placeholder, are written to.
  `mds build` and `mds watch` refuse an output that is the entry file itself —
  however `-o`, `--out-dir`, `build.output_dir` or the default output name it —
  before anything is written or any directory created (`mds::io`, exit 2, #425).
  Nor do they write an output over an MDS module — a `.mds` file, or a `.md` file
  whose frontmatter declares `type: mds`, which a template can import — or over a
  file the run reads: the entry, an imported module, the `--vars` file, the
  `mds.json` in force — in directory mode also the `mds.json` nearest each source,
  which a file-mode build of it would hold in force.
  In directory mode, this input rule applies per source: its own compile's entry and imported modules.
  The `--vars` file and the `mds.json` files named above count too.
  Just before the rename the
  file at the output path is looked at in the directory the write walked to: a
  `.mds` file is a module by its name, never read, and a `.md` file is read as
  opened there (on Windows, by path); it is compared with each file the run reads,
  looked up by its path then — on Unix by device and inode, on Windows by canonical
  path, so a hard link to an input is written over there. A module, a `.md` file that cannot be read to tell, and a
  file the run reads are refused and left as they are (`mds::io`, #425). One put
  there in the instant between that look and the rename is replaced. An output that
  itself declares `type: mds` — a module a template generates — replaces a module,
  its own or one written by hand, though never a file the run reads.
  A generator may replace a module that another source of the same directory run imports.
  A source compiled before the generator reads the module as it was. On Windows a
  name holding a `:` — NTFS stream syntax, `lib.mds::$DATA` being `lib.mds` itself —
  is refused before anything is written (`invalid filename`), so no write reaches a
  module by a name the check does not take for one.
  Consequence: hard links, ACLs, xattrs, and owner/group of a pre-existing target
  are not preserved (permission bits are, on Unix — read, write and execute alone,
  never a setuid, setgid or sticky bit, and only from a file the user running mds owns:
  one another user owns lends none, so a file another user plants at an output path in
  a shared directory cannot choose the mode of the output that replaces it) — see spec
  §7.2 "Output writing".

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
| Max `mds.json` size | 1 MiB (1,048,576 bytes), read no more than one byte past it | `mds-cli/src/build/config.rs` (`MAX_CONFIG_SIZE`) |
| Max call depth | 128 | `evaluator.rs` (`MAX_CALL_DEPTH`) |
| Max total function calls | 16,000,000 per compile, across all evaluated modules | `limits.rs` (`MAX_TOTAL_FUNCTION_CALLS`) |
| Max iterations per loop | 100,000 | `evaluator.rs` (`MAX_LOOP_ITERATIONS`) |
| Max total iterations | 1,000,000 per module evaluation | `evaluator.rs` (`MAX_TOTAL_ITERATIONS`) |
| Max output size | 50 MiB (52,428,800 bytes) per evaluation output buffer. Function results, message content and imported modules can use separate buffers; the compile memory budget below bounds them together. | `limits.rs` (`MAX_OUTPUT_SIZE`) |
| Max compile memory | 200 MiB (209,715,200 bytes, four output buffers) per compile ([#420](https://github.com/dean0x/mdscript/issues/420), [#488](https://github.com/dean0x/mdscript/issues/488)). One budget counts output buffers by capacity, values that evaluation makes, an estimate of the syntax tree of each module (charged before the parse, by a compile, a check and an import scan), the compiler's copy of the runtime variables, retained module text and frontmatter trees (the merged trees of `@extends` bases included), kept source-map segments, warnings, messages and the finalize output. Exactly the budget passes; above it, `resource limit exceeded: compile memory exceeds maximum of 209715200 bytes`. A reused `ModuleCache` keeps the count of its cached modules until it is dropped, so it fills faster. The tree estimate and its ceilings are below the table. | `limits.rs` (`MAX_COMPILE_MEMORY`, `TREE_PER_TOKEN`), `evaluator/budget.rs` |
| Frontmatter byte allowance | 50 MiB (52,428,800 bytes) per frontmatter parse, or the rest of the compile budget when that is smaller. Each YAML node spends 192 bytes plus its string or key bytes before it is built, so an alias of a long scalar spends the scalar again at each use. The one exception is the bytes of a `!tag`: the parser spends them after it copies the tag. Above it, `resource limit exceeded: frontmatter YAML expands beyond maximum of 52428800 bytes`, or the compile memory message when the rest of the budget set the allowance. | `resolver/frontmatter.rs` (`NODE_YAML`, `Allowance`) |
| Max warnings | 1,000 | `evaluator.rs` (`MAX_WARNINGS`) |
| Max import depth | 64 | `resolver.rs` (`MAX_IMPORT_DEPTH`) |
| Max module count | 256 distinct modules besides the entry; see [spec §4.1](spec.md#41-variables-yaml-frontmatter) for the count rule and virtual-map bound | `limits.rs` (`MAX_MODULE_COUNT`), `resolver/charges.rs` |
| Max path segments | 256 per entry or import path | `fs.rs` (`MAX_PATH_SEGMENTS`) |
| Max block nesting depth | 64 | `limits.rs` (`MAX_NESTING_DEPTH`) |
| Max `@define` declarations per module | 256 | `limits.rs` (`MAX_DEFINES_PER_MODULE`) |
| Max @elseif branches per @if | 256 | `limits.rs` (`MAX_ELSEIF_BRANCHES`) |
| Max value (YAML/JSON) nesting depth | 64 | `value.rs` (`MAX_VALUE_DEPTH`) |
| Max dot-path segments | 32 | `limits.rs` (`MAX_DOT_SEGMENTS`) |
| Max data file size | 10 MiB (10,485,760 bytes) per local data file | `limits.rs` (`MAX_DATA_FILE_BYTES`) |
| Max data file nodes | 200,000 per data file (YAML alias expansion counted) | `limits.rs` (`MAX_DATA_NODES_PER_FILE`) |
| Max data files | 256 distinct data files per compile | `limits.rs` (`MAX_DATA_FILES`) |
| Max data entries | 64 per module, 256 per compile | `limits.rs` (`MAX_DATA_ENTRIES_PER_MODULE`, `MAX_DATA_ENTRIES_PER_COMPILE`) |
| Max data references | 256 per data entry, in its URL, headers and body together, before the data graph check | `limits.rs` (`MAX_DATA_REFS_PER_ENTRY`) |
| Max data graph depth | 16 levels of references between data entries | `limits.rs` (`MAX_DATA_LEVELS`) |
| Max data text | 32 MiB (33,554,432 bytes) of data file text per compile, and of data text in a binding's virtual module map | `limits.rs` (`MAX_DATA_BYTES_PER_COMPILE`) |

The data limits and their messages are in [spec §4.1](spec.md#41-variables-yaml-frontmatter)
"Local data limits". A data file also counts toward the compile memory budget: the budget
keeps the converted value of each distinct data file once per compile.

These limits apply to the quantities and scopes in the table. They do not bound
elapsed time.

In a compile or a check, the compile memory budget counts an estimate of the syntax tree
of each module: a cached import, an `@extends` base, the file entry and the string
entry. The estimate is `tokens × 832 + expression bytes × 184 + source bytes × 16`
bytes, in checked arithmetic. Expression bytes are the text of each directive line and
each interpolation. The compiler charges the estimate after the lexer and before the
parse, so it never builds a tree that does not fit. An import scan (`scan_imports`,
`scan_import_records` and the WASM file pre-scan of `@mdscript/mds`) charges the same
estimate against its own new budget, so a scan of a very large or dense module stops
with `mds::resource_limit`. A cached module keeps its estimate
charged. A module resolved first as an `@extends` base and then as an import keeps one
estimate. The factors are measured on a 64-bit host: for each of the 65 shapes in
`crates/mds-core/tests/memory_bound.rs`, the estimate is at least the compile peak and
the bytes that a cached module keeps. On wasm32 the estimate is high, so a compile is
refused earlier. The table gives about the largest source of each shape for a module
compiled alone, with no runtime variables:

| Source shape | One compile | Reused `ModuleCache`, all cached sources |
|---|---|---|
| The densest syntax, `\{{a` repeated | 484,330 bytes | 484,610 bytes |
| `{{a}}` repeated | 952,385 bytes | 956,729 bytes |
| The densest measured expression shape (alternating fence content and directives) | 616,935 bytes | 618,527 bytes |
| Prose with one `{{a}}` in each 80 bytes | 5,229,805 bytes | 5,237,969 bytes |
| Prose | 10,485,760 bytes (the 10 MiB file cap applies first; the budget alone admits 12,336,188) | 12,336,188 bytes |

A reused `ModuleCache` fills faster: each module that it caches keeps its estimate until
the cache is dropped.

The budget also counts the compiler's copy of the runtime variables, once per compile.
The caller's own map is outside the budget. The count stops at 64 levels of value
nesting, so a `Value` built in Rust and nested deeper fails with the compile memory
message. On a cache hit at the root of a reused `ModuleCache`, the compiler makes a copy
and frees it before it returns, and does not count it. A copy that a cached function
captures stays alive in a reused cache after its compile, and only the compile that made
it counts it.

The compile memory budget does not count these stages:

- the lexer stage of each source: its array of characters (`Vec<char>`), its array of
  byte offsets (`byte_offsets`) and the tokens that it makes. This stage ends before the
  compiler charges the syntax tree estimate. It grows with the source text, which the
  10 MiB file cap bounds. On a 64-bit host it holds 14 bytes per source byte for ASCII
  prose, and at most 56 bytes per source byte for the densest syntax;
- the caller's own map of runtime variables (the compiler's copy is counted);
- the line table of a source map;
- the YAML event buffer of one frontmatter parse, whose input the 1 MiB frontmatter cap
  bounds;
- the YAML event buffer of one YAML data file parse, whose input the 10 MiB data file cap
  bounds;
- the conversion of a result into the host types of a binding (napi, WASM, Python);
- the old buffer that a reallocation holds for one instant when it moves a buffer;
- resolver warnings, the message and warning lists, and the call stack, which their
  count caps bound.

A few allocations are counted directly after they are made, each one allocation that
an existing cap bounds. A host thus needs memory for the budget, plus the stages above,
plus the largest of those allocations; on napi, WASM and Python, add the conversion of
the result. `crates/mds-core/tests/memory_bound.rs` measures the peak heap of compiles
that reach the budget: it allows each of them at most 8 MiB above the budget, plus one
1 MiB output pass for a compile that writes output. A module of the densest syntax whose
estimate does not fit grows the heap by at most 56 bytes per source byte. The test's
`assert_syntax_tree_peaks` checks the estimate against the 62 source shapes, and the
lexer stage against the 56-byte bound for the densest syntax, two tokens for each four
source bytes.

At the end of a compile, the compiler checks its memory count. A failed check is a
compiler defect: the compile returns `mds::internal` (`internal compiler error`, no
help, no span) instead of a result, and the module is not cached. `mds build`, `mds check`
and `mds lint` exit 101. `mds fmt` does not: when its safety gate's compile of the
original source fails the check, the gate compares the tokens of the two sources. A live
`mds watch` session keeps its exit code.

## ⚠️ The `debug-panics` feature must never ship enabled

The three binding crates — `mds-napi`, `mds-wasm` and `mds-python` — and `mds-cli`
declare an off-by-default `debug-panics` Cargo feature (`crates/mds-napi/Cargo.toml`,
`crates/mds-wasm/Cargo.toml`, `crates/mds-python/Cargo.toml`,
`crates/mds-cli/Cargo.toml`). `mds-core` has none. In a binding the feature surfaces
the raw Rust panic payload as `err.detail` on `mds::internal` errors thrown at the
binding boundary; in the CLI it prints the panic's message and location after the
internal-compiler-error text below. Both exist to help diagnose unexpected panics
during local development.

**Native host panic boundaries (#436).** The napi and Python bindings use the shared
`mds::host_panic::catching` helper around core calls. It installs one hook per library
and silences the previous hook only inside the current thread's catch boundary.
Outside that boundary, the previous hook receives the original panic information.
The previous hook is Rust's default hook unless the host replaced it first.
Nested calls restore the prior boundary state. Other threads keep their own state.
The error remains `mds::internal` with message `internal compiler error`.
The off-by-default `debug-panics` feature still adds the payload as error detail.

A later host `set_hook` call replaces this hook. The library does not reinstall it
per call, because that would race with the host. With `panic = "abort"`, a panic
cannot return through the catch boundary. A Python panic outside `guard` can still
become a `PanicException` with the payload.

WASM installs no hook: on wasm32-unknown-unknown, the default hook has no output channel.
A panic traps on that target.

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

Eight more test-only hooks use the same compile condition. Seven pause the run at one point until a test releases it.
`MDS_TEST_PAUSE_BEFORE_REPLACE` pauses a rewrite between its read and its replace.
For a new file, it pauses the write between its name lookup and its commit.
`MDS_TEST_PAUSE_AFTER_BATCH_SPLIT` pauses a directory `mds watch` rebuild after its source-presence check, before the compile.
`MDS_TEST_PAUSE_AFTER_LOOK` pauses a rebuild, in either watch mode, after its empty-file check, before any source read.
`MDS_TEST_PAUSE_AFTER_READ` pauses a file-mode rebuild after the compile reads its files, before its baseline snapshot.
`MDS_TEST_PAUSE_BEFORE_LATE_LOOK` pauses a file-mode rebuild after its `--vars` load and compile read their files, and a directory batch after each source's compile, before the late empty-file check.
`MDS_TEST_PAUSE_AFTER_WALK` pauses a directory command after its file walk.
`MDS_TEST_PAUSE_BEFORE_WATCH_COMPILE` pauses a watch compile after its path check, before entry resolution.
Each pause variable names a file. The run writes `<file>.paused` at the pause point.
It resumes once `<file>` exists, or after ten seconds.
`MDS_TEST_READY` has `mds watch` create the file the variable names once the watch is active.
It creates that file by path, outside the write primitive's checks.
A release build contains none of them: each has an empty stub there, so it never reads the
variable and never writes the file, unless its profile turns debug assertions on.
`panic_hook.rs` pins this for each hook, as it does for `MDS_TEST_PANIC`.

**Never enable `debug-panics` in a published or production build.** Panic messages
can contain absolute filesystem paths and other internal details that should not be
exposed to template authors or end users. The feature is off unless opted into
explicitly: none of the four crates lists it in a `default` feature set
(`mds-python`'s default is `extension-module` only), and the commands that build the
published artifacts — `napi build --release` in `release.yml` for the addon, the
`@mdscript/mds-wasm` build script (`wasm-pack build ../../crates/mds-wasm --target
nodejs …` and `--target web …`) for the WASM package, and `maturin` with
`pyproject.toml`'s `features = ["pyo3/abi3-py311"]` for the wheels — pass no
`--features debug-panics`. `crates/mds-cli/tests/panic_hook.rs` checks all four
manifests. It rejects a quoted feature value outside a comment, or a declaration
other than `debug-panics = []`. It also checks the maturin features, napi package
scripts and `release.yml` for feature selection. Each input has a planted control.
The same test file checks both native catch funnels and the WASM hook policy.
`mds-cli` is published to crates.io, so
`cargo install mds-cli --features debug-panics` builds a CLI that prints panic
messages: build one only for your own debugging.
