# MDS - Markdown Script

[![CI](https://github.com/dean0x/mdscript/actions/workflows/ci.yml/badge.svg)](https://github.com/dean0x/mdscript/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/mds-cli.svg)](https://crates.io/crates/mds-cli)
[![npm](https://img.shields.io/npm/v/@mdscript/mds.svg)](https://www.npmjs.com/package/@mdscript/mds)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

LLM prompts grow into copy-pasted walls of text that drift across agents, models, and environments. MDS gives you variables, functions, imports, and conditionals so you can write prompts once and compose them everywhere, compiled to clean Markdown.

Built for AI engineers who manage prompt libraries across agents, models, and environments.

## Quick Start

**Install via npm** (Node or browser):

```bash
npm install @mdscript/mds
```

**Or install the CLI** (Rust):

```bash
cargo install mds-cli
```

**Create a prompt template** (`system.mds`):

```
---
model: claude-sonnet
tools: [search, calculator]
---

@import "./safety.mds" as guard
@import "./personas.mds" as persona

{persona.code_reviewer("TypeScript")}

{guard.safety_rules()}

## Available Tools

@for tool in tools:
- **{tool}**
@end

@if model == "claude-sonnet":
Use extended thinking for complex tasks.
@end
```

**Compile it**:

```bash
mds build system.mds          # writes system.md
mds build system.mds -o -     # stdout
```

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
mds init [FILENAME]             Create a starter MDS file

Global options:
  -q, --quiet                 Suppress status messages (applies to all commands)

Build/Watch options:
  -o, --output <PATH>         Output file, or "-" for stdout (build and single-file watch only;
                              rejected in directory watch mode — use --out-dir instead)
  --out-dir <DIR>             Output directory (build/single-file watch: <stem>.md or <stem>.json;
                              dir-mode watch: mirrors source subtree)
  --vars <FILE>               JSON file with variable overrides (reloaded each rebuild)
  --set KEY=VALUE             Set a single variable (repeatable)

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

**Directory mode** (`mds build <dir>` / `mds check <dir>`): every non-partial `.mds` file under the directory is compiled. `_`-prefixed files are partials — tracked as dependencies but never emitted to their own output. Output mirrors the source subtree (e.g. `src/a/b/foo.mds` → `dist/a/b/foo.md`). Symlinks are rejected. Errors are per-file and do not abort the run; a summary is printed and the exit code is non-zero if any file fails. Stale output files (compiled outputs with no corresponding source) are cleaned up automatically. The output extension is intrinsic: `.md` for Markdown templates, `.json` for templates with `@message` blocks.

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

> **Breaking change (next release):** Directory mode with `--out-dir` or `mds.json output_dir`
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
echo 'Hello   {name}!' | mds fmt -   # format from stdin, write to stdout; creates no file
```

What it normalizes:

- CRLF → LF, everywhere (including inside frontmatter and code fences)
- Two or more consecutive blank lines collapse to a single blank line; leading blank lines in the body are removed entirely
- Trailing whitespace on `@if`/`@for`/`@define`/… directive lines is stripped
- Exactly one final newline (empty or whitespace-only input formats to an empty file)

What it deliberately leaves untouched:

- Trailing whitespace on body-text content lines, including whitespace-only "blank" lines — two
  trailing spaces are a Markdown hard line break, and stripping them (anywhere in body text,
  including a stray blank line) can change rendered output
- Blank-line structure within frontmatter and code fences — CRLF is normalized there (the same
  as everywhere else in the file), but blank lines are not collapsed inside these regions
- The byte-for-byte content of `@message`/`@define` bodies — neither CRLF normalization nor
  blank-line collapsing is applied inside them, because these bodies bypass the compiler's own
  whitespace normalization and their content reaches compiled output verbatim

Directory mode formats every `.mds` file recursively, **including `_`-prefixed partials**,
continuing past per-file errors and printing a summary
(`N formatted, M unchanged, K failed`, or `N would reformat, K failed` under `--check`). A file
is only written (and its mtime touched) when its content actually changes. Status lines and
summaries go to stderr; `--diff` output and stdin filter-mode content go to stdout; `--quiet`
suppresses status but never errors. Reads a `fmt` section from `mds.json`
(`{"fmt": {"sort_frontmatter_keys": true}}`) for forward compatibility — the field doesn't drive
any formatting behavior yet; frontmatter key sorting is deferred to a future version.

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
const result = compile('---\nname: World\n---\nHello {name}!\n');
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

// Error handling
try {
  compile('Hello {undefined_var}!');
} catch (err) {
  if (isMdsError(err)) console.error(err.code, err.span);
}
```

`@mdscript/mds` uses a native addon on Node.js with an automatic WASM fallback, and runs in the browser via WASM.

### Rust

```rust
let output = mds::compile(Path::new("template.mds"), None)?;
let output = mds::compile_str("---\nname: World\n---\nHello {name}!\n")?;
let formatted = mds::format_str("Hello   {name}!\n")?;
```

## Examples

Runnable templates, a Node.js API demo, and Vite/Rollup/Webpack/Rspack integration apps
live in [`examples/`](examples/).

## Language Reference

See [spec.md](spec.md) for the full MDS v0.2.0 language specification.

## Contributing

Contributions are welcome! See [CONTRIBUTING.md](CONTRIBUTING.md) for the local
workflow and quality gates.

## Security

Please report vulnerabilities privately via GitHub's
[private vulnerability reporting](https://github.com/dean0x/mdscript/security/advisories/new),
not public issues. See [SECURITY.md](SECURITY.md) for the security model, built-in
resource limits, and supported versions.

## License

MIT. See [LICENSE](LICENSE).
