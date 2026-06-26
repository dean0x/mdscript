# MDS Examples

Runnable examples demonstrating the MDS language and its integrations. Compile
any `.mds` file with the CLI:

```bash
mds build examples/ai-agent/system-prompt.mds -o -
```

## Templates

| Directory | What it shows |
|-----------|---------------|
| [`ai-agent/`](ai-agent/) | System prompts, multi-turn conversations, tool instructions, and structured `@message` chat output ([`chat-messages.mds`](ai-agent/chat-messages.mds)) for LLM agents |
| [`api-docs/`](api-docs/) | Generating API documentation from endpoint and response-schema templates |
| [`blog-generator/`](blog-generator/) | A blog post template driven by frontmatter variables |
| [`prompt-library/`](prompt-library/) | A reusable prompt library using `@export`/`@import` (personas, formatting, guardrails) |
| [`inheritance/`](inheritance/) | Template inheritance with `@extends`/`@block` — one base agent skeleton specialized into a data analyst and a code reviewer |
| [`edge-cases/`](edge-cases/) | Numbered walkthrough of language features — loops, conditionals, imports, escaping, re-exports, runtime vars, built-in functions, default args, logical operators, expression directives, frontmatter imports |
| [`stress-test/`](stress-test/) | A large, deeply-composed template tree exercising the resolver and evaluator |

Some examples take runtime variables — pass the accompanying `vars.json`:

```bash
mds build examples/edge-cases/08_runtime_vars.mds --vars examples/edge-cases/vars.json -o -
```

## Output formats

**Output shape is intrinsic to the template — decided by content, not a flag.**

A template containing any `@message` block compiles to a JSON messages array;
all other templates compile to Markdown. The output extension reflects the kind:
`.json` for messages templates, `.md` for Markdown templates.

```bash
# Markdown template → .md next to source
mds build examples/ai-agent/system-prompt.mds

# Messages template → .json next to source
mds build examples/ai-agent/chat-messages.mds

# Compile to stdout (kind-appropriate bytes)
mds build examples/ai-agent/chat-messages.mds -o -

# Compile a whole directory (intrinsic extension per file)
mds build examples/ --out-dir dist/

# Check a whole directory without writing output
mds check examples/
```

`@message` detection is **static**: a `@message` block anywhere in the template
(even inside `@if false:`) makes it a messages template. **Mixed content** —
loose top-level prose or interpolations alongside `@message` blocks — is a hard
compile error (`mds::mixed_content`).

A messages template that produces zero messages emits `[]`.

From JavaScript, `compile`/`compileFile` return a **discriminated union**
branched on `kind`:

```js
import { compile, compileFile } from '@mdscript/mds';

// Markdown template
const r1 = compile(markdownSource);
if (r1.kind === 'markdown') {
  console.log(r1.output); // string
}

// Messages template
const r2 = await compileFile('chat-messages.mds');
if (r2.kind === 'messages') {
  console.log(r2.messages); // Array<{ role: string; content: string }>
}
```

There is no `--format` flag and no `compileMessages`/`compileMessagesFile`
function — the kind is determined by the template source.

## Node.js API

[`node-api-test.mjs`](node-api-test.mjs) demonstrates compiling templates from
JavaScript via `@mdscript/mds`, including `kind` discrimination between Markdown
and messages results.

## Bundler integrations

Each app imports `.mds` files directly through the bundler plugin and resolves
the MDS packages from this monorepo (`file:` dependencies):

| App | Plugin |
|-----|--------|
| [`vite-app/`](vite-app/) | `@mdscript/vite-plugin` |
| [`rollup-app/`](rollup-app/) | `@mdscript/rollup-plugin` |
| [`webpack-app/`](webpack-app/) | `@mdscript/webpack-loader` |
| [`rspack-app/`](rspack-app/) | `@mdscript/rspack-loader` |

To run one (after building the workspace packages from the repo root with
`npm install && npm run build --workspaces`):

```bash
cd examples/vite-app
npm install
npm run build
```
