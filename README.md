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

Exit codes:
  0   Success (or clean Ctrl+C in watch mode)
  1   Template error (syntax, undefined variable, arity mismatch)
  2   I/O error (file not found, not an MDS file), or invalid CLI argument (clap parse error)
  3   Resource limit exceeded
```

**Directory mode** (`mds build <dir>` / `mds check <dir>`): every non-partial `.mds` file under the directory is compiled. `_`-prefixed files are partials — tracked as dependencies but never emitted to their own output. Output mirrors the source subtree (e.g. `src/a/b/foo.mds` → `dist/a/b/foo.md`). Symlinks are rejected. Errors are per-file and do not abort the run; a summary is printed and the exit code is non-zero if any file fails. Stale output files (compiled outputs with no corresponding source) are cleaned up automatically. The output extension is intrinsic: `.md` for Markdown templates, `.json` for templates with `@message` blocks.

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
