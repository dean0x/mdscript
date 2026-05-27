# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Negation in `@if` conditions** — prefix a dot-path with `!` to negate it (`@if !feature_enabled`).
- **Equality comparisons in `@if`/`@elseif`** — compare a dot-path against a string, number, boolean, or null literal using `==` or `!=` (`@if role == "admin"`, `@if count != 0`).
- **`@elseif` directive** — chain multiple conditional branches without nesting (`@if`/`@elseif`/`@else`/`@end`).
- **NaN and Infinity rejection** — numeric literals that parse to `NaN` or infinite values are now rejected at parse time with a clear error.
- **CJS build for `@mds/webpack-loader`** — Webpack 5 compatibility; the package now ships both ESM and CommonJS entry points.
- **`LazyInit<T>` utility in `@mds/bundler-utils`** — concurrency-safe lazy initialization with deduplication of concurrent factory calls, retry-on-reject semantics, and a TOCTOU-safe `reset()`. Extracted from the webpack-loader for shared use across bundler plugins (#32).
- **API surface tests and non-UTF-8 rejection tests** — `api_surface.rs` covers the new `&str` signatures for `resolve_path` and `resolve_source`; Unix-only tests verify that non-UTF-8 `OsStr` paths are rejected at the boundary with a clear error (#12).
- **Bundler integration packages** — import `.mds` templates natively in JavaScript/TypeScript bundlers
  - `@mds/bundler-utils` — shared transform, frontmatter detection, and error formatting utilities
  - `@mds/vite-plugin` — Vite transform hook with HMR support (`vite ^5 || ^6`)
  - `@mds/rollup-plugin` — Rollup 3/4 transform hook
  - `@mds/webpack-loader` — Webpack 5 async loader
  - All plugins accept `{ vars?: Record<string, unknown> }` for template variables
  - TypeScript module declarations via `@mds/bundler-utils/mds`
- **`@mds/mds` npm package** — universal JavaScript/TypeScript bindings for the MDS compiler
  - Node.js entry auto-selects the native addon with WASM fallback
  - Browser entry via WASM; requires `init()` before use
  - API: `compile`, `check`, `compileFile`, `checkFile`, `getBackend`, `init`, `isMdsError`
  - `MDS_BACKEND` environment variable to force `native` or `wasm` backend
  - Full TypeScript types with JSDoc

### Changed

- **`MAX_NESTING_DEPTH` reduced from 256 to 64** — the maximum nesting depth for `@if`/`@for`/`@define` blocks is now 64 (down from 256). This prevents stack overflow on crafted adversarial inputs while remaining generous for real templates. Consumers relying on nesting deeper than 64 levels must flatten their templates. (`mds-core` crate breaking change.)
- **`isMdsError()` stricter identification** — the function now requires the `code` property to start with `"mds::"` in addition to being an `Error` instance with a string `code`. Consumers who previously created synthetic error objects with arbitrary `code` strings and relied on `isMdsError()` returning `true` must prefix their codes with `"mds::"` or use a separate check.
- **`ModuleCache::resolve_path` and `ModuleCache::resolve_source` accept `&str` instead of `&Path`** — eliminates silent UTF-8 corruption on non-UTF-8 paths; non-UTF-8 paths now fail with an explicit error at the public API boundary rather than producing garbled output. Rust library consumers calling these methods must pass `&str`; this is a breaking change for direct users of the `mds-core` crate (#23, #12).

## [0.1.0] — 2026-05-15

Initial release of the MDS (Markdown Script) compiler.

### Added

**Language features**
- Variable interpolation from YAML frontmatter (`{name}`)
- `@if`/`@else`/`@end` conditionals with full MDS truthiness rules
- `@for item in list:` loops over arrays
- `@define` function definitions with parameters and lexical scoping
- `@import` directives: alias (`as ns`), merge, and selective (`{ a, b }`)
- `@export` directives: named, re-export from module, wildcard re-export
- `@include ns` to inline the prompt body of an imported module
- Escaped braces (`\{` produces `{`)
- Frontmatter `type: mds` marker to allow `.md` files as MDS sources
- String literal arguments with single-quote delimiters

**Compiler pipeline**
- Lexer with token types for all MDS syntax elements
- Recursive-descent parser producing a typed AST
- Module resolver with `Arc<ResolvedModule>` caching and cycle detection
- Semantic validator (undefined variables/functions, arity, type checks)
- Evaluator with `EvalContext` threading (call stack, iteration counting, warnings)
- `mds.json` project config with `build.output_dir`

**CLI** (`mds` binary)
- `mds build` — compile `.mds` to Markdown with auto-detection, `--out-dir`, `--set`, `--vars`
- `mds check` — validate without rendering
- `mds init` — create a starter template
- Stdin mode (`mds build -`)
- Categorized exit codes (0/1/2/3)
- Rich miette diagnostics with source spans

**Security**
- Path traversal prevention for imports and config `output_dir`
- Symlink rejection in import paths
- File size limits (10 MB per file, 1 MB for `mds.json`)
- Resource limits: call depth (128), loop iterations (100 K per loop, 1 M total), output size (50 MB), warnings (1000)
- YAML/JSON value nesting depth limit (64 levels)

**Library API** (`mds` crate)
- `compile()`, `compile_str()`, `compile_str_with()`, `compile_file()` — render to String
- `check()`, `check_str()`, `check_str_with()` — validate without rendering
- `compile_collecting_warnings()`, `compile_str_collecting_warnings()` — render and return `(String, Vec<String>)` for caller-controlled warning output
- `check_collecting_warnings()`, `check_str_collecting_warnings()` — validate and return `((), Vec<String>)` for caller-controlled warning output
- `load_vars_file()` — load runtime variables from JSON

**Tests**
- 292 tests covering integration, unit, and doc-tests
