# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### **BREAKING** — lint JSON wire contract (#202, #203, #211)

#### Lint JSON wire contract (#202, #203, #211)

> This block is the **single wire-change ledger** for the lint JSON envelope.
> Later changes to `mds lint --format json` append here rather than opening a
> parallel section, so a consumer has one place to read.

**Before / after**, for `mds lint - --format json` on a source with one unused
selective import:

```jsonc
// abbreviated — see spec.md for the full schema
// before
{ "files": [ { "diagnostics": [
    { "rule": "duplicate-export", "span": { "length": 7, "offset": 59 } },
    { "rule": "unused-import",    "span": { "length": 7, "offset":  0 } }
  ], "file": "input.mds" } ], "truncated": false, "version": 1 }

// after
{ "files": [ { "diagnostics": [
    { "rule": "unused-import",    "span": { "length": 5, "offset": 10 } },
    { "rule": "duplicate-export", "span": { "length": 7, "offset": 59 } }
  ], "file": "<stdin>" } ], "truncated": false, "version": 1 }
```

**A consumer breaks if it** keys off `files[].file == "input.mds"` for CLI stdin
output, matches `<source>` in a rendered diagnostic frame (stderr only — the JSON
`error.message` field cannot carry source identity; no `MdsError` Display template
interpolates `ctx.file_str`, per AD-211-5), relies on
`diagnostics[]` arriving in rule-execution order, assumes `unused-import`
spans have length 7, relies on the `mds lint <dir>` file-group order being
component-wise (`Path::Ord`), or on Windows assumes `files[].file` values use
the native backslash separator. File groups are now ordered by the byte-wise string
of the relative display path (e.g. `api-utils.mds` sorts before `api/x.mds`
because `'-'` (0x2D) < `'/'` (0x2F)). On Windows, `relative_display` normalises
path separators to forward slashes, so a nested path that previously appeared as
`sub\c.mds` in the JSON now appears as `sub/c.mds`; a consumer that string-matches
or splits on `\` in `files[].file` values will silently fail to match.

**1. Diagnostics are sorted by byte offset (#202).** Within each
`files[].diagnostics` array, diagnostics are ordered by ascending `span.offset`
for results produced by the lint engine; a `LintResult` assembled directly via
`LintResult::new` is emitted in the order the caller supplied.
Previously the order was rule-execution order (implementation-defined).

- Diagnostics without a span sort to the end of their file group.
- Equal-offset diagnostics preserve rule-execution order (stable sort).
- File groups have a defined order: `mds lint <dir>` sorts `files[]` by the
  byte-wise (lexicographic) string comparison of the relative display path — e.g.
  `api-utils.mds` sorts before `api/x.mds` because `'-'` (0x2D) < `'/'` (0x2F).
  This is a CLI directory-mode contract only: the binding surfaces (napi / WASM /
  Python) lint a single entry source, so their `files[]` array never carries more
  than one entry.
- Ordering is established on `LintResult.diagnostics` itself, so the CLI human
  path and the napi / WASM / Python surfaces observe the same order.
- **Truncation is unchanged and is NOT offset-ranked.** When `truncated` is
  `true`, the retained diagnostics are still the first `MAX_DIAGNOSTICS` (1,000)
  in rule-execution order, re-sorted afterwards — not the 1,000 smallest offsets.
- **Sort cost (AC-P1-22):** The sort key is a borrowed tuple `(bool, &str, bool,
  usize)` — zero per-comparison heap allocations. The sort runs at most once per
  `LintResultBuilder::build` call over n <= `MAX_DIAGNOSTICS` (1,000) items.

**2. The stdin source identity is `<stdin>` (CLI only — see below) (#211).** Every CLI context
that names a stdin source now uses the single sentinel `<stdin>`:

- the JSON `files[].file` key (previously `"input.mds"`, the internal VFS key);
- human diagnostic frames for `mds lint -` (previously `input.mds`);
- fix-preview status lines and diff headers (previously bare `stdin`);
- the **analysis-failure envelope** — a stdin source that fails the check gate
  used to render `<source>:L:C`, the resolver's internal label. `mds check -` and
  `mds build -` rendered `<source>` on the same path and now render `<stdin>`
  too, so all four subcommands agree.  Note: the analysis-failure JSON envelope
  shape is `{"version":1,"error":{"code","message","help","span"}}` — it carries
  **no `file` key** (unlike the success envelope which has `files[].file`).  A
  JSON consumer reading `error` results MUST NOT look for a `file` key there.

`mds::STRING_SOURCE_MAP_LABEL` is **unchanged** and remains `"input.mds"`: it is a
virtual-FS entry key, not a display label. The napi, WASM and Python lint APIs
continue to report `"input.mds"` for string-source input. The relabel is applied
only at the CLI output boundary.

**Zero-diagnostic behaviour:** when stdin lint completes with no findings, the
JSON is `{"files":[],"truncated":false,"version":1}` — no file entry. The
`<stdin>` sentinel appears in `files[0].file` only when at least one diagnostic is
emitted. This matches non-stdin zero-diagnostic behaviour and keeps the JSON
identical across the CLI and binding surfaces (napi, WASM, Python) for the clean
case.

**3. `unused-import` spans anchor at the unused name (#203).** For selective
imports (`@import { name1, name2 } from "path"`), the span now covers the unused
name rather than the `@import` keyword, and `span.length` is the name's length
instead of a constant 7. Alias imports (`@import "path" as alias`) are unchanged —
their span still covers the `@import` keyword.

#### New `fix_edits` field on `LintDiagnostic`

`LintDiagnostic` gains an additive `fix_edits` field (null when not fixable;
an array of `{start, end, new_text}` byte-span edit objects when fixable). This
field is present across all binding surfaces: CLI JSON output, napi
(`LintDiagnostic.fix_edits?: …`), WASM, and Python
(`LintDiagnostic.fix_edits: list[dict] | None`).

#### `Fixed: <path>` and `Would fix: <path>` now emitted in JSON directory mode

`mds lint --fix <dir>` (with `--format json`) now emits `Fixed: <path>` to stderr per
fixed file, and `mds lint --fix --check <dir>` (with `--format json`) emits
`Would fix: <path>` per file with pending fixes. Previously both lines appeared only in
`--format human` mode. Both are suppressed under `--quiet`. The JSON document on stdout
is unchanged. **A CI job that asserts zero stderr from these invocations on a non-quiet
run will now receive output when any file is fixed or has fixable issues.**

### **BREAKING** — Error/lint messages now carry `\uXXXX` literals for embedded control bytes (#176)

Across the JS / Python / WASM API surfaces, `err.message`, `err.help`, and lint
`LintDiagnostic.message` / `LintDiagnostic.help` now contain six-character `\uXXXX`
Unicode escape literals (e.g. `\u001B`, `\u007F`, `\u0085`) wherever MDS source
content caused raw C0-minus-`\n`/`\t`, DEL (U+007F), or C1 (U+0080–U+009F) control
bytes to appear in error or diagnostic messages.

**Not affected:** `span.offset`, `span.length`, and `fix_edits` byte ranges are raw
byte offsets and are never sanitized. The `rule` field is a fixed ASCII identifier.
The `"file"` key in lint JSON output is sanitized on the same pass as `message`/`help`.

**Migration:** consumers that test for exact control byte sequences in error or
diagnostic messages must update to check for the `\uXXXX` literal form instead.

### **BREAKING** — Options validation, directory walker, source-map labels, check API (#196)

- **`@mdscript/mds` now rejects unknown option keys** with
  `Error { code: 'mds::invalid_options' }` before forwarding to the backend. Previously
  unrecognized keys were silently passed through (napi and WASM backends would reject
  them, but the universal JS wrapper did not validate). Callers with typos in option
  objects will now get immediate, accurate error messages. (#196)

- **`CheckOptions` is now split from `CompileOptions`** in `@mdscript/mds`.
  `check()` accepts `{ vars?, basePath? }` (via `CheckOptions`); `checkFile()` accepts
  `{ vars? }` only — `CheckFileOptions` declares `basePath?: never`, so passing a non-null
  `basePath` to `checkFile` throws `mds::invalid_options` synchronously. Source-map options
  (`sourceMap`, `sourcesContent`) are not valid for either check call and are rejected with
  `mds::invalid_options`. `CompileOptions` retains `sourceMap`/`sourcesContent`. TS interface
  implementers: `check` narrows to `CheckOptions`; `checkFile` narrows to `CheckFileOptions`
  (amended by #180). Non-`strict`-mode TypeScript consumers who guard on
  `diag.span !== undefined` must update to `diag.span != null` — `help` and `span` on
  `LintDiagnostic` are now typed as `... | null` so the `!== undefined` guard no longer
  catches `null` values. (#196)

- **String-source `sourceMap` label changed from `"<source>"` to `"input.mds"`**
  across all surfaces (CLI, napi, WASM, Python). The `sources[0]` entry in Source Map v3
  output for `compile(src, {sourceMap:true})` / `compile_str*` / WASM `compile` now reads
  `"input.mds"` instead of `"<source>"`. CLI stdin builds use `"<stdin>"` (unchanged).
  Code inspecting `sources[0]` for the string `"<source>"` must be updated. (#196)

- **Directory walker now excludes hidden directories and `node_modules` by default**
  across all subcommands (`mds build`, `mds check`, `mds watch`, `mds fmt`,
  `mds lint`). Directories whose name starts with `.` (e.g. `.git`, `.venv`) and
  `node_modules` are silently skipped during recursive traversal. Templates inside these
  directories are no longer compiled, formatted, or linted in directory mode. (#196)

- **`mds check` summary wording changed** from `N checked` to `N passed, M
  failed`. Scripts parsing CLI output must be updated. (#196)

- **lint `--format json` `"file"` keys are now full relative paths** in directory mode.
  When running `mds lint --format json .`, the `"file"` key in each JSON result is now
  the path relative to the lint root (e.g. `"src/template.mds"`) rather than just the
  basename (e.g. `"template.mds"`). This prevents key collisions when two different
  files have the same filename. (#196)

- **`mds-core::CompileOptions` gained `source_map_base: Option<PathBuf>`**.  Rust code
  that initializes `CompileOptions` with a struct literal must either add
  `source_map_base: None` or use the `..Default::default()` tail.  Binding surfaces
  (napi, Python, WASM) are not affected. (#3)

### **BREAKING** — File-method `basePath` rejection, TypeScript option types, and WASM `basePath` rejection (#180, #213)

#### `compileFile` and `checkFile` now reject `basePath` (#180)

`compileFile(path, options?)` and `checkFile(path, options?)` previously accepted a
`basePath` option and silently discarded it — the option passed the unknown-key
validator but was dropped before the backend was reached, so file resolution always
used the directory containing the path. Both functions now **throw synchronously**
(`Error { code: 'mds::invalid_options' }`) when `basePath` is non-null. The base
directory for file-based operations is always derived from the file path itself.

**Migration:** remove `basePath` from any options object passed to `compileFile` or
`checkFile`. This throw is **synchronous** — `.catch()` on the returned promise does
not receive it; wrap the call in `try/catch`. This is also a **compile-time break**
when a variable typed as `CompileOptions` or `CheckOptions` is passed to these
functions — see the compatibility notes below. Audit all call sites.

#### `FileOptions` no longer extends `CompileOptions` (#213)

`FileOptions` (used by `compileFile`) was previously declared as
`interface FileOptions extends CompileOptions`. This inheritance was an error:
`CompileOptions` now carries `basePath`, which is not valid for file-based
operations. `FileOptions` is now a standalone interface with its own `vars`,
`sourceMap`, and `sourcesContent` fields.

**Compatibility:** this change is a **compile-time break** for code that passes a
`CompileOptions`-typed variable to `compileFile`. After this PR, `CompileOptions`
carries `basePath?: string` while `FileOptions` declares `basePath?: never`;
TypeScript reports `"Types of property 'basePath' are incompatible"` at any such
assignment or call. Code that never reuses a string-surface options variable for
file operations compiles without changes.

**Migration for shared variables:** retype the variable as `FileOptions`, or
destructure only the accepted fields:
```ts
const { vars, sourceMap, sourcesContent } = compileOpts;
compileFile(path, { vars, sourceMap, sourcesContent });
```

#### `checkFile` parameter type changed from `CheckOptions` to `CheckFileOptions` (#213)

`checkFile(path, options?)` previously accepted `CheckOptions`. After this PR,
`CheckOptions` carries a `basePath` field that is not valid for file-based operations;
the parameter is now typed as `CheckFileOptions` — a new interface with only
`vars?: Record<string, unknown>`.

**Compatibility:** this type narrowing is a **compile-time break** for code that
passes a `CheckOptions`-typed variable to `checkFile`. `CheckOptions` carries
`basePath?: string` while `CheckFileOptions` declares `basePath?: never`; TypeScript
reports `"Types of property 'basePath' are incompatible"` at any such call. Code
that never reuses a string-surface variable for `checkFile` compiles without changes.

**Migration for shared variables:** retype the variable as `CheckFileOptions`, or
restrict it to `{ vars?: Record<string, unknown> }` at the call site.

#### `LintFileOptions` gained `basePath?: never` (#213)

`LintFileOptions` (used by `lintFile` and `lintVirtual`) previously had the shape
`{ vars?, rules? }`. It now declares `basePath?: never`.

**Compatibility:** this is a **compile-time break** for code that assigns a variable
whose inferred type includes a `basePath` field to a `LintFileOptions`-typed slot.
For example, passing a `LintOptions`-typed variable directly to `lintFile` or
`lintVirtual` now fails with `TS2322` — `LintOptions.basePath` is `string | undefined`
which is not assignable to `never`. Code that passes a fresh object literal without
`basePath`, or a variable that was already typed as `{ vars?, rules? }`, continues to
compile unchanged.

**Migration:** at each `lintFile` / `lintVirtual` call site that passes a
`LintOptions`-typed variable, either extract a narrowed copy
(`const { basePath: _unused, ...fileOpts } = opts`) or redeclare the variable as
`LintFileOptions` when `basePath` was never meaningful there.

#### WASM backend rejects `basePath` on string-surface methods (#180)

`compile(source, { basePath: '/dir' })` and `check(source, { basePath: '/dir' })`
previously silently ignored `basePath` on the WASM backend. They now throw
`Error { code: 'mds::invalid_options' }`. `lint(source, { basePath: '/dir' })` was
already documented as WASM-unsupported; it now enforces this at runtime too.

**Migration:** switch to the native backend (`MDS_BACKEND=native`) when you need
import resolution with a `basePath` in WASM environments.

### **BREAKING** — Interpolation syntax: `{x}` → `{{x}}` (#236)

Interpolation now uses **double braces**: `{{variable}}`, `{{obj.field}}`,
`{{func("arg")}}`, `{{alias.func()}}`. Single `{` and `}` are always **literal
text** — no escaping needed for lone braces.

#### Interpolation syntax changed (#236)

Templates using the old `{var}` form will no longer interpolate — they will emit
the literal text `{var}` instead. This affects every template that uses variable
substitution, function calls, or dynamic message roles.

**Migration:** run `mds lint --fix` to auto-migrate (the `legacy-interpolation`
lint rule, Tier A, rewrites every `{x}` → `{{x}}` in one pass), then run
`mds fmt` to normalize formatting.

```
# One-command migration for a project:
mds lint --fix && mds fmt
```

#### Escape syntax changed

- Old: `\{` → literal `{`, `\}` → literal `}` — **deleted**; these escapes are
  no longer recognized (single braces are ordinary text and need no escaping).
- New: `\{{` → literal `{{` in output. Use this only when you need `{{` as
  literal text in the output (e.g. inside a Jinja/Python f-string example).

#### `@message {{role}}:` dynamic roles

Dynamic message roles now use double braces: `@message {{role}}:` instead of
`@message {role}:`. Bare-word roles (`@message system:`) are unchanged.

#### `LintDiagnostic` is now `#[non_exhaustive]`; use the constructor, not struct literals

`LintDiagnostic` is marked `#[non_exhaustive]` so future minor releases can add
fields without a breaking change. External Rust crates can no longer construct it
via a struct literal. Use `LintDiagnostic::new(rule, severity, message)` to create
a diagnostic with required fields and all optional fields defaulting to `None`, then
chain the builder methods `with_help`, `with_span`, `with_file`, `with_fix_removals`,
and `with_fix_edits` to set optional fields.

#### Nine additional public types are now `#[non_exhaustive]`; migrate struct literals to constructors

The following types are marked `#[non_exhaustive]` so future minor releases can add
fields without a breaking change. External Rust crates can no longer construct them
via struct literals. Use the named constructor or builder listed for each:

- **`LintResult`** — use `LintResult::new(diagnostics)` (defaults: `truncated=false`, `is_standalone=false`),
  then chain `.truncated()` or `.standalone()` to override.
- **`SerializedError`** — not externally constructable by design; obtain via `MdsError::serialize()`.
- **`SerializedSpan`** — use `SerializedSpan::new(offset, length)`, then chain `.with_line(n)` and/or `.with_column(n)`.
- **`TextEdit`** — use `TextEdit::new(start, end, new_text)`. Previously this type was `pub`
  inside a `pub(crate)` module and was thus unnameable from external crates; this PR re-exports
  it at the crate root, making `mds::TextEdit` accessible for the first time. The `fix_edits`
  field on `LintDiagnostic` (and the corresponding JSON field) was effectively unusable from
  Rust until this change.
- **`FixLineSpan`** — use `FixLineSpan::single(offset)` for single-line removals,
  `FixLineSpan::range_inclusive(from, to)` to remove through the line containing `to`,
  or `FixLineSpan::range_exclusive(from, to)` to keep the line containing `to`.
- **`ByteEdit`** — use `ByteEdit::deletion(start, end, rule)` for pure deletions or
  `ByteEdit::replacement(start, end, rule, text)` for in-place replacements.
- **`RejectedEdit`** — use `RejectedEdit::new(edit, reason)`.
- **`FixPlan`** — use `FixPlan::default()` for an empty plan; its fields are `pub`, so they
  remain directly readable and writable from external crates.
- **`LintConfig`** — use `LintConfig::from_rules_checked(rules)` or `LintConfig::default()` for no overrides.
- **`LintDiagnostic::sanitized_for_render()`** — a new method that returns a sanitized clone
  suitable for miette render boundaries. `mds-cli`'s diagnostic render path now delegates to
  this method instead of assembling sanitized copies itself, keeping the escape logic co-located
  with the struct definition (PF-014).

### **BREAKING** — Strict cross-type comparisons, merged `@extends` frontmatter, interior-verbatim whitespace, filesystem API (#146, #150, #151, #152, #154)

These changes alter observable runtime behavior and compiled output. Templates relying
on the previous (buggy) behavior must be updated.

#### Cross-type comparisons are now errors (#152)

`@if a == b:` or `@if a != b:` where `a` and `b` are different types (e.g. a number
vs. a string, or a boolean vs. null) now raises `mds::type_mismatch` at runtime
instead of silently returning `false` (for `==`) or `true` (for `!=`).

**Migration:** add an explicit conversion before comparing:
- `@if string(count) == "3":` — convert number to string
- `@if count == 3:` — compare number to number literal

#### Cross-flag duplicate keys in `--set` / `--set-string` are now a hard error (#152)

Supplying the same variable key via both `--set KEY=VALUE` and `--set-string KEY=VALUE`
in a single invocation is now rejected at startup with an explicit error.

**Migration:** remove the duplicate key from one flag.

#### `@extends` emits deep-merged frontmatter (#154)

Compiled output for a child template now contains the **deep-merged** frontmatter
(base keys + child keys, child wins on collision, reserved keys `imports`/`type`/`extends`
excluded) rather than only the child's raw frontmatter. Base-only frontmatter keys
now appear in the compiled output.

**Migration:** if your pipeline depends on base frontmatter keys being absent from the
compiled output, strip them downstream or move them to a non-frontmatter location.

#### Interior-verbatim whitespace contract for `@block` bodies and `mds fmt` (#150, #151)

Leading blank lines and interior blank runs inside `@block` bodies and
`mds fmt` output are now preserved verbatim; previously they were collapsed or stripped.
The `mds fmt` blank-line collapsing rule (R3) has been removed to maintain compile
equivalence with the updated evaluator behavior. Only the trailing edge normalizes (to
exactly one final newline). `@message` and `@define` bodies continue to edge-trim —
leading and trailing blank lines are stripped — and are not covered by this contract.

**Migration:** compiled outputs may gain blank lines that were previously collapsed or
stripped; templates relying on this collapse must remove the extra blank lines at the
source level.

#### `FileSystem` trait now requires `normalize_in_dir` and `parent_dir` (#146)

`FileSystem` now requires two new methods — `normalize_in_dir` and `parent_dir` — that
replace the internal `<source>` path-sentinel pattern. String-source `@import`/`@extends`
resolution is now directly directory-anchored: `ctx.base_dir` carries the importing
directory explicitly, with no synthetic filename appended. No behavior change for
`compile`/`check` users; only affects code that implements the `FileSystem` trait
directly via `ModuleCache::with_fs`.

### Added

- **`lint` and `lintVirtual` are now exported from the browser entry point (#215).**
  Previously only `compile` and `check` were available from the WASM/browser surface;
  the underlying WASM module already supported linting but the exports were missing from
  `browser.ts`. Both functions are available after `init()` and follow the same
  unknown-option guard used by `compile`/`check`.

  All eight lint types (`LintDiagnostic`, `LintFileOptions`, `LintFileReport`,
  `LintOptions`, `LintResult`, `LintRuleName`, `LintSpan`, `RuleSeverity`) and the
  `LINT_RULE_NAMES` constant are now exported from the browser entry point as well as
  the Node.js entry point.

- **`SourceMapV3` is now exported from the Node.js and browser entry points.**
  `MarkdownResult.sourceMap` has always been typed as `SourceMapV3`, but the type was
  only re-exported from `index.ts`, which the package `exports` map does not resolve —
  so consumers could receive the value but not name its type. Purely additive.


- **`mds lint <dir>` directory-mode summary (#216).** After linting a directory, one summary
  line is printed to stderr:
  `N clean, N with warnings, N with errors, N resource-limited`
  Under `--quiet`, the summary is suppressed when the worst outcome is warnings only (mirrors
  `mds fmt`); it is always emitted when any file is in the error or resource-limited bucket.
  Scripts or tests that relied on `mds lint <dir>` producing no stderr on a clean tree should
  note that this summary line is now always printed on a clean run (to suppress it, pass
  `--quiet`). The JSON stdout envelope (`{"files":…,"truncated":…,"version":1}`) is unchanged
  — no `"summary"` key is added, so existing consumers of `--format json` are unaffected.
  Exception: `mds lint --fix --check --quiet <dir>` exits 1 with zero stderr bytes when
  pending fixes exist but no file has errors or hits a resource limit — the exit code is
  unexplained on the surface but is intentional and documented in `--help`.

- **`mds lint --fix` adds stderr parity across all three input modes (#216).** Three status
  messages that previously appeared in some modes but not others are now present in all three
  input modes, each suppressed under `--quiet`:
  - **Stdin diagnostic-cap notice**: `mds lint --fix -` on inputs that exceed the diagnostic cap
    now prints `diagnostic cap (N) reached; further findings were suppressed — re-run --fix to
    continue` to stderr. This line did not exist in stdin mode before this release. stdout (the
    fixed source) is unaffected.
  - **`Fixed: <path>` in `--format json` directory mode**: previously emitted only by
    `--format human`; now also printed per fixed file in `--format json`. The JSON document on
    stdout is unchanged.
  - **`Would fix: <path>` in `--format json` directory mode**: same — previously human-only, now
    also printed in JSON mode under `--fix --check`.
  Scripts or tests that assert zero stderr from `mds lint --fix -` or
  `mds lint --fix --format json <dir>` on non-quiet runs should note these additions.

- **Lint rule-name registry, exposed on every surface (#224).** The recognised rule
  names now have one source of truth, derived from each rule module's own name constant.
  - `mds-core`: `KNOWN_LINT_RULES: &[&str]` (the canonical slice),
    `find_unknown_rule_names(&HashMap<String, Severity>) -> Option<UnknownRuleNames>`
    (`None` when every name is recognised), `UnknownRuleNames` (a `#[non_exhaustive]`
    report with a `names() -> &[String]` accessor, always non-empty and sorted), and
    `format_unknown_rule_names_warning(&UnknownRuleNames) -> String`. The formatter takes
    the report type rather than a slice so its non-empty precondition is structural — it
    has no panic path — and it WIRE-escapes each name before interpolating it.
  - `mds-core`: `LintConfig::from_rules_checked(HashMap<String, Severity>) -> (LintConfig,
    Option<UnknownRuleNames>)` — the preferred constructor. It returns the config and the
    unknowns report in one `#[must_use]` call so a caller cannot silently skip detection.
    `LintConfig::from_rules` is retained but **deprecated since 0.4.0** in its favour; it
    still behaves exactly as before (it never fails on an unknown name) and is not removed.
  - `mds-core`: `attach_lint_warnings(&mut serde_json::Map<String, Value>, Option<String>)`
    — the single definition of the `lint_warnings` wire contract (key name, `string[]`
    shape, absent-when-empty) shared by the napi, WASM, and Python bindings. It takes a
    `&mut Map` rather than a `&mut Value` so the "target is a JSON object" precondition is
    structural rather than a silent no-op on a non-object.
  - `@mdscript/mds` (Node entry point) and browser entry point: `LINT_RULE_NAMES: readonly LintRuleName[]`
    and the `LintRuleName` string-union type. The browser entry point exports `lint`, `lintVirtual`,
    and `LINT_RULE_NAMES`; `lintFile` is intentionally absent (file operations require `node:fs`,
    which is unavailable in browser environments).
  - TypeScript `LintResult` gains `lint_warnings?: string[]`.
  - Python `LintResult` gains a `.lint_warnings` property returning `list[str]` (empty
    when there is nothing to report); the type stub is updated to match.

- **`--set-string KEY=VALUE`** CLI flag for `mds build`, `mds check`, and `mds watch`.
  Sets a variable as a string without type coercion — useful when a value is
  numeric-looking but must stay a string (e.g. `mds build t.mds --set-string id=007`).
  Repeatable. (#152)

- **`mds fmt`** — an opinionated, safety-gated auto-formatter for `.mds` templates. Every
  rewrite is guaranteed compile-equivalent: a runtime safety gate re-compiles the formatted
  source and refuses to write if it would change compiled output (`mds::formatter_invariant`)
  rather than silently corrupting a template. Normalizes CRLF to LF everywhere (including
  inside frontmatter and code fences), strips trailing whitespace on directive lines, and
  ensures exactly one trailing newline — while leaving interior blank lines, blank-line
  structure within frontmatter and code fences, body-text trailing whitespace (Markdown
  hard breaks), and the byte-for-byte content of `@message`/`@define` bodies untouched.
  Supports a single file, a directory (recursive, including `_`-prefixed partials), or
  stdin (`-`, as a filter); `--check` exits non-zero without writing when anything would
  change, and `--diff` prints a unified diff (colorized on a TTY) without writing. New
  public `mds-core` API: `format_str` / `format_str_with`. (#60)

- **Native Python bindings** (`crates/mds-python`, PyO3 + maturin), to be distributed
  as `markdown-script` on PyPI (importable as `markdown_script`). Seven functions — `compile`, `compile_file`,
  `compile_virtual`, `check`, `check_file`, `check_virtual`, and `scan_imports` —
  with idiomatic keyword-only signatures. Results are typed, frozen, and picklable
  (`CompileResult` / `Message` / `Span` / `CheckResult`), and failures raise a native
  `MdsError` carrying `.code` / `.message` / `.help` / `.span`. Ships `.pyi` stubs +
  `py.typed` and exposes `__version__`. Output is byte-identical to the Rust,
  Node.js, and WASM bindings (shared core serializer). Built as an `abi3-py311`
  (`cp311-abi3`) extension; each compile releases the GIL and the module is
  free-threading ready (`gil_used = false`), enabling multi-threaded use.
  Cross-platform wheel matrix and PyPI publishing are a tracked follow-up (#132) —
  for now, install from source: `pip install ./crates/mds-python`. (#59)

- **`mds lint`** — 10-rule static analyzer for `.mds` templates (#61). Available
  across all surfaces (CLI, Rust, napi, WASM, Python). The per-file and
  per-diagnostic canonical JSON payload is byte-identical across all surfaces;
  binding surfaces (napi, WASM, Python) additionally expose a `lint_warnings`
  channel absent from the CLI surface (see #224 in this block).

  **Rules** (individually configurable via `mds.json` `lint.rules` or the
  `rules` API option; severities differ per rule):
  - `unused-variable` (warn): frontmatter key defined but never referenced in the body
  - `unused-import` (warn): `@import` never used in the file (Tier B: auto-fixed only for standalone files)
  - `unused-function` (warn): `@define` function never called in the file (Tier B: auto-fixed only for standalone files)
  - `shadow-variable` (off by default / info when enabled): inner-scope variable shadows an outer-scope variable; must be enabled via `mds.json`
  - `empty-block` (warn): `@if`/`@elseif`/`@else`/`@for`/`@define`/`@message` body is empty or whitespace-only (auto-fixable)
  - `redundant-else` (warn): `@else` body is structurally identical to the `@if`/`@elseif` then-body (Tier C — never auto-fixed)
  - `unreachable-branch` (error): branch condition is always-true or always-false (auto-fixable)
  - `duplicate-import` (error): same file imported more than once (auto-fixable)
  - `duplicate-export` (error): same export name defined more than once (auto-fixable)
  - `legacy-interpolation` (warn): single-brace `{x}` interpolation syntax superseded by `{{x}}` — flags all occurrences requiring migration to the double-brace form (auto-fixable)

  **CLI** (`mds lint`): file, directory, and stdin input modes; `--fix` for
  auto-fixable issues (Tier A always; Tier B for standalone files); `--check`
  and `--diff` preview modes for CI; `--format json` for machine-readable output;
  `--quiet` to suppress warnings; `--vars`/`--set`/`--set-string` for variable
  overrides forwarded to the check gate.

  **Exit codes** (lint-specific): `0` = clean, `1` = warnings only, `2` = errors
  or analysis failure, `3` = resource limit.

  **Canonical JSON shape** (keys alphabetical, BTreeMap order):
  ```json
  {"files":[{"diagnostics":[...],"file":"template.mds"}],"truncated":false,"version":1}
  ```

  **Library API**: new public functions in `mds-core` — `lint`, `lint_str`,
  `lint_str_with`, `lint_virtual`; `LintResult`, `LintDiagnostic`,
  `LintConfig`, `Severity` types.

  **napi** (`@mdscript/mds-napi`): `lint`, `lintFile`, `lintVirtual` exports.

  **WASM** (`@mdscript/mds-wasm`): `lint`, `lintVirtual` exports.

  **Universal TypeScript** (`@mdscript/mds`): `lint()`, `lintFile()`,
  `lintVirtual()` with full TypeScript types (`LintResult`, `LintDiagnostic`,
  `LintSpan`, `LintFileResult`, `LintOptions`, `LintFileOptions`). Both native
  and WASM backends implement the full surface; `lintFile()` on the WASM backend
  uses `buildModulesMap` for `@import` resolution.

  **Python** (`markdown_script`): `lint()`, `lint_file()`, `lint_virtual()` with keyword-only
  `rules` and `base_path` / `vars` options; `LintResult` with `.version`, `.truncated`,
  `.files`, `.to_dict()`, `.to_json()`. Stubs shipped in `_markdown_script.pyi` / `__init__.pyi`.

  **Backend interfaces** (`MdsBaseBackend`, `MdsNodeBackend`): `MdsBaseBackend` gained
  `lint` and `lintVirtual` as required members; `MdsNodeBackend` gained `lintFile`. These
  interfaces are internal to the package — defined in `packages/mds/src/index.ts`, which is
  not listed in the `@mdscript/mds` export map and is removed in this release (see
  `### Removed`). No migration is required for consumers of the published package.

- **Source Map v3** (#62). Compile calls can now produce a [Source Map v3](https://sourcemaps.info/spec.html)
  document alongside the rendered output.

  **CLI** (`mds build`): `--source-map` writes a `<output>.map` sidecar and leaves
  the compiled output byte-identical to a no-flag build (ADR-002). `--inline` embeds
  the map as a `<!--# sourceMappingURL=data:... -->` HTML comment at the end of the
  output; no sidecar is written (requires `--source-map`). `--no-source-map` suppresses
  generation when `build.source_map=true` is set in `mds.json`. `--embed-sources`
  (requires `--source-map`) embeds the original source text as `sourcesContent`.

  **Rust** (`mds-core`): new `compile_str_with_deps_opts`, `compile_with_deps_opts`,
  `compile_virtual_with_deps_opts` functions accept `CompileOptions { source_map: bool,
  include_sources_content: bool }`. `CompileResult` carries `source_map: Option<SourceMap>`.

  **Node.js** (`@mdscript/mds-napi`, `@mdscript/mds`): pass `{ sourceMap: true }` (and
  optionally `{ sourcesContent: true }`) to `compile()` or `compileFile()`. The returned
  result gains a `sourceMap` key when source maps are enabled.

  **WASM** (`@mdscript/mds-wasm`): same `sourceMap`/`sourcesContent` options on `compile()`.

  **Python** (`markdown_script`): `compile()`, `compile_file()`, and `compile_virtual()` accept
  `source_map=True` and `sources_content=True` keyword arguments. Results expose a
  `.source_map` property (`dict | None`).

  **Cross-field invariant**: passing `sourcesContent: true` (or `sources_content=True`)
  without `sourceMap: true` (or `source_map=True`) is rejected on all surfaces — the
  CLI rejects the combination at argument-parse time (clap `requires`; exit code 2);
  the library bindings (napi/WASM/Python) raise `mds::invalid_options`. Messages-mode
  templates silently degrade: `sourceMap` is absent from the result and a warning is
  emitted.

  **⚠ Privacy warning**: `--embed-sources` / `sourcesContent: true` / `sources_content=True`
  includes the full original template source in the map file — including any hardcoded
  secrets or PII. Only use in trusted build environments.

- **Partial fix application** for `mds lint --fix`: when a batch of fixes partially
  applies (some edits are accepted, some are rejected due to post-fix regression),
  the CLI now reports `"N of M fixes applied"` and writes the best accumulated state
  to the file. Previously, a partial batch was all-or-nothing (either all or nothing
  applied). (#196)

- **`type_mismatch` errors now carry a source span** (file + line + column) pointing
  to the `@if` or `@elseif` directive that triggered the comparison. The span is
  propagated through all surfaces (CLI miette code frame, napi `.span`, Python
  `.span`, WASM error object). (#196)

- **Spans on `mds::name_collision` errors** in `@export *` (wildcard), alias-import,
  and merge-import paths. The error now points to the collision site instead of the
  file root. (#196)

- **Spans on unclosed-block errors**: `@if`/`@for`/`@define`/`@message` blocks that
  are never closed now produce `mds::syntax` errors anchored at the opening directive.
  (#196)

- **`\{` escape hint on unclosed interpolation brace**: when the compiler encounters
  an unclosed `{` (brace without a matching `}`), the error now includes the hint
  "to include a literal `{`, escape it as `\{`". (#196)

- **`ArityMismatch` help text**: function-call arity errors now include a help string
  pointing users to check the call site and the `@define` signature. (#196)

- **Per-branch `@elseif` offset** in the AST (`ElseifBranch.offset`): lint diagnostics
  for `empty-block`, `unreachable-branch`, and `duplicate-@elseif` now anchor at the
  `@elseif` directive span rather than the parent `@if` opener. (#196)

- **`format_str_named(source, base_dir, file_name)`** — new public `mds-core` API that
  threads a caller-supplied file name through the formatter so that any `mds::syntax`
  errors emitted during formatting name the file rather than using a generic sentinel.
  `mds-cli`'s `mds fmt` uses this to show the actual file path in error output. (#196)

- **`mds fmt --check` summary now includes unchanged count**: the directory-mode summary
  under `--check` is now `"N would reformat, M unchanged, K failed"` (previously `"N
  would reformat, K failed"`). (#196)

- **napi workspace `build` script**: `crates/mds-napi/package.json` gains a `build`
  script (`napi build --release --no-js`) for local development. (#196)

- **Block-span lint fixes — `--fix` now removes whole blocks** (`@if`/`@for`/`@define`
  spans) for the `empty-block`, `unreachable-branch`, and `unused-function` rules.
  Previously the fixer could only remove a single directive line, leaving the
  matching `@end` orphaned; the reverify gate would catch the resulting parse error and
  decline the fix, making these rules report-only in practice (tracked as limitation
  in #172). The implementation now threads `end_offset` through the AST
  (`IfBlock`, `ForBlock`, `DefineBlock`) and uses a `FixLineSpan` descriptor
  (byte range of the full block) to perform whole-block removal. Containment dedup
  in the planner coalesces overlapping spans and deduplicates identical fix ranges
  across rules. JSON `"fixable"` is now `true` only when an actual `FixLineSpan`
  is deliverable and the tier gate passes. The reverify gate still applies
  fail-closed — if recompile fails, the fix is reported not applied.

- **Per-file `mds.json` discovery in `mds lint` directory mode**: when linting a
  directory, the nearest `mds.json` is now located by walking up from **each input
  file** independently (with a cached walk-up per directory). Previously a single
  config at the lint-root directory was applied to all files. A malformed config in
  a subdirectory now produces a per-file error entry and contributes to exit code 2
  rather than aborting the entire run.

- **`mds::invalid_vars`** — new error code for malformed or non-object `--vars`
  JSON (exits 1). A missing `--vars` file continues to use `mds::file_not_found`
  (exits 2). The two failure modes were previously reported as the same generic error.

- **Python typed lint result classes** (`crates/mds-python`): `LintDiagnostic` and
  `LintFileReport` are now typed, frozen `#[pyclass]` instances. `LintResult.files`
  returns a list of `LintFileReport` objects rather than raw dicts. Stubs
  (`.pyi` files) and the `mypy`/`pyright` typecheck sample are updated accordingly.

- **Python `CompileResult.to_dict()` always includes `"sourceMap"` key**: the key is
  present with value `None` when no source map was generated, and with the map dict
  when one was. `to_json()` stays canonical (omits the key when absent) — the
  asymmetry is intentional and documented.

- **WASM CI size guard raised from 800 K to 850 K**: the branch's core growth
  (span attribution machinery, `end_offset` fields, `FixLineSpan` planner) pushed
  the optimized WASM binary to approximately 808 KB locally. The guard in `ci.yml` was raised
  accordingly. CI-measured size after all wave PRs: 836,126 bytes (Binaryen v129; the
  local wasm-opt v117 bundled by wasm-pack reads roughly 2-3 KB higher).

- **Code of Conduct** (#38): `CODE_OF_CONDUCT.md` at the repository root, using
  Contributor Covenant 2.1 with `deanshrn@gmail.com` as the enforcement contact.
  Linked from `CONTRIBUTING.md` and `README.md`.

- **Source-hygiene CI gate** (#288): `scripts/verify-no-control-bytes.mjs` scans
  every tracked file for hazardous codepoints — C0 control characters (excluding
  TAB and LF), DEL, C1 (at codepoint level, catching UTF-8-encoded NEL U+0085),
  the twelve `Bidi_Control=Yes` characters (Trojan Source / CVE-2021-42574), the
  JavaScript line/paragraph separators U+2028 and U+2029, and U+FEFF (BOM).
  Runs in CI on every pull_request and on tag pushes (release.yml). An opt-in
  pre-commit hook (`scripts/hooks/pre-commit`) is provided; it reads the staged
  blob via `git cat-file`, not the working tree. Also remediates seven live
  U+0085 bytes that had been injected into tracked source by the edit tooling
  (PF-018).

- **Pre-merge check verifier** (#289): `scripts/verify-pr-checks.mjs` guards
  against PF-017 (a cancelled CI run reads as "not failing" to `gh pr merge
  --admin`). It evaluates four tiers: Tier A asserts every required
  branch-protection context is `completed+success`; Tier A+ (`EXPECTED_CONTEXTS`)
  asserts locally-named jobs are present and passing regardless of branch
  protection — absence is FAIL, not advisory (ADR-009, PF-013); Tier B fails on
  any non-required check-run that concluded
  `failure/cancelled/timed_out/action_required/stale`; Tier C (legacy commit
  statuses) is advisory. It emits a `gh pr merge --squash --admin
  --match-head-commit <sha>` command pinned to the verified SHA. Exit 0: Tier A,
  Tier A+, and Tier B pass; exit 1: any Tier A, Tier A+, or Tier B failure, or
  zero check-runs found; exit 2: tool/permission errors.

- **`MdsError::source_name() -> Option<&str>`** — a new method that returns the name embedded
  in the error's `NamedSource`, or `None` for errors without a source (e.g. `MdsError::Io`).
  `source_name()` is domain-neutral; callers that need to detect the string-source analysis
  path should use `MdsError::is_string_source()` rather than comparing the returned name
  against the sentinel value themselves — the internal sentinel is `pub(crate)` and is not
  reachable from downstream crates.

- **`MdsError::is_string_source() -> bool`** — a new predicate that returns `true` when the
  error was produced by the string-source analysis path (`resolve_source_intrinsic`). Use this
  instead of comparing `source_name()` against a bare string literal: the internal sentinel
  (`SOURCE_LABEL`) is `pub(crate)` and is not accessible from downstream crates.


### Changed

- **TypeScript: `LintDiagnostic.help` and `LintDiagnostic.span` widened to include `null`**
  (`help?: string | null`, `span?: LintSpan | null`). The JSON wire format has always emitted
  these as `null` (not absent keys) when no hint or span is available — only the TypeScript
  declaration was narrower than the runtime value. This is a semver event for consumers who
  pattern-matched on `diag.span !== undefined` to detect the no-span case; after this change,
  both `undefined` and `null` indicate "no span" and the guard should use `diag.span != null`.
  Matches the already-correct `fix_edits?: ... | null` pattern on the same interface.

- **`mds build --quiet <dir>` no longer prints its summary line on a fully-successful run (#216).**
  Previously `mds build --quiet <dir>` printed `N built, 0 failed` even when every file
  succeeded. CI jobs that grep for `N built` in their logs should note that this line is now
  suppressed under `--quiet` on a clean run. When any file fails the summary is still always
  printed, so a non-zero exit under `--quiet` is never unexplained. Exit codes are unaffected.

- **`mds lint --quiet` now suppresses the remaining `--fix` status messages (#216).** The
  `fix rejected: <reason>` notice (emitted when the three-tier safety gate refuses a fix and
  leaves the file unchanged) and the `diagnostic cap (N) reached` notice are now suppressed
  under `--quiet` in **all three input modes** — directory, single file, and stdin. Previously
  the apply-path `fix rejected:` notice was ungated in all three modes, so `mds lint --fix
  --quiet` printed it to stderr under directory, single-file, and stdin input alike. The
  preview-path copy (under `--fix --check` or without `--fix`) was already `--quiet`-gated in
  most modes but remained ungated in `--format json` directory mode. The `diagnostic cap (N)
  reached` notice was ungated in single-file and both directory modes; in stdin mode it is newly
  added by this release (stdin previously had no cap notice at all — see Added below).
  Additionally, `Fixed: <path>` and `Would fix: <path>` confirmation lines are newly added to
  `--format json` directory mode — previously these lines appeared only in `--format human`;
  all three new emitters are `--quiet`-gated from the start. Scripts that grep stderr for
  `fix rejected` must drop `--quiet`. Exit codes are unaffected, and error-severity diagnostics
  still print under `--quiet` as always.


- **Unknown lint rule names now emit a warning instead of being silently ignored
  (#224).** Previously an unrecognised rule name in `mds.json`'s `lint.rules` object
  (or in the `rules` option on a binding surface) was silently accepted: the rule had
  no effect and nothing signalled that the key was misconfigured. Now the unknown name
  is reported and linting continues — **exit codes are unchanged**, the JSON envelope on
  stdout is unchanged, and the rule is still not enforced (it does not exist). This
  surfaces typos without hard-failing a config that names a rule added in a newer
  release.
  - **CLI**: the warning goes to **stderr**, never stdout, so `mds lint --format json`
    still writes a single valid JSON document. `--quiet` suppresses it. Singular and
    plural formats (offenders sorted lexicographically):
    - `warning: in mds.json: unknown lint rule 'NAME'; recognised rules are: …; ignoring`
    - `warning: in mds.json: unknown lint rules: 'A', 'B'; recognised rules are: …; ignoring`
  - **napi / WASM / Python**: the warning is surfaced as `lint_warnings: string[]` on
    the lint result. In the JSON wire form and in `to_dict()` / `to_json()` output, the
    key is absent (not `null`, not `[]`) when no warnings occurred. On the Python
    live-object surface, `LintResult.lint_warnings` is a property that always exists
    and returns an empty list when no warnings occurred. The message body is shared with
    the CLI via `mds::format_unknown_rule_names_warning` (AC-224-3 met under amended
    criterion, repo-owner ruling 2026-08-16: shared body, shared recognised-rules list,
    shared sort order; the CLI adds a `"warning: in mds.json: "` provenance prefix that
    the bindings cannot provide because their rules arrive in the caller's options
    object, not a config file). The bindings use the body as-is:
    - Singular: `unknown lint rule 'NAME'; recognised rules are: …; ignoring`
    - Plural:   `unknown lint rules: 'A', 'B'; recognised rules are: …; ignoring`
    The recognised-rules list, sort order, and name wire-escaping are all shared.
    Per-surface parity (PF-007): each surface's format is asserted by its own tests.
  - Only `mds lint` reads `lint.rules`, so only `mds lint` warns. `mds build`,
    `mds fmt <DIR>`, and `watch` read `mds.json` via `load_config` but deserialize
    the `lint` field without calling `load_lint_config` — an accepted D2(a)
    asymmetry, not an oversight (see build.rs:49-51). `mds check` and `mds fmt
    <FILE>` do not call `load_config` at all. The D2(a) invariant is held in CI by
    the `build_unknown_lint_rule_in_mds_json_emits_no_warning` and
    `fmt_unknown_lint_rule_in_mds_json_emits_no_warning` tests in `cli_build.rs`,
    each with a positive-control arm (unknown severity causes non-zero exit, proving
    `load_config` was reached). Those tests mechanically hold the AC-224-14
    watch-path invariant: `watch.rs:822` calls `load_config(...).unwrap_or(None)`;
    because `build` and `mds fmt <DIR>` share the same `load_config` implementation,
    a passing build or dir-fmt proves `load_config` returns `Ok` for configs with
    unknown rule names, so `unwrap_or(None)` cannot collapse `output_dir` to `None`
    on account of an unknown lint rule name alone.
  - Unknown **severity values** continue to hard-fail with `mds::invalid_options`. The
    asymmetry is deliberate: severities are a closed set, rule names grow every release.

- **napi and Python `compileFile` / `compile_file` now emit root-relative
  `sources[]`** in Source Map v3 output. Previously these surfaces emitted the
  absolute filesystem path as `sources[0]` (e.g. `/home/user/project/src/foo.mds`);
  now they emit a slash-separated path relative to the project root found via
  `.mdsroot` / `.git` walk-up (e.g. `src/foo.mds`). The `@mdscript/mds`
  universal package's `compileFile` previously returned different `sources[]`
  depending on which backend `init()` loaded (absolute on native, root-relative via
  `buildModulesMap` on WASM); both backends now produce identical root-relative paths.
  Code that compares `sources[0]` to an absolute path must be updated. (#3)

- **Inline stdout source-map absolute-path leak fixed**: `mds build --source-map
  --inline -o -` and `mds build --source-map -o -` no longer leak absolute filesystem
  paths in the embedded `sourceMappingURL` data-URI; sources are relativized against
  the current working directory. Previously the output path was `None` for stdout
  builds, causing the relativization step to short-circuit and leave absolute paths.
  (#196)

- **`mds build --inline -o -` for stdin input is now allowed**: previously rejected
  with an error. Inline and sidecar source maps now work identically for stdin and
  file inputs. The `sources[0]` label is `"<stdin>"` for stdin builds. (#196)

- **lint `--fix --check` and `--fix --diff` are now honest gated previews**: the
  preview pass runs through the same reverify gate as apply. Fixes that would be
  rejected (overlap, post-fix regression) are reported as `"fix rejected: <reason>"`
  rather than silently shown as `"would fix"`. Directory mode `--fix --check` exits 1
  when any file has fixable issues. (#196)

- **Overlap-rejected fix plans are now surfaced**: when `lint --fix` finds overlapping
  byte ranges (two rules targeting the same span), the plan is no longer silently
  abandoned. The overlap is reported so users know a fix exists but could not be auto-
  applied. (#196)

- **`mds fmt` errors name the file**: formatting errors emitted to stderr now include
  the file path as a prefix (e.g. `"src/foo.mds: formatter_invariant: …"`). Previously
  file context was absent, making batch `mds fmt .` errors hard to trace. (#196)

- **`--vars` JSON errors name the file**: when a `--vars` JSON file is malformed or
  does not contain a top-level object, the error message now includes the file path.
  (#196)

- **stdin `mds lint` code frames**: lint diagnostics for stdin input now include a
  miette code frame with `<stdin>` as the source label. Previously stdin lint
  diagnostics lacked source context. (#196)

- **Bare relative filenames now work** for all subcommands and the `compile_str`
  binding family. Running `mds build foo.mds` (without a `./` prefix) from the file's
  directory previously failed on some platforms because the parent-path resolution
  produced an empty path instead of `.`. Fixed by `effective_parent` in `fs.rs`. (#196)

- **`mds fmt` formatter-invariant gate false positive on trailing blank lines is
  fixed**: templates containing trailing blank lines (e.g. `@if … @end\n\n`) were
  incorrectly rejected by the safety gate with `mds::formatter_invariant` after being
  formatted. The gate now correctly ignores insignificant trailing whitespace. (#196)

- **Lint diagnostic messages now consistently end with a period** (G3 message-copy
  consistency): all `empty-block` and `unreachable-branch` rule messages are
  punctuated uniformly. (#196)

- **Messages-mode source-map warning reworded and deduplicated**: the warning emitted
  when `sourceMap: true` is requested on a messages-mode template now reads "source
  maps are not supported for messages-mode templates (@message blocks); no source map
  will be generated" across all surfaces. The warning is emitted exactly once per
  compilation (previously it could appear twice for some template shapes). (#196)

- **`mds::syntax` error label no longer duplicates the message**: the miette diagnostic
  label was previously set to `{message}` (same as the headline), producing redundant
  output in code-frame renderings. It now reads `"syntax error occurred here"`. (#196)

- **Imported-macro `type_mismatch` errors now point to the defining file**: when a
  type mismatch is raised inside a `@define` body that was imported from another file,
  the error frame now names the helper file and shows the relevant line (e.g.
  `helper.mds:3:5`) rather than pointing at the call site in the importing file.
  Implemented via `FunctionDef.origin` (always-populated, one `Arc::from(source)` per
  module) and `EvalContext.body_origin` (LIFO swap around body evaluation). The
  performance trade-off (one Arc per module instead of zero) is an explicit
  AC-PERF-01 relaxation accepted for span correctness.

- **Parser syntax errors now carry directive-line spans**: approximately 20 previously
  spanless `mds::syntax` errors now include a source span pointing at the directive
  line that caused the error (implemented via `MdsError::or_span`). This affects
  common mistakes such as unclosed strings in frontmatter and malformed directive
  arguments.

- **`ArityMismatch` help now shows the expected signature**: the help text for a
  wrong-argument-count error now includes the function's expected call form (e.g.
  `pair(a, b)` or `f(x, y="admin")`), rendered from the `@define` parameter list.
  Previously it only said to check the call site.

- **`TypeMismatch` help text rewritten**: the help message now distinguishes three
  scenarios — comparing a variable to a literal of a different type (suggests an
  explicit conversion), using a non-boolean value in an `@if` truthiness check (notes
  that any non-null, non-false value is truthy), and confusion from `--set` type
  coercion (suggests `--set-string` to keep a string value byte-for-byte).

- **`--vars` missing file exits 2 with `mds::file_not_found`**: previously a missing
  `--vars` path produced a generic I/O error. It now exits 2 (I/O error) with a
  structured `mds::file_not_found` diagnostic including a help note.

- **Config-sourced `source_map=true` degrades gracefully on stdout output**: when
  `build.source_map=true` is set in `mds.json` and the output is stdout (`-o -`),
  the build now proceeds with exit 0 and a single warning naming the config file
  (suggesting `--no-source-map` or `-o <file>`). Previously this combination produced
  a confusing double-error. An explicit `--source-map -o -` flag combination still
  hard-errors with an extended message (`-o <file>` / `--out-dir` / `--inline` /
  `--no-source-map`).

- **Config `embed_sources=true` without `source_map=true` now warns** at all merge
  sites (mds.json merge, `--vars` merge, CLI flag merge). Previously the warning was
  emitted inconsistently.

- **`mds fmt` and `mds lint` check path existence before checking the `.mds`
  extension**: a path that does not exist now exits 2 with `mds::file_not_found`
  (rather than the "not an MDS file" extension error). `mds lint` preserves the JSON
  envelope for a missing-file error in `--format json` mode.

- **WASM `check()` rejects `sourceMap`, `sourcesContent`, and unknown option keys**
  with `mds::invalid_options`. Previously these keys were silently ignored by the
  WASM backend's `check` function.

- **Python `check()` rejects `source_map` and `sources_content` options** with
  `mds::invalid_options`. These options are valid for `compile()` but not for
  `check()`.

- **Fix-rejection message is now actionable**: when the reverify gate declines a fix
  (because the edited source fails to reparse or produces different output), the
  message now reads "could not verify fix — the edited source did not re-parse
  cleanly (reason); leaving the file unchanged" rather than a generic internal note.

- **`unused-import` documented as report-only in practice**: the JSON `"fixable"` key
  for `unused-import` findings is always `false`. A file that triggers this rule has
  at least one `@import` directive, which makes it non-standalone; Tier B fixes
  require a standalone file, so the fix is never delivered. The rule is worth keeping
  for awareness — it clears as a side effect of applying other fixes (e.g. removing a
  duplicate import that was also the unused one).

- **WASM binary size reduced via wasm-opt flag tuning and rustc toolchain pin**: the wasm job
  now pins rustc to 1.96.0 so the size guard measures code changes rather than compiler drift;
  wasm-opt flags were tuned (`-Oz --flatten --rereloop -Oz --converge --strip-producers`) and
  four tie-free sort sites switched to `sort_unstable`. CI-measured size: 836,126 bytes
  (Binaryen v129) against the 850,000-byte guard.

- **Python distribution renamed: `mdscript` → `markdown-script`; import as `markdown_script` (#292, ADR-012).**
  The PyPI name `mdscript` is held by a genuine dormant 2021 project with a direct
  topical collision (`top_level.txt` is exactly `mdscript`), and there is no PEP 541
  route for reclaiming it. Both the distribution name and the importable module name
  must change together because they were identical before (`mdscript`/`mdscript`). The
  private extension module is renamed from `_mdscript` to `_markdown_script` for
  consistency with the new public package name.

  This rename is pre-publication: `publish = false` in `crates/mds-python/Cargo.toml`
  and there is no PyPI publish step in `release.yml`, so there are zero existing PyPI
  consumers. No deprecation shim or compat alias is provided.

  Migration: `pip install markdown-script` (hyphen), then `import markdown_script` (underscore).

### Deprecated

- **`mds::fix::apply_fixes` is deprecated in favor of `apply_fixes_incremental` (#209).**
  The replacement applies the same ADR-004 three-tier reverify-gate safety contract with a
  batch-first attempt plus a bounded per-edit fallback, salvaging the safe subset of fixes
  when some edits fail the reverify gate rather than refusing the whole batch.

  Two behavioral differences require manual migration:

  - **Closure bound**: `apply_fixes` takes `F: FnOnce`; `apply_fixes_incremental` requires
    `F: Fn` because the reverify closure may be called more than once. A move-once closure
    cannot be migrated mechanically.
  - **New reachable outcome**: `apply_fixes_incremental` can return
    `FixOutcome::PartiallyFixed` when some edits are accepted and some are refused.
    `apply_fixes` never returns `PartiallyFixed`. Because `FixOutcome` is
    `#[non_exhaustive]`, existing wildcard arms compile unchanged, but a wildcard that
    swallows `PartiallyFixed` silently discards partial results.

  Scheduled for removal in v0.5.0; tracked in GitHub issue #304. The six ADR-004
  regression tests pinned only against `apply_fixes` must be ported or retired before
  the v0.5.0 tag (see #304 for the enumerated list with line numbers and the behavior
  each test pins).

- **`mds::LintConfig::from_rules` is deprecated in favor of `LintConfig::from_rules_checked` (#224).**
  The replacement returns both the config and an unknowns report in a single `#[must_use]`
  call, making it structurally impossible to silently skip unknown-rule detection.
  `from_rules` still accepts any rule name without error; unknown names have no effect.

  Migration: change `LintConfig::from_rules(map)` to `LintConfig::from_rules_checked(map)`
  and handle the `Option<UnknownRuleNames>` second return value. No removal is scheduled
  before v1.0.0; this function will remain available throughout the v0.x series (contrast
  `apply_fixes` above, which is scheduled for removal at v0.5.0).

### Removed

- **`packages/mds/src/index.ts` removed.** This file was not listed in the package
  `exports` map (which resolves only `./dist/node.js` and `./dist/browser.js`), so no
  supported import path from `@mdscript/mds` is affected. The internal backend interfaces
  it defined — `MdsBackend`, `MdsBaseBackend`, `MdsNodeBackend`, `WasmModule`,
  `initWasmNode`, `initWasmBrowser`, and `createWasmBackend` — were never reachable from
  `@mdscript/mds` via a published import path.

### Fixed

- **`basePath` option is now honored on `compile()`, `check()`, and `lint()` (#180).**
  Previously `basePath` was accepted by the unknown-option validator (so no error was
  thrown) but was silently discarded before reaching the backend: the forwarding builders
  (`compileOpt`/`varsOpt`) never included it in what they passed through. Templates
  containing `@import` or `@extends` directives compiled with a string-source call and a
  `basePath` option would either fail to resolve their imports (native backend) or fail
  silently (WASM backend). The fix adds `basePath` to both `CompileOptions` and
  `CheckOptions` and propagates it to the backend for the string-source methods
  (`compile`, `check`). `compileFile` and `checkFile` deliberately exclude
  `basePath` — the base directory for file operations is derived from the file
  path itself (see the [BREAKING section above](#breaking--file-method-basepath-rejection-typescript-option-types-and-wasm-basepath-rejection-180-213)).

  The WASM backend has no filesystem access and cannot resolve file-relative imports; it
  now **rejects** a non-null `basePath` immediately with `mds::invalid_options` instead
  of silently ignoring it, so misconfigured callers receive an actionable error rather
  than a silent wrong answer. `{basePath: undefined}` is treated as absent on both
  backends (`!= null` check; value-is-intent). To use `basePath` with import resolution,
  set `MDS_BACKEND=native`.


- **`mds build -o build/out.md` with sources in `src/` again emits map-relative
  paths** (e.g. `../src/foo.mds`) in the sidecar `.map` file and inline source map.
  The `source_map_base` field added to `CompileOptions` tells `relativize_source`
  to emit paths relative to the map file's parent directory (as the Source Map v3
  spec requires) rather than root-relative. Without this, a source `src/foo.mds`
  compiled to `build/out.md` with `--source-map` would emit `src/foo.mds` in the
  map instead of the spec-correct `../src/foo.mds`. Root-relative emission
  (`source_map_base: None`) is now the default for all binding surfaces (napi,
  Python, WASM), which never write map files to disk. Map-relative paths are emitted
  only when the sidecar map file is written inside the project root (located via
  `.mdsroot` / `.git` walk-up); when the map lands outside the root, paths remain
  root-relative, and a `..`-escaping reference (a source outside the project root)
  falls back to the basename. (#3)

- **Code fences: tilde (`~~~`), indented, and blockquoted variants are now recognized**
  as passthrough regions. Previously only `` ``` ``-fences that started at column 1
  were treated as code — a `~~~` fence, a `` > ``` `` blockquote fence, or a fence
  indented with spaces/tabs would allow interpolation and directive parsing inside,
  silently corrupting output for affected templates. The lexer now matches any fence
  that starts with `[ \t>]*` followed by three or more matching backticks or tildes.
  (#149)

- **Interpolation errors now suggest `\{`** in the help text when a closed interpolation
  contains an invalid expression (e.g. `{foo bar}` or `{1+2}`). Helps users who intended
  a literal `{` but received a parse error on the expression inside. (#153)

- **Windows: string-source `@import`/`@extends` now resolve relative imports
  correctly.** `std::fs::canonicalize` returns a `\\?\` verbatim extended-length path
  on Windows, and inside a `\\?\` prefix `/` is a literal character, not a path
  separator — so building the in-memory-source base key with
  `format!("{canonical}/<source>")` produced a key that `Path::parent()` could not
  strip back to the base directory, silently resolving relative imports against the
  wrong directory. Fixed by eliminating the synthetic `<source>` key entirely: the
  importing directory is now carried directly as `ctx.base_dir` and passed to
  `FileSystem::normalize_in_dir`, so no synthetic path component is ever constructed
  or decomposed. Fixes napi `compile`/`check(src, { basePath })`, Python
  `compile`/`check(src, base_path=...)`, and CLI `mds build -` / `mds check -`
  (stdin) — all share the same resolution path. POSIX behavior is unchanged. (#133,
  #146)

- **Messages-mode stdout no longer produces a double-error**: previously, when a
  messages-mode template was built with a stdout output path (`-o -`) and
  `source_map=true` (from config), two overlapping errors could fire. The
  messages-mode stdout path now emits exactly one warning.

- **`mds lint --fix --format json <file>` no longer emits `"file": "input.mds"`
  for residual diagnostics.** In single-file mode with both `--fix` and
  `--format json`, the `files[].file` key in the JSON output for residual
  (post-fix) diagnostics was the internal VFS label `"input.mds"` instead of the
  real file basename.  The reverify closure inside `plan_and_apply_fixes` calls
  `lint_str_with`, which sets `diag.file` to `STRING_SOURCE_MAP_LABEL`; the
  resulting residual was not relabeled before `emit_result`.  Fixed by calling
  `set_diag_display_path(&mut residual, filename)` in the `Fixed` and
  `PartiallyFixed` match arms of `run_lint_file`, mirroring the existing relabel
  in directory mode (which was already correct).

- **Python `LintDiagnostic.to_dict()` now conditionally includes `"line"` and `"column"` in the `span` object; `LintResult.files[]` now parses these fields from backing JSON.** Previously `to_dict()` dropped `"line"` and `"column"` unconditionally, emitting only `{"offset": …, "length": …}` in the `span` sub-dict. A caller round-tripping a user-constructed `LintDiagnostic` through `to_dict()` previously received `{"offset": N, "length": M}` and now receives `{"column": …, "length": M, "line": …, "offset": N}` — breaking golden-file assertions and strict JSON-schema validators using `additionalProperties: false`. No built-in lint rule sets `"line"` or `"column"`, so live lint output from the engine is unchanged. `LintResult.files[]` previously hardcoded `line=None` and `column=None` when parsing backing JSON; it now reads the actual values.

### Security

- **Source Map v3 `sources[]` no longer leaks absolute filesystem paths** across
  all surfaces. Previously, `compileFile` on napi and Python emitted the absolute
  filesystem path (e.g. `/home/user/project/src/foo.mds`) as `sources[0]` in the
  generated Source Map v3. Shipped source maps and inline maps embedded with
  `--inline` could expose the full path of the machine that compiled the template,
  a privacy-significant information disclosure. Fixed by the `relativize_source`
  choke-point in `crates/mds-core/src/source_path.rs` (ADR-005 Phase A): all
  surfaces now emit root-relative paths (e.g. `src/foo.mds`) relative to the
  project root (located via `.mdsroot` / `.git` walk-up), and `..`-escaping
  references outside the project root fall back to the basename. (#3)

- **Control-byte injection hardening (CWE-150 / #176):** Raw C0 / DEL / C1
  control bytes in `.mds` source content could reach terminal stderr and
  JS / Python / WASM API error messages, enabling terminal escape-sequence
  injection. The serialization and diagnostic-render boundaries hardened here are
  `MdsError::serialize()` (inherited by all three binding layers),
  `LintResult::to_canonical_json()` including the `"file"` group key,
  `CompileResult::to_canonical_json()` warnings, and the CLI render path. That is
  an audit list, **not a closed set**: the governing rule is the per-field one
  below, and the residual it leaves is named there. Enumerating boundaries is
  exactly the framing this changelog retires further down.
  The CLI render path (PF-014 redesign) sanitizes the renderer's *source-excerpt*
  input byte-length-preservingly — hostile C0/DEL/C1 bytes become `?` (C0/DEL) or
  NBSP (C1) so span offsets and caret columns stay exact and miette's own SGR
  colour codes survive intact on TTY. `message` and `help` are renderer inputs too,
  but they are `\uXXXX`-escaped rather than length-preserved; only source text
  carries the byte-length invariant. A new
  `MdsError::display_sanitized()` public API is provided for Rust consumers;
  the raw `Display` impl is preserved with an explicit unsafety contract in
  its rustdoc. `span` byte offsets, `fix_edits` byte ranges, and `rule`
  identifiers are deliberate exclusions — they carry position data, not
  terminal-bound text. (#176)

- **CLI error *message* text is now escaped too (#176).** The hardening above
  covered rendered source excerpts, filenames, and the diagnostic wire boundaries, but a
  diagnostic's own message and help text still reached stderr raw. Both CLI error
  families interpolate untrusted input into their messages — compiler errors carry
  template text (`invalid include alias: '<alias>'`) and CLI errors carry `mds.json`
  values and filesystem paths (`mds.json output_dir '<value>' must not contain '..'`)
  — so a hostile `.mds` file or config value could still emit raw ANSI escape
  sequences to a terminal. `mds build`, `check`, `fmt`, `lint`, and `watch` now escape
  each report's message, help, and caret-label text at the single `eprint_error`
  choke-point, *before* the diagnostic renderer runs. The rendered frame is still never
  post-processed, so terminal colour and caret alignment are unaffected, and output for
  well-formed input is byte-for-byte unchanged. (#176)

- **Every CLI print now escapes what it interpolates, and CI enforces it (#176).**
  Warning and status prints scattered across `main.rs`, `build.rs`, `fmt.rs`, `lint.rs`,
  `watch.rs` and `output.rs` interpolated filenames, `mds.json` rule names, `--format`
  arguments and `io::Error` causes into `eprintln!` raw, bypassing the
  `sanitize_control_chars` call that `mds-core`'s `emit_warnings` applies on the primary
  code paths (a PF-004 parallel-path gap). The two most directly reachable:

  - a rule NAME in `mds.json` is an arbitrary JSON object key, and a JSON `\uXXXX`
    escape decodes to a real byte, so any repository could put a raw ESC on a
    developer's stderr — or forge whole `Clean: …` / `0 problems found` status lines —
    just by being linted;
  - the shared directory walker's depth-limit warning named the directory it stopped at,
    so one hostile directory name reached `mds build`, `check`, `fmt`, `lint` and
    `watch` at once.

  All of them now apply the per-field rule below: the warning *body* goes through
  `eprint_warning` (HUMAN), and every value interpolated into it goes through
  `safe_path` / `safe_inline` (WIRE). `watch.rs`'s lifecycle status lines
  (`Watching {}`, `Removed {}`, `warning: could not remove {}: {e}`) — previously
  carved out as a pre-existing gap — are included.

  **This is now a machine-checked invariant, not an enumeration.** A new
  `crates/mds-cli/tests/print_discipline.rs` fails CI if *any* print macro under
  `crates/mds-cli/src/**` interpolates a value that is not passed through one of the
  escape helpers. It applies the same rule to the argument of `eprint_warning`
  (HUMAN-mode escaping alone is not sufficient — it preserves `\n`, which is the
  line-forgery vector), including when the message has been hoisted into a local:
  a bare identifier is traced one hop through its `let` binding and judged the same
  way, and an argument the trace cannot resolve is **reported**, not trusted. Because
  `let`s are matched file-wide, every `for` variable, function parameter and closure
  parameter **poisons** its own name, so a value arriving through one of those is
  reported rather than resolved against an unrelated `let` that happens to share the
  name. It also
  scans `write!` / `writeln!` to a stdout/stderr handle. Deliberate exceptions — the
  compiled artefact written to stdout, `&'static str` labels, integer counters, and
  whole warning strings produced by `mds-core` — live in explicit allowlists with a
  written justification per entry, and a companion test fails if an entry ever stops
  matching. The guard is a **lexical** scanner: it catches accidental reintroduction,
  and its five known limits (name-matched sanitizers, anti-rot-not-anti-reuse
  allowlists, the one-hop single-file trace, name-based stream detection, and the
  `if let` / `while let` / `match`-arm binders the poison set does not model) are stated
  in its own rustdoc rather than implied away. Four successive reviews of this change
  each found a *different* unescaped print; the guard is what ends that.

  The one precondition the guard depends on and cannot check — that `mds-core` WIRE-escapes
  the identifiers its warning producers interpolate, since `mds-cli` prints whole
  warning strings — is now pinned by `crates/mds-cli/tests/producer_discipline.rs` for
  the only producer whose input can carry a hostile character (`resolver.rs`'s
  imported-module filename). The other two producers interpolate an `@include` alias,
  which the parser restricts to `[A-Za-z_][A-Za-z0-9_]*`, so they are upheld by review
  and stated as such rather than claimed to be tested. (#176)

- **The escape mode is chosen per field, not per surface (#176).** Normative in spec
  §7.5: **on the diagnostic surfaces — the `"version": 1` JSON wire, CLI status and
  warning lines, `[file:line:col]` frame headers — untrusted identifiers, filenames and
  error causes are WIRE-escaped, human terminal output included; prose — a diagnostic
  message or help body — stays HUMAN so multi-line frames keep rendering.** The
  rule governs *diagnostic* output; the two carve-outs below are not diagnostics and are
  not escaped at all. The discriminator is whether the
  value is ever legitimately multi-line: a filename, a config key, a `--format`
  argument and an `io::Error` never are, so preserving a raw `\n` in one buys nothing
  and lets it forge a standalone line byte-identical in form to genuine output
  (CWE-117). This supersedes the earlier per-surface framing and the "wire mode at
  exactly four boundaries" enumeration. A new `mds::sanitize_control_chars_wire` and
  `mds::named_source_for_render` are public in `mds-core` for consumers that need to
  apply the same rule.

  **Declared carve-out: functional path references are NOT escaped.** Source-map
  documents (the `mds build --source-map` sidecar, and the `sourceMap` embedded in
  `CompileResult.to_canonical_json()`) emit their `file`, `sources` and `sourcesContent`
  values **verbatim**, as does the `dependencies` array. These are functional references
  that devtools, bundlers and IDEs resolve against the filesystem — rewriting a path to a
  `\uXXXX` literal would point at a path that does not exist, breaking source-map
  resolution and dependency tracking to defend against a pathological filename. That is
  the same product-versus-display distinction that keeps compiled output byte-faithful.
  **Consumers of a source map or of `dependencies` must treat every path in them as
  untrusted** and escape it for whatever destination they render it to; JSON string
  encoding is not that escaping, since a decoded `"\n"` is a real newline again. The CLI
  does not rely on this: its `Compiled to …` and `Source map written to …` lines print
  through `safe_path` and carry the escaped form even though the sidecar does not.
  Specified in spec §7.5 ("Carve-out: functional path references"). (#176)

  **Declared residual.** "Identifier / filename / cause" means the value occupies such a
  *field* — a CLI status line, a `[file:line:col]` frame header, the JSON `file` key. A
  path or identifier interpolated into a diagnostic **message body** is part of prose,
  so it follows the message row and stays HUMAN on terminal surfaces. That applies at
  both message-construction sites: the CLI's `miette::miette!()` reports **and**
  `mds-core`'s `MdsError` message bodies (`fs.rs`'s `cannot read {path}: {e}`,
  `parser_helpers.rs`'s `invalid import alias: '{alias}'`). A `\n` in one of those
  survives into the rendered frame and takes a line there. It is a weaker surface than a
  status line — frame content is indented and `│`-prefixed, and the prefix survives
  `strip()`, so it cannot masquerade as genuine bare status output — and no raw control
  byte reaches the terminal either way. Closing it means WIRE-escaping over a hundred
  `MdsError` construction sites and changing the public message text seen by all three
  binding layers; that is a separate change. Disclosed in spec §7.5 ("Residual: paths and
  identifiers inside a message body") and in the boundary table in
  `crates/mds-core/src/lint/diagnostic.rs`. (#176)

- **Hostile filenames can no longer forge CLI status lines (CWE-117 / #176).** POSIX
  permits a newline inside a filename, and directory-mode commands discover names by
  walking the tree — the user never types them. Filename display used HUMAN mode, which
  preserves newlines by design so that multi-line diagnostic *messages* keep rendering,
  so a file named `evil.mds<LF>Clean: real.mds<LF>OK: all-fine.mds` made
  `mds build`/`lint`/`fmt`/`check` emit attacker-authored lines byte-identical in form
  to genuine status output — unframed, unindented, and indistinguishable. **Diagnostic
  filename fields are now escaped in WIRE mode on every surface that renders one, human
  included** (source-map paths and `dependencies` are the declared carve-out): `safe_path`
  and the status-line printers, and the `[file:line:col]` frame header via a new shared
  `mds::named_source_for_render` builder that `MdsError::at()`, the formatter and the
  lint renderer all call. Message and help text are unchanged (still HUMAN, still
  multi-line). A filename is never legitimately multi-line, so nothing legitimate is
  lost. (#176)

- **Fix-rejection reasons are display-safe by construction (#176).**
  `mds::fix::FixOutcome::Rejected.reason` interpolated an `MdsError`'s deliberately-raw
  `Display` — whose variants embed template text (`syntax error: {message}`) and
  filesystem paths (`file not found: {path}`) — and the CLI prints that value as an
  unframed `fix rejected: {reason}` status line. The embedded error is now escaped in
  WIRE mode at the single construction site in `fix.rs`, so the field is single-line and
  control-byte-free for **every** consumer of the published `mds::fix` API, not just the
  CLI's own print sites. (#176)

- **Widened escape class: bidi / separator / BOM characters (#176).** The
  escaped set now covers characters outside C0 / DEL / C1 that are still
  display-hazardous, on every surface that escapes:
  - **U+061C, U+200E, U+200F, U+202A–U+202E, U+2066–U+2069** — the complete
    Unicode `Bidi_Control=Yes` set (all twelve codepoints), behind Trojan Source
    (CVE-2021-42574). A single U+202E in a filename or diagnostic message
    reverses how the rest of the line renders in any bidi-aware terminal, IDE, or
    code-review UI. U+061C ARABIC LETTER MARK is the only member outside
    U+200E–U+2069 and is easy to miss for exactly that reason.
  - **U+2028, U+2029** — LINE / PARAGRAPH SEPARATOR, which terminate a
    JavaScript string literal.
  - **U+FEFF** — BOM / ZWNBSP, invisible in every renderer.

  Each becomes its uppercase six-character `\uXXXX` literal, exactly like the
  existing C0 / DEL / C1 escapes. Source excerpts inside a rendered diagnostic
  frame are neutralized to a **same-width** substitute instead, preserving the
  byte-length invariant that keeps span offsets and caret columns exact: 1-byte
  C0/DEL → `?`, 2-byte C1 and U+061C → U+00A0, 3-byte bidi controls, separators
  and BOM → U+FFFD. (#176)

- **BREAKING (wire format): machine-readable boundaries now escape `\n` (#176).**
  `MdsError::serialize()`, `LintResult::to_canonical_json()` (message, help, and
  the `"file"` group key), `CompileResult::to_canonical_json()` warnings, and the
  Python typed lint surface now emit `\n` as the six-character `\u000A` literal.
  A raw newline inside a diagnostic string is a line-forging vector: any consumer
  that prints or line-splits the value can be made to render an attacker-authored
  line as a genuine second finding. `\t` is unaffected.

  **Human-render output of diagnostic PROSE is unchanged** — the CLI renderer,
  `MdsError::display_sanitized()`, and warning *bodies* on stderr still preserve
  raw newlines so multi-line diagnostic frames stay readable. Human output of
  diagnostic filenames, identifiers and causes **did** change, by design: under the
  per-field rule above those are WIRE-escaped on every surface that renders a
  diagnostic, human included, so a newline
  in one now renders as the six-character `\u000A` literal instead of forging a
  line. Source-map paths and `dependencies` are unaffected: they are the declared
  carve-out and stay verbatim. See the two entries below.

  **Migration:** consumers that split a `message` / `help` / warning string on
  `\n` will now see a single line containing the literal `\u000A` where a real
  newline used to be. Split on that literal instead, or render the value verbatim.

- **Escaping is one-way — consumers must not un-escape (#176).** The
  transformation is lossy and non-injective by design: a template that literally
  contains the six characters `\u001B` and one containing an actual ESC byte are
  indistinguishable after serialization. **Do not** convert `\uXXXX` sequences
  back into bytes — that reconstitutes the injection the escape prevents. Round-tripping is
  an explicit non-goal; no backslash-escaping will be added to make the mapping
  reversible. Consumers needing original bytes must read them from the source via
  the raw `span` / `fix_edits` byte offsets, which stay unsanitized for this
  purpose. Documented normatively in spec §7.5.

- **`--diff` preview output is TTY-gated (#176).** Applies to both
  `mds lint --fix --diff` and `mds fmt --diff`, which share one renderer.
  Preview diff text is neutralized when stdout is a terminal (where control bytes
  would execute) and emitted **byte-faithful when piped or redirected**, so a
  redirected diff remains applicable. Preview output is not part of the
  `"version": 1` JSON wire format.

## [0.3.0] — 2026-06-28

### **BREAKING** — Intrinsic output format (removes `--format` flag and `compileMessages` API)

Output shape is now **intrinsic to the template** — decided by content, never a flag:

- A template containing any `@message` block → JSON messages array (`.json`).
  Detection is static: a `@message` even inside an `@if` branch that is never taken at
  runtime makes the template a messages template.
- All other templates → Markdown string (`.md`).
- **Mixed content** (loose top-level prose or interpolations alongside `@message` blocks)
  is a hard compile error: `mds::mixed_content`. There is no silent drop or auto-wrap.
- A messages template that yields zero messages at runtime emits `[]`.

**Removed** (breaking):

- CLI `--format` flag — passing it is now an unknown-argument error.
- Rust `compile_messages_str`, `compile_messages_str_with_deps`,
  `compile_messages_virtual`, `compile_messages_virtual_with_deps` (mds-core).
- JS `compileMessages` / `compileMessagesFile` from `@mdscript/mds`, `@mdscript/mds-napi`,
  and `@mdscript/mds-wasm`.
- JS type `CompileMessagesResult` — superseded by the discriminated `CompileResult` union.

**New API** (replacing the above):

- `compile(src, opts?)` / `compileFile(path, opts?)` → discriminated union
  `{ kind:'markdown', output:string, warnings, dependencies }` |
  `{ kind:'messages', messages:Message[], warnings, dependencies }`.
  The inactive payload field (`output` or `messages`) is absent — branch on `kind`.
- Rust `compile_str` / `compile_virtual` / `compile_file` return `CompileResult` with the
  same discriminated shape; use `.into_markdown()` / `.into_messages()` to extract.
- Bundler plugins: importing a `.mds` (or `.md` with `type: mds` frontmatter)
  default-exports a **string** for Markdown templates, a **`Message[]` array** for
  messages templates. The ambient type declaration in `@mdscript/bundler-utils/mds` is
  `string | MdsMessage[]`.

### **BREAKING** — `mds build/check <dir>` directory mode

`mds build` and `mds check` now accept a directory argument:

- Compiles (or validates) every non-partial `.mds` file in the tree recursively.
- Output extension is intrinsic per file (`.md` or `.json`).
- With `--out-dir <out>`, mirrors the source subtree; without it, writes next to source.
- `_`-prefixed files are partials: they are skipped (not compiled to output).
- Symlinked files and symlinked directories inside the tree are skipped; a symlinked
  entry root is rejected at startup with a non-zero exit.
- `-o` is rejected for a directory input.
- Continue-on-error: all files are attempted; summary + non-zero exit if any failed.
- Empty directory: exits 0 with a "no .mds files found" message.
- **Stale-flip cleanup**: when a file's kind changes (Markdown ↔ messages), the old
  sibling output (`.md` or `.json`) is deleted automatically.

### **BREAKING** — `mds watch` output layout change (dir mode)

- **`--out-dir` and `mds.json output_dir` now mirror the source subtree** instead of
  using a flat stem. `src/a/b/foo.mds` now compiles to `out/a/b/foo.md` (was `out/foo.md`).
  Old flat outputs are orphaned on disk — no auto-migration. Users with `--out-dir`
  must delete stale flat outputs manually. Zero published users.
- **`_`-prefixed files are now treated as partials**: they are tracked in the dependency
  graph and trigger rebuilds of their importers, but they no longer emit their own output
  file. Rename any `_`-prefixed files you previously wanted to compile to a name without
  a leading underscore.

### Language features

- `@extends "./base.mds"` + `@block name: … @end` template inheritance — a child template
  extends a base, overriding named `@block` placeholders; only the root base declares block
  names. Frontmatter deep-merges base < child < runtime (arrays replace wholesale). New error
  code `mds::extends`; new limits `MAX_BLOCKS_PER_MODULE` (256) and
  `MAX_FRONTMATTER_MERGE_DEPTH` (64). See spec §4.11.
- `@message role: … @end` blocks for structured chat-message output. Roles may be
  bare words (literal strings) or `{expr}` (evaluated at runtime using the full
  expression grammar). Output kind is intrinsic — see above.

### CLI

- `mds watch` subcommand: watches an `.mds` file (or directory) and auto-recompiles on
  save. Single-file mode tracks transitive `@import` deps — editing any imported file
  triggers a rebuild. Directory mode tracks a **reverse-dependency graph**: editing a
  shared partial recompiles all transitive importers. Full flag parity with `mds build`:
  `-o`, `--out-dir`, `--vars`, `--set`, `--clear` (clears terminal before rebuild when
  stderr is a TTY), `--debounce` (milliseconds, default 100), `--poll-interval`
  (self-heal tick in ms, default 1000; `0` disables). Status/warnings/errors go to
  stderr; compiled content goes to stdout only when `-o -`. Ctrl+C exits cleanly with
  code 0. Depends on `notify 8` and `ctrlc 3.5` (both compatible with MSRV 1.88).
- Internal refactor: shared output-path and directory-traversal logic extracted to
  `crates/mds-cli/src/output.rs` (`output_path_for`, `collect_mds_files`,
  `probe_and_remove_stale`, etc.) to remove duplication across `build.rs` and `watch.rs`.

### Security & resource limits

- `mds watch` now rejects a symlinked entry file, symlinked `--vars` file, or symlinked
  directory target at startup (non-zero exit, symlink message on stderr) — parity with
  `mds build`. Previously `mds watch` called `std::fs::canonicalize` up-front, silently
  following symlinks that `mds build` would reject via the core `NativeFs::check_symlink`
  guard. The fix routes watch startup through the same guard. Symlinked `.mds` source
  files inside a watched directory continue to be skipped at discovery.
- `MAX_MESSAGE_COUNT` (10,000) cap: templates exceeding this limit return a resource
  error rather than allocating unboundedly.
- Cumulative message-content size cap (50 MB): enforced per-compile across all
  `@message` blocks.

### Library API (additions)

- `NativeFs::check_symlink(path: &Path) -> Result<PathBuf, MdsError>` is now `pub`
  (was `pub(crate)`). Canonicalizes `path` and rejects a symlinked final component;
  returns the canonical `PathBuf` for non-symlinks. Callable as
  `mds::NativeFs::check_symlink(path)`.

### Ecosystem

- New package `@mdscript/rspack-loader`: Rspack loader for importing `.mds` templates
  as ES modules. Mirrors `@mdscript/webpack-loader` — delegates to the shared
  `createMdsLoader()` factory in `@mdscript/bundler-utils`, peer-depends on
  `@rspack/core ^1.0.0 || ^2.0.0` (verified against Rspack 1.x and 2.x), and ships
  dual ESM + CJS builds. Published as the 8th coordinated package in the
  `@mdscript` release.
- Verified and hardened HMR behaviour across all four bundler integrations
  (Vite, Rollup, Webpack, Rspack). Each integration now has a documented HMR contract,
  known-limitation notes (AC-E1/E2/E3), and spec-level tests.
- **Vite fix**: `handleHotUpdate` now correctly triggers a full-page reload for `.md`
  files with `type: mds` frontmatter and for files tracked only as transitive `@import`
  dependencies. Previously only bare `.mds` extension files were detected. The fix adds
  a closure-level `transformed` Set with a `canon()` path-normalization helper that
  resolves symlinks (macOS `/tmp` → `/private/tmp`) and strips Vite query suffixes.

## [0.2.0] — 2026-06-06

### Language features

- 18 built-in functions: `upper`, `lower`, `trim`, `trim_start`, `trim_end`,
  `replace`, `split`, `join`, `length`, `contains`, `starts_with`, `ends_with`,
  `repeat`, `substring`, `reverse`, `default`, `number`, `string`
- Default function arguments: `@define greet(name, greeting = "Hello"):`
- Logical operators in conditions: `@if a && b:`, `@if a || b:` with `&&`
  binding tighter than `||`
- Expression support in `@for` and `@if` directives — function calls and
  chained expressions can be used directly in directive arguments
- Frontmatter imports: declare dependencies in YAML frontmatter alongside
  variables, replacing or supplementing `@import` directives in the body

### Performance

- Re-enabled `wasm-opt` with `-Oz` optimization (Binaryen v129) for smaller
  WASM binary output

### Internal

- Consolidated cross-module resource-limit constants into `crates/mds-core/src/limits.rs`
- Split `parser.rs` into focused modules: `parser.rs` (core), `parser_helpers.rs` (helpers), and `parser_tests.rs` (tests)
- Updated all dependencies and CI actions (TypeScript 6, Vite 8, actions v6/v7/v8)

## [0.1.0] — 2026-05-31

First public release of the MDS (Markdown Script) compiler.

### Language features

- Variable interpolation from YAML frontmatter (`{name}`)
- `@if`/`@elseif`/`@else`/`@end` conditionals with full MDS truthiness rules,
  negation (`@if !feature_enabled`), and equality/inequality comparisons against
  string, number, boolean, or null literals (`@if role == "admin"`, `@if count != 0`)
- `@for item in list:` loops over arrays
- `@define` function definitions with parameters and lexical scoping
- `@import` directives: alias (`as ns`), merge, and selective (`{ a, b }`)
- `@export` directives: named, re-export from module, wildcard re-export
- `@include ns` to inline the prompt body of an imported module
- Escaped braces (`\{` produces `{`)
- Frontmatter `type: mds` marker to allow `.md` files as MDS sources
- String literal arguments with single- and double-quote delimiters
- `NaN` and `Infinity` numeric literals are rejected at parse time with a clear error

### Compiler pipeline

- Lexer with token types for all MDS syntax elements
- Recursive-descent parser producing a typed AST
- Module resolver with `Arc<ResolvedModule>` caching and cycle detection
- Semantic validator (undefined variables/functions, arity, type checks)
- Evaluator with `EvalContext` threading (call stack, iteration counting, warnings)
- `mds.json` project config with `build.output_dir`

### CLI (`mds` binary)

- `mds build`: compile `.mds` to Markdown with auto-detection, `--out-dir`, `--set`, `--vars`
- `mds check`: validate without rendering
- `mds init`: create a starter template
- Stdin mode (`mds build -`)
- Categorized exit codes (0 success / 1 template error / 2 I/O error / 3 resource limit)
- Rich miette diagnostics with source spans
- Global `--quiet` flag

### Security & resource limits

- Path traversal prevention for imports and config `output_dir`
- Symlink rejection in import paths
- File size limits (10 MB per file, 1 MB for `mds.json`)
- Resource limits: call depth (128), loop iterations (100 K per loop, 1 M total),
  output size (50 MB), warnings (1000)
- Block nesting depth limit of 64 for `@if`/`@for`/`@define` (guards against
  stack overflow on adversarial input)
- YAML/JSON value nesting depth limit (64 levels)
- Non-UTF-8 paths are rejected at the public API boundary with an explicit error
  rather than producing corrupted output

### Library API (`mds-core` crate, imported as `mds`)

- `compile()`, `compile_str()`, `compile_str_with()`, `compile_file()`: render to `String`
- `check()`, `check_str()`, `check_str_with()`: validate without rendering
- `compile_collecting_warnings()`, `compile_str_collecting_warnings()`: render and
  return `(String, Vec<String>)` for caller-controlled warning output
- `check_collecting_warnings()`, `check_str_collecting_warnings()`: validate and
  return `((), Vec<String>)` for caller-controlled warning output
- `load_vars_file()`: load runtime variables from JSON
- `#[non_exhaustive]` on the public `MdsError` and `Value` enums

### JavaScript / TypeScript packages

- **`@mdscript/mds`**: universal bindings for the MDS compiler
  - Node.js entry auto-selects the native addon (`mds-napi`) with WASM fallback
  - Browser entry via WASM; requires `init()` before use
  - API: `compile`, `check`, `compileFile`, `checkFile`, `getBackend`, `init`, `isMdsError`
  - `isMdsError()` identifies MDS errors by an `Error` instance whose `code` starts with `"mds::"`
  - `MDS_BACKEND` environment variable to force the `native` or `wasm` backend
  - Full TypeScript types with JSDoc
- **Bundler integration**: import `.mds` templates natively in JS/TS bundlers
  - `@mdscript/bundler-utils`: shared transform, frontmatter detection, error
    formatting, and a concurrency-safe `LazyInit<T>` utility
  - `@mdscript/vite-plugin`: Vite transform hook with HMR support (`vite ^5 || ^6`)
  - `@mdscript/rollup-plugin`: Rollup 3/4 transform hook
  - `@mdscript/webpack-loader`: Webpack 5 async loader (ships ESM + CommonJS)
  - All plugins accept `{ vars?: Record<string, unknown> }` for template variables
  - TypeScript module declarations (`.mds` → `string`) via `@mdscript/bundler-utils/mds`

### Tests

- 590 Rust tests (integration, unit, and doc-tests across the workspace) plus the JavaScript package suites

[Unreleased]: https://github.com/dean0x/mdscript/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/dean0x/mdscript/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/dean0x/mdscript/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/dean0x/mdscript/releases/tag/v0.1.0
