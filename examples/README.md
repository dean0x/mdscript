# MDS Examples

Runnable examples demonstrating the MDS language and its integrations. Compile
any `.mds` file with the CLI:

```bash
mds build examples/ai-agent/system-prompt.mds -o -
```

## New in v0.4.0

Four capabilities shipped with v0.4.0 — each has a dedicated example:

```bash
# Safety-gated formatter — rewrites directive lines only, never body text
mds fmt --check examples/

# Static analysis — 10 rules, human and JSON output, --fix --diff preview
mds lint examples/linting/

# Source Map v3 — sidecar .map file, --inline data-URI, or --embed-sources
mds build examples/source-maps/annotated-prompt.mds --source-map -o /tmp/out.md

# String variables without type coercion (e.g. preserve leading zeros)
mds build template.mds --set-string zip_code=02134

# Decode a Source Map v3 and trace output lines back to their source
node examples/source-maps/consume-map.mjs

# Python bindings tour — compile, source maps, MdsError handling, lint
source .venv/bin/activate && python examples/python/demo.py

# Per-rule severity overrides via mds.json in config-demo/ (exits 2 by design)
mds lint examples/linting/config-demo/loop-shadow.mds
```

## Templates

| Directory | What it shows |
|-----------|---------------|
| [`ai-agent/`](ai-agent/) | System prompts, multi-turn conversations, tool instructions, and structured `@message` chat output ([`chat-messages.mds`](ai-agent/chat-messages.mds)) for LLM agents |
| [`api-docs/`](api-docs/) | Generating API documentation from endpoint and response-schema templates |
| [`blog-generator/`](blog-generator/) | A blog post template driven by frontmatter variables |
| [`prompt-library/`](prompt-library/) | A reusable prompt library using `@export`/`@import` (personas, formatting, guardrails) |
| [`inheritance/`](inheritance/) | Template inheritance with `@extends`/`@block` — one base agent skeleton specialized into a data analyst and a code reviewer |
| [`edge-cases/`](edge-cases/) | Numbered walkthrough of language features — loops, conditionals, imports, escaping, re-exports, runtime vars, built-in functions, default args, logical operators, expression directives, frontmatter imports; v0.4.0 adds interior blank-line preservation, typed comparisons, and `@extends` frontmatter merge; `30_set_string_cli.mds` demos `--set` (coerces to number, may raise `mds::type_mismatch`) vs `--set-string` (keeps string bytes byte-for-byte), and both flags on the same key as a hard error; `31_wide_and_nested_fences.mds` shows a 4-backtick fence wrapping a 3-backtick block verbatim, and mismatched ``` / ~~~ markers that never cross-close |
| [`stress-test/`](stress-test/) | A large, deeply-composed template tree exercising the resolver and evaluator |
| [`linting/`](linting/) | A deliberately-messy template that trips four lint rules; shows `mds lint` human and JSON output, `--fix` tiers (A auto-applies, B standalone-only, C report-only), `--diff` preview, exit-code semantics, and per-rule severity overrides via `mds.json` (`config-demo/` — enables `shadow-variable`, promotes `unused-variable` to error, silences `redundant-else`) |
| [`source-maps/`](source-maps/) | Source Map v3 generation via `mds build --source-map` — sidecar map, `--inline` data-URI embed, and `--embed-sources` self-contained variant |
| [`formatting/`](formatting/) | Auto-formatter demo (`mds fmt`) — write, `--check`, `--diff`, and stdin filter modes; what fmt normalizes (directive trailing whitespace, line endings, final newline) vs. preserves byte-for-byte (body text, `@message`/`@define` bodies); the safety gate that refuses any rewrite changing compiled output; exit codes |
| [`python/`](python/) | Native Python bindings (`markdown_script`, built with PyO3) — compile strings and files with `markdown_script.compile`/`compile_file`, generate Source Map v3 with `source_map=True`, handle `MdsError` (`.code`, `.help`, `.span`), and lint with `markdown_script.lint`; requires a virtualenv + `maturin develop` (`source .venv/bin/activate`) |

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

Note: `examples/stress-test/errors/` contains five intentionally-failing fixtures (`bad-arity`, `bad-circular-a/b`, `bad-type`, `bad-undefined`), so `mds build examples/ --out-dir dist/` and `mds check examples/` exit non-zero by design. Likewise, `mds lint examples/` exits 2 by design because `examples/linting/demo.mds` and `examples/linting/config-demo/loop-shadow.mds` both deliberately trigger error-level findings.

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
