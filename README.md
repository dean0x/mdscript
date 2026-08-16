# MDS - Markdown Script

[![CI](https://github.com/dean0x/mdscript/actions/workflows/ci.yml/badge.svg)](https://github.com/dean0x/mdscript/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/mds-cli.svg)](https://crates.io/crates/mds-cli)
[![npm](https://img.shields.io/npm/v/@mdscript/mds.svg)](https://www.npmjs.com/package/@mdscript/mds)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

LLM prompts grow into copy-pasted walls of text that drift across agents, models, and environments. MDS gives you variables, functions, imports, and conditionals so you can write prompts once and compose them everywhere, compiled to clean Markdown.

Built for AI engineers who manage prompt libraries across agents, models, and environments.

## Quick Start

**Install the CLI** (Rust):

```bash
cargo install mds-cli
```

**Or install via npm** (Node or browser):

```bash
npm install @mdscript/mds
```

**Create and compile a starter template**:

```bash
mds init                       # creates hello.mds
mds build hello.mds            # compiles to hello.md
mds build hello.mds -o -       # stdout
```

**For multi-file prompt libraries**, templates compose via `@import` — for example:

```
---
model: claude-sonnet
tools: [search, calculator]
---

@import "./safety.mds" as guard
@import "./personas.mds" as persona

{{persona.code_reviewer("TypeScript")}}

{{guard.safety_rules()}}

## Available Tools

@for tool in tools:
- **{{tool}}**
@end

@if model == "claude-sonnet":
Use extended thinking for complex tasks.
@end
```

See [`examples/prompt-library/`](examples/prompt-library/) for a complete reusable
prompt library using `@export`/`@import` (personas, formatting, guardrails).

Unlike general-purpose template engines, MDS is Markdown-native: no delimiters to escape, no runtime to configure. The compiler catches undefined variables, import cycles, and arity mismatches at build time, not in production.

## Features

- **Variables**: YAML frontmatter or runtime `--set KEY=VALUE` flags
- **Conditionals**: `@if`/`@elseif`/`@else`/`@end` with negation and equality comparisons
- **Loops**: `@for item in list:` iteration over arrays and objects
- **Functions**: `@define` reusable blocks with parameters
- **Imports/Exports**: modular prompt libraries with alias, merge, and selective imports
- **Messages**: `@message role: … @end` blocks compile to a JSON `[{role, content}]` array (`.json`); all other templates compile to Markdown (`.md`) — output format is intrinsic to the template content
- **Security**: path traversal guards, symlink rejection, file size limits
- **Rich errors**: source-span diagnostics with line/column context

## CLI Reference

```
mds build [FILE|DIR] [OPTIONS]  Compile an MDS template or directory to Markdown / JSON
mds watch [FILE|DIR] [OPTIONS]  Watch and auto-recompile on save
mds check [FILE|DIR] [OPTIONS]  Validate without rendering
mds fmt [FILE|DIR] [OPTIONS]    Reformat MDS file(s) in place (opinionated, safety-gated)
mds lint [FILE|DIR] [OPTIONS]   Static-analysis lint (10 rules; --fix, --format json)
mds init [FILENAME]             Create a starter MDS file

Global options:
  -q, --quiet                 Suppress status and diagnostic output; errors always print; exit codes unaffected

Build/Watch options:
  -o, --output <PATH>         Output file, or "-" for stdout (build and single-file watch only;
                              rejected in directory watch mode — use --out-dir instead)
  --out-dir <DIR>             Output directory (build/single-file watch: <stem>.md or <stem>.json;
                              dir-mode watch: mirrors source subtree)
  --vars <FILE>               JSON file with variable overrides (reloaded each rebuild)
  --set KEY=VALUE             Set a single variable (repeatable); value coerced to number/bool/null/array when possible
  --set-string KEY=VALUE      Set a single variable as a string, bypassing type coercion (repeatable)
  --source-map                Write a Source Map v3 sidecar (<output-file>.map, e.g. -o out.md → out.md.map); output is
                              byte-identical to a no-flag build. Ignored for messages-mode
                              templates (no renderable output). See ⚠ privacy note below.
  --inline                    Embed the source map as a sourceMappingURL data-URI comment
                              at the end of the output; no sidecar is written.
                              Requires --source-map.
  --no-source-map             Suppress source-map generation for this invocation, even when
                              build.source_map=true is set in mds.json.
  --embed-sources             Include original source text in the map's sourcesContent field.
                              Requires --source-map. ⚠ Embeds full template text — avoid if
                              templates contain secrets or PII.

Watch-only options:
  --clear                     Clear terminal before each rebuild (only when stderr is a TTY)
  --debounce <MS>             Debounce window in milliseconds (default: 100)
  --poll-interval <MS>        Liveness-probe interval in milliseconds (default: 1000).
                              0 disables self-heal (native events only). Clamped to ≥50ms.
                              The watcher self-heals after a watched dir/root is deleted and
                              recreated; --poll-interval controls how quickly it detects recovery.

Fmt options:
  --check                     Read-only: exit non-zero if any file would change; never writes
  --diff                      Read-only: print a unified diff of pending changes; never writes
                              (colorized only when stdout is a terminal). Combines with --check —
                              --diff controls what's printed, --check controls the exit code.

Exit codes:
  0   Success (or clean Ctrl+C in watch mode; or a clean `fmt --check` / `fmt --diff` preview)
  1   Template error (syntax, undefined variable, arity mismatch), or `fmt --check` found a
      file that would change
  2   I/O error (file not found, not an MDS file), or invalid CLI argument (clap parse error)
  3   Resource limit exceeded
```

**Directory mode** (`mds build <dir>` / `mds check <dir>`): every non-partial `.mds` file under the directory is compiled, with two automatic exclusions: directories whose name starts with `.` (e.g. `.git`, `.github`, `.claude`, `.cursor`) and `node_modules` are skipped during traversal. `_`-prefixed files are partials — tracked as dependencies but never emitted to their own output. Output mirrors the source subtree (e.g. `src/a/b/foo.mds` → `dist/a/b/foo.md`). Symlinks are rejected. Errors are per-file and do not abort the run; a summary (`N built, N failed`; `N passed, N failed` for `check`) is printed on a successful run or when any file fails; the exit code is non-zero if any file fails. Under `--quiet`, the summary is suppressed on a fully-successful run but is always emitted when any file fails, so the non-zero exit is never unexplained. If **every** `.mds` file is under a default-excluded directory, the command exits non-zero and prints a diagnostic carrying the skip count — even under `--quiet` — because this is the silent CI green-pass failure mode for prompt-template libraries stored under `.github/prompts/`, `.claude/`, or `.cursor/rules/`. A genuinely empty directory (no `.mds` files anywhere) still exits 0 with a "No .mds files found" message. Stale output files (compiled outputs with no corresponding source) are cleaned up automatically. The output extension is intrinsic: `.md` for Markdown templates, `.json` for templates with `@message` blocks.

`mds fmt <dir>` follows the same directory-mode conventions (recursive, symlinks rejected, continue-on-error, non-zero exit summary) with one deliberate difference: it formats `_`-prefixed **partials too** — formatting rewrites source, not compiled output, and a partial's source is just as much a candidate for reformatting as any other file.

### Live preview with `mds watch`

Watch a single file and recompile whenever it (or any of its imports) changes:

```bash
mds watch system.mds            # recompiles to system.md on every save
mds watch system.mds -o -       # stream output to stdout
mds watch system.mds --clear    # clear terminal before each rebuild
mds watch system.mds --vars vars.json  # with variable overrides
```

Watch an entire directory:

```bash
mds watch src/                  # compile each .mds next to its source
mds watch src/ --out-dir dist   # mirror source subtree under dist/
                                # src/a/b/foo.mds → dist/a/b/foo.md  (not dist/foo.md)
```

> **Changed in v0.4.0:** Directory mode with `--out-dir` or `mds.json output_dir`
> now mirrors the source subtree instead of writing flat stems. Old flat outputs are
> orphaned and must be removed manually.

**Single-file mode** tracks transitive imports: editing any `@import`-ed file triggers a
recompile of the entry. **Directory mode** tracks a reverse-dependency graph: editing a
shared partial rebuilds **all transitive importers** automatically.

- `_`-prefixed files are **partials**: tracked in the dependency graph and their importers
  are rebuilt when edited, but the partial itself never emits its own `.md` output.
- **Cross-root imports**: if a file imports a partial located outside the watched root
  (e.g. `../shared/_x.mds`), editing that external partial rebuilds its in-root importers.
  The external file is never compiled to its own output.

- Status lines and warnings go to stderr (pipe-safe). Compiled content only goes to stdout when `-o -`.
- `--quiet` suppresses status and warnings; compile errors still print and the watcher keeps running.
- Ctrl+C exits with code 0 and prints `Stopped watching.`
- `--vars` file is reloaded from disk on every rebuild; edits to it trigger a recompile.

### Formatting with `mds fmt`

An opinionated, safety-gated auto-formatter. Every rewrite is guaranteed **compile-equivalent**:
before writing anything, `mds fmt` re-compiles the formatted source and refuses to write if it
would change compiled output — a formatter bug surfaces as a clean error, never a silent
corruption of your template.

```bash
mds fmt template.mds            # format a file in place
mds fmt .                       # format every .mds file recursively (partials included)
mds fmt --check template.mds    # exit 1 if the file would change; never writes — for CI
mds fmt --diff template.mds     # print a unified diff of pending changes; never writes
mds fmt --check --diff .        # show diffs for every file that would change; exit 1 if any would
printf '@if ready:   \nGo\n@end\n' | mds fmt -   # format from stdin, write to stdout; creates no file
```

What it normalizes:

- CRLF → LF, everywhere (including inside frontmatter and code fences)
- Trailing whitespace on `@if`/`@for`/`@define`/… directive lines is stripped
- Exactly one final newline (empty or whitespace-only input formats to an empty file)

What it deliberately leaves untouched:

- Trailing whitespace on body-text content lines — two trailing spaces are a Markdown hard line
  break; stripping them would change rendered output
- Blank-line structure within the file body, frontmatter, and code fences — the formatter does
  not add or remove blank lines; blank-line layout is the template author's choice
- The byte-for-byte content of `@message`/`@define` bodies — whitespace inside these bodies
  reaches compiled output verbatim and must not be altered

Directory mode formats every `.mds` file recursively, **including `_`-prefixed partials**,
continuing past per-file errors and printing a summary
(`N formatted, M unchanged, K failed`, or `N would reformat, M unchanged, K failed` under `--check`). A file
is only written (and its mtime touched) when its content actually changes. Status lines and
summaries go to stderr; `--diff` output and stdin filter-mode content go to stdout; `--quiet`
suppresses status but never errors. Reads a `fmt` section from `mds.json`
(`{"fmt": {"sort_frontmatter_keys": true}}`) for forward compatibility — the field doesn't drive
any formatting behavior yet; frontmatter key sorting is deferred to a future version.

### Static analysis with `mds lint`

A 10-rule static analyzer that catches common template authoring issues:

```bash
mds lint template.mds           # lint a single file
mds lint .                      # lint all .mds files recursively (partials included)
mds lint --fix template.mds     # auto-fix fixable issues in place
mds lint --format json .        # machine-readable JSON output (stdout)
mds lint --quiet template.mds   # suppress output; exits 1 on warnings, 2 on errors
mds lint --quiet .              # directory lint: silent on clean/warn-only; summary prints on errors or resource limits
```

Directory mode (`mds lint <dir>`) lints every `.mds` file recursively (partials included) and
prints one summary line to stderr after processing all files:
`N clean, N with warnings, N with errors, N resource-limited`.
Under `--quiet`, the summary is suppressed when the worst outcome is warnings or clean; it is
always printed when any file has errors or hits a resource limit, so the non-zero exit is never
unexplained in those cases. Exception (D1-a): a warn-only run (`mds lint --quiet <dir>`) exits 1
with zero stderr; `mds lint --fix --check --quiet <dir>` with pending fixes also exits 1 with zero
stderr — both are intentional and documented in `--help`. The JSON stdout envelope is unchanged in
directory mode (no `"summary"` key added).

Rules (configure via `mds.json` `lint.rules`; severities differ per rule):

| Rule | Severity | Description |
|------|----------|-------------|
| `unused-variable` | warn | Frontmatter variable defined but never referenced in the body |
| `unused-import` | warn | `@import` that is never referenced (Tier B: report-only in practice — no `fix_removals` wired; a file with imports is never structural-standalone) |
| `unused-function` | warn | `@define` function that is never called (Tier B: auto-fixed only for standalone files) |
| `shadow-variable` | off/info | Inner-scope variable shadows an outer-scope variable (must be enabled via `mds.json`) |
| `empty-block` | warn | `@if`/`@elseif`/`@else`/`@for`/`@define`/`@message` body is empty or whitespace-only (auto-fixable) |
| `legacy-interpolation` | warn | Single-brace `{x}` syntax from MDS v0.x; migrates to `{{x}}` automatically (auto-fixable) |
| `redundant-else` | warn | `@else` body is structurally identical to the `@if`/`@elseif` then-body |
| `unreachable-branch` | **error** | Branch condition is always-true or always-false (auto-fixable) |
| `duplicate-import` | **error** | Same file imported more than once (auto-fixable) |
| `duplicate-export` | **error** | Same export name defined more than once (auto-fixable) |

Exit codes: `0` = clean, `1` = warnings only, `2` = errors or analysis failure, `3` = resource limit. With `--quiet`, output is suppressed but exit codes are unaffected. `info`-severity findings (e.g. `shadow-variable`) never raise the exit code regardless of `--quiet`.
JSON output shape: `{"files":[{"file":"…","diagnostics":[…]}],"truncated":false,"version":1}`.

## Bundler Integration

Import `.mds` templates directly in Vite, Rollup, Webpack, and Rspack projects:

```ts
import systemPrompt from './prompts/system.mds';
// systemPrompt is the compiled Markdown string
```

| Package | Bundler | Version |
|---------|---------|---------|
| [`@mdscript/vite-plugin`](packages/vite-plugin/README.md) | Vite | ^5 \|\| ^6 \|\| ^7 \|\| ^8 |
| [`@mdscript/rollup-plugin`](packages/rollup-plugin/README.md) | Rollup | ^3 \|\| ^4 |
| [`@mdscript/webpack-loader`](packages/webpack-loader/README.md) | Webpack | ^5 |
| [`@mdscript/rspack-loader`](packages/rspack-loader/README.md) | Rspack | ^1 |

All plugins require `@mdscript/mds` as a peer dependency and accept `{ vars?: Record<string, unknown> }` for runtime template variables. See each package README for configuration details.

TypeScript module declarations (`.mds` → `string | MdsMessage[]`) are provided by `@mdscript/bundler-utils/mds`. The kind is intrinsic to the template content: Markdown templates produce a `string`; templates with `@message` blocks produce an `MdsMessage[]`.

## Library Usage

### TypeScript / JavaScript

```ts
import { init, compile, compileFile, isMdsError } from '@mdscript/mds';

await init();

// Compile a string — result is a discriminated union based on template content
const result = compile('---\nname: World\n---\nHello {{name}}!\n');
if (result.kind === 'markdown') {
  console.log(result.output);      // string
} else {
  console.log(result.messages);    // { role: string; content: string }[]
}

// Override variables at runtime
const result2 = compile(source, { vars: { env: 'production' } });

// Compile a file (resolves @import directives)
const fileResult = await compileFile('./prompts/system.mds');
if (fileResult.kind === 'markdown') {
  console.log(fileResult.output, fileResult.dependencies);
} else {
  console.log(fileResult.messages, fileResult.dependencies);
}

// Source Map v3 (Markdown templates only)
const smResult = compile(source, { sourceMap: true });
if (smResult.kind === 'markdown' && smResult.sourceMap) {
  console.log(smResult.sourceMap.version);   // 3
  console.log(smResult.sourceMap.mappings);  // Base64-VLQ string
}
// ⚠ Privacy: sourcesContent embeds the full template source — only use in trusted environments.
const smWithContent = compile(source, { sourceMap: true, sourcesContent: true });

// Error handling
try {
  compile('Hello {{undefined_var}}!');
} catch (err) {
  if (isMdsError(err)) console.error(err.code, err.span);
}
```

`@mdscript/mds` uses a native addon on Node.js with an automatic WASM fallback, and runs in the browser via WASM.

### Rust

```rust
let output = mds::compile(Path::new("template.mds"), None)?;
let output = mds::compile_str("---\nname: World\n---\nHello {{name}}!\n")?;
let formatted = mds::format_str("Hello   {{name}}!\n")?;
```

## Examples

Runnable templates, a Node.js API demo, and Vite/Rollup/Webpack/Rspack integration apps
live in [`examples/`](examples/).

## Language Reference

See [spec.md](spec.md) for the full MDS v0.4.0 language specification.

## Contributing

Contributions are welcome! See [CONTRIBUTING.md](CONTRIBUTING.md) for the local
workflow and quality gates. By participating you agree to the
[Contributor Covenant 2.1](CODE_OF_CONDUCT.md).

## Security

Please report vulnerabilities privately via GitHub's
[private vulnerability reporting](https://github.com/dean0x/mdscript/security/advisories/new),
not public issues. See [SECURITY.md](SECURITY.md) for the security model, built-in
resource limits, and supported versions.

## License

MIT. See [LICENSE](LICENSE).
