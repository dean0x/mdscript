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

By default `mds build` emits Markdown text. A template that declares `@message`
blocks can additionally compile to a JSON array of chat messages with
`--format messages` — ready to pass straight to a chat LLM API's `messages`
parameter:

```bash
# Text mode (default): @message bodies render inline (backward compatible)
mds build examples/ai-agent/chat-messages.mds -o -

# Messages mode: JSON array of { "role", "content" } objects
mds build examples/ai-agent/chat-messages.mds --format messages -o -
```

From JavaScript, the same two modes are `compile`/`compileFile` (text) and
`compileMessages`/`compileMessagesFile` (messages array), exported from
`@mdscript/mds`.

## Node.js API

[`node-api-test.mjs`](node-api-test.mjs) demonstrates compiling templates from
JavaScript via `@mdscript/mds`.

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
