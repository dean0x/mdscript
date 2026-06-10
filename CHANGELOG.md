# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### CLI

- `mds watch` subcommand: watches an `.mds` file (or directory) and auto-recompiles on
  save. Single-file mode tracks transitive `@import` deps — editing any imported file
  triggers a rebuild. Directory mode tracks a **reverse-dependency graph**: editing a
  shared partial recompiles all transitive importers. Full flag parity with `mds build`:
  `-o`, `--out-dir`, `--vars`, `--set`, `--format` (single-file only), `--clear`
  (clears terminal before rebuild when stderr is a TTY), `--debounce` (milliseconds,
  default 100), `--poll-interval` (self-heal tick in ms, default 1000; `0` disables).
  Status/warnings/errors go to stderr; compiled content goes to stdout only when `-o -`.
  Ctrl+C exits cleanly with code 0. Depends on `notify 8` and `ctrlc 3.5` (both
  compatible with the workspace MSRV of 1.88).
- Internal refactor: build logic extracted from `main.rs` into `build.rs` with
  `pub(crate)` helpers; `compile_and_write` shared helper routes both markdown and
  messages modes through `read_build_input` / `compile_with_deps`, preserving the
  10 MiB file-size cap on all code paths (PF-004).

### **BREAKING** — `mds watch` output layout change (dir mode)

- **`--out-dir` and `mds.json output_dir` now mirror the source subtree** instead of
  using a flat stem. `src/a/b/foo.mds` now compiles to `out/a/b/foo.md` (was `out/foo.md`).
  Old flat outputs are orphaned on disk — no auto-migration. Users with `--out-dir`
  must delete stale flat outputs manually. Zero published users.
- **`_`-prefixed files are now treated as partials**: they are tracked in the dependency
  graph and trigger rebuilds of their importers, but they no longer emit their own `.md`
  output file. Rename any `_`-prefixed files you previously wanted to compile to
  a name without a leading underscore.

### Language features

- `@message role: … @end` blocks for structured chat-message output. Roles may be
  bare words (literal strings) or `{expr}` (evaluated at runtime using the full
  expression grammar). Templates without `@message` blocks compile identically to
  before; in `markdown` mode `@message` body content renders inline.

### CLI

- `mds build --format messages` emits a pretty-printed JSON `[{role, content}]` array
  to stdout or `-o <path>`; `--out-dir` and `mds.json` `output_dir` are ignored in
  this mode.

### Library API

- `compile_messages_str`, `compile_messages_str_with_deps`,
  `compile_messages_virtual`, `compile_messages_virtual_with_deps` (mds-core)
- `compileMessages(source, options?)` exported from `@mdscript/mds` (both NAPI
  and WASM backends)
- `compileMessages` exported directly from `@mdscript/mds-napi` (native addon)
- `compileMessages` exported from `@mdscript/mds-wasm` (WASM module)
- New public types: `Message` (`{ role: string; content: string }`) and
  `CompileMessagesResult` (`{ messages, warnings, dependencies }`)

### Security & resource limits

- `MAX_MESSAGE_COUNT` (10,000) cap: templates exceeding this limit return a resource
  error rather than allocating unboundedly.
- Cumulative message-content size cap (50 MB): enforced per-compile across all
  `@message` blocks.

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

[Unreleased]: https://github.com/dean0x/mdscript/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/dean0x/mdscript/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/dean0x/mdscript/releases/tag/v0.1.0
