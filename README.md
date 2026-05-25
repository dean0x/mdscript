# MDS — Markdown Script

MDS is a template language for composable LLM prompt engineering. Write prompts with variables, loops, conditionals, functions, and imports — then compile them to clean Markdown.

## Quick Start

**Install** (from source):

```bash
cargo install --path crates/mds-cli
```

**Create your first template** (`hello.mds`):

```
---
name: World
items: [one, two, three]
---

Hello {name}!

Your items:
@for item in items:
- {item}
@end
```

**Compile it**:

```bash
mds build hello.mds          # writes hello.md
mds build hello.mds -o -     # stdout
```

## Features

- **Variables** — YAML frontmatter or runtime `--set KEY=VALUE` flags
- **Conditionals** — `@if`/`@else`/`@end` blocks
- **Loops** — `@for item in list:` iteration over arrays
- **Functions** — `@define` reusable blocks with parameters
- **Imports** — `@import` for modular prompt libraries (alias, merge, selective)
- **Exports** — `@export` for building prompt component libraries
- **Security** — path traversal guards, symlink rejection, file size limits
- **Rich errors** — source-span diagnostics with line/column context

## CLI Reference

```
mds build [FILE] [OPTIONS]    Compile an MDS template to Markdown
mds check [FILE] [OPTIONS]    Validate without rendering
mds init [FILENAME]           Create a starter MDS file

Global options:
  -q, --quiet                 Suppress status messages (applies to all commands)

Build options:
  -o, --output <PATH>         Output file, or "-" for stdout
  --out-dir <DIR>             Output directory (creates <stem>.md inside it)
  --vars <FILE>               JSON file with variable overrides
  --set KEY=VALUE             Set a single variable (repeatable)

Exit codes:
  0   Success
  1   Template error (syntax, undefined variable, arity mismatch)
  2   I/O error (file not found, not an MDS file)
  3   Resource limit exceeded
```

## Bundler Integration

Import `.mds` templates directly in Vite, Rollup, and Webpack projects:

```ts
import systemPrompt from './prompts/system.mds';
// systemPrompt is the compiled Markdown string
```

| Package | Bundler | Version |
|---------|---------|---------|
| [`@mds/vite-plugin`](packages/vite-plugin/README.md) | Vite | ^5 \|\| ^6 |
| [`@mds/rollup-plugin`](packages/rollup-plugin/README.md) | Rollup | ^3 \|\| ^4 |
| [`@mds/webpack-loader`](packages/webpack-loader/README.md) | Webpack | ^5 |

All plugins require `@mds/mds` as a peer dependency and accept `{ vars?: Record<string, unknown> }` for runtime template variables. See each package README for configuration details.

TypeScript module declarations (`.mds` → `string`) are provided by `@mds/bundler-utils/mds`.

## Library Usage

```rust
// Compile from string
let output = mds::compile_str("---\nname: World\n---\nHello {name}!\n")?;

// Compile from file
let output = mds::compile(Path::new("template.mds"), None)?;

// With runtime variables
let vars = mds::load_vars_file(Path::new("vars.json"))?;
let output = mds::compile(Path::new("template.mds"), Some(vars))?;

// Validation only (no rendering)
mds::check(Path::new("template.mds"), None)?;
```

## Language Reference

See [spec.md](spec.md) for the full MDS v0.1 language specification.

## License

MIT — see [LICENSE](LICENSE).
