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
- **Messages**: `@message role: … @end` blocks compile to a JSON `[{role, content}]` array via `--format messages`
- **Security**: path traversal guards, symlink rejection, file size limits
- **Rich errors**: source-span diagnostics with line/column context

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
  --format <FORMAT>           markdown (default) or messages (JSON chat array)

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
| [`@mdscript/vite-plugin`](packages/vite-plugin/README.md) | Vite | ^5 \|\| ^6 \|\| ^7 \|\| ^8 |
| [`@mdscript/rollup-plugin`](packages/rollup-plugin/README.md) | Rollup | ^3 \|\| ^4 |
| [`@mdscript/webpack-loader`](packages/webpack-loader/README.md) | Webpack | ^5 |

All plugins require `@mdscript/mds` as a peer dependency and accept `{ vars?: Record<string, unknown> }` for runtime template variables. See each package README for configuration details.

TypeScript module declarations (`.mds` → `string`) are provided by `@mdscript/bundler-utils/mds`.

## Library Usage

### TypeScript / JavaScript

```ts
import { init, compile, compileFile, compileMessages, isMdsError } from '@mdscript/mds';

await init();

// Compile a string
const { output } = compile('---\nname: World\n---\nHello {name}!\n');

// Override variables at runtime
const result = compile(source, { vars: { env: 'production' } });

// Compile a file (resolves @import directives)
const { output, dependencies } = await compileFile('./prompts/system.mds');

// Compile @message blocks to a structured chat array
const { messages, warnings } = compileMessages(source);
// messages: [{ role: 'system', content: '...' }, { role: 'user', content: '...' }]

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

Runnable templates, a Node.js API demo, and Vite/Rollup/Webpack integration apps
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
