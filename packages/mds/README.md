# @mdscript/mds

JavaScript/TypeScript bindings for the [MDS](../../README.md) compiler.

## Installation

```sh
npm install @mdscript/mds
```

## Node.js usage (zero-config)

Node.js auto-selects the native addon and falls back to WASM if unavailable.
No initialization required.

```ts
import { compile, check, compileFile, checkFile, getBackend, isMdsError } from '@mdscript/mds';

// Compile MDS source to Markdown
const result = compile('Hello {{name}}', { vars: { name: 'world' } });
console.log(result.output);       // "Hello world"
console.log(result.warnings);     // string[]
console.log(result.dependencies); // string[] of imported file paths

// Validate without rendering
const checked = check('Hello {{name}}', { vars: { name: 'world' } });

// File-based operations (resolves @import directives)
const fileResult = await compileFile('./my-template.mds');
await checkFile('./my-template.mds');

// Which backend is active?
console.log(getBackend()); // 'native' | 'wasm'
```

## Browser usage

The browser entry requires an explicit `init()` call before any compile/check/lint
operations. `init()` is idempotent and safe to call multiple times.

```ts
import { init, compile, check, lint, lintVirtual, isMdsError } from '@mdscript/mds';

await init();
// or with a custom WASM URL:
await init({ wasmUrl: '/assets/mds_bg.wasm' });

const result = compile('# {{title}}', { vars: { title: 'Hello' } });
const lintResult = lint('# {{title}}', { vars: { title: 'Hello' } });
const virtualResult = lintVirtual({ 'entry.mds': '# {{title}}' }, 'entry.mds');
```

> `compileFile`, `checkFile`, and `lintFile` are not available in browser environments.

## Backend selection (`MDS_BACKEND`)

Set the `MDS_BACKEND` environment variable in Node.js to force a specific backend:

| Value | Behavior |
|-------|----------|
| *(unset)* | Native addon, WASM fallback |
| `native` | Native only, throws if addon unavailable |
| `wasm` | WASM only |

```sh
MDS_BACKEND=wasm node my-script.js
```

## Error handling

Use `isMdsError` to distinguish MDS compiler errors from other exceptions:

```ts
import { compile, isMdsError } from '@mdscript/mds';

try {
  compile(source);
} catch (err) {
  if (isMdsError(err)) {
    console.error(err.code);    // e.g. "mds::undefined_var"
    console.error(err.message);
    console.error(err.help);    // optional guidance string
    console.error(err.span);    // optional { offset, length, line, column }
  } else {
    throw err;
  }
}
```

## API

| Function | Description |
|----------|-------------|
| `compile(source, options?)` | Compile MDS source string to Markdown or messages |
| `check(source, options?)` | Validate MDS source without rendering |
| `compileFile(path, options?)` | Compile an MDS file, resolving imports |
| `checkFile(path, options?)` | Validate an MDS file, resolving imports |
| `lint(source, options?)` | Static analysis on a source string |
| `lintFile(path, options?)` | Static analysis on a file |
| `lintVirtual(modules, entry, options?)` | Static analysis on an in-memory module map |
| `getBackend()` | Returns the active backend: `'native'` or `'wasm'` |
| `init(options?)` | Initialize the WASM backend (browser/explicit WASM only) |
| `isMdsError(err)` | Type guard for MDS compiler errors (requires `code` starting with `"mds::"`) |

### Options

#### Option matrix

Each cell names the accepted option type. `basePath` is a string-surface option only;
file-path methods derive the base directory from the file argument.

|           | String source                | File path          |
|-----------|------------------------------|--------------------|
| `compile` | `CompileOptions`             | `FileOptions`      |
| `check`   | `CheckOptions`               | `CheckFileOptions` |
| `lint`    | `LintOptions`                | `LintFileOptions`  |

#### Option types

```ts
// CheckOptions — accepted by check() (string source)
// basePath: defaults to process cwd when omitted. Caution: omitting it resolves
// imports against cwd, which may be the wrong directory. Provide an explicit
// path when the source contains @import or @extends.
// WASM backend: basePath throws mds::invalid_options (no filesystem access);
// set MDS_BACKEND=native to use the native backend with import resolution.
// {basePath: undefined} is treated as absent on both backends.
interface CheckOptions {
  vars?: Record<string, unknown>;
  basePath?: string;
}

// CompileOptions — accepted by compile() (string source)
// Extends CheckOptions: inherits vars and basePath.
// basePath behaves the same as for CheckOptions (see above).
interface CompileOptions extends CheckOptions {
  sourceMap?: boolean;      // generate Source Map v3; result gains a `sourceMap` field
  sourcesContent?: boolean; // embed source text in map (requires sourceMap: true)
                            // ⚠ Privacy: embeds the full template source
}

// FileOptions — accepted by compileFile()
// basePath is NOT accepted: the base directory is derived from the file path.
// basePath?: never blocks assigning a CompileOptions variable to this type (TS2322).
interface FileOptions {
  vars?: Record<string, unknown>;
  sourceMap?: boolean;
  sourcesContent?: boolean;
  basePath?: never; // not accepted; present to produce a compile-time error when a string-surface variable is passed
}

// CheckFileOptions — accepted by checkFile()
// basePath is NOT accepted (base directory from file path).
// basePath?: never blocks assigning a CheckOptions variable to this type (TS2322).
// Source-map options are NOT accepted; passing them throws mds::invalid_options.
interface CheckFileOptions {
  vars?: Record<string, unknown>;
  basePath?: never; // not accepted; present to produce a compile-time error when a string-surface variable is passed
}

// LintOptions — accepted by lint() (string-source)
// basePath: defaults to process cwd when omitted. Caution: omitting it resolves
// imports against cwd, which may be the wrong directory. Provide an explicit
// path when the source contains @import or @extends.
// WASM backend: basePath throws mds::invalid_options — rejects instead of
// silently ignoring so misconfigured callers see an actionable error.
// Set MDS_BACKEND=native to use the native backend, or use lintVirtual with
// pre-resolved modules.
interface LintOptions {
  vars?: Record<string, unknown>;
  rules?: Record<string, 'off' | 'info' | 'warn' | 'error'>;
  basePath?: string;
}

// LintFileOptions — accepted by lintFile() and lintVirtual()
// basePath is NOT accepted: lintFile derives the base directory from the file path;
// lintVirtual resolves imports against the caller-supplied module map, not the filesystem.
// basePath?: never blocks assigning a LintOptions variable to this type (TS2322).
interface LintFileOptions {
  vars?: Record<string, unknown>;
  rules?: Record<string, 'off' | 'info' | 'warn' | 'error'>;
  basePath?: never; // not accepted; present to produce a compile-time error when a string-surface variable is passed
}

// InitOptions
interface InitOptions {
  wasmUrl?: string | URL | Response | BufferSource;
}
```

**Unknown-option rejection:** passing an unrecognised key to a public method throws
`Error { code: 'mds::invalid_options' }` before calling the backend; the error names
the offending key(s) and lists the accepted keys. Exception: passing `basePath` to a
file-path method (`compileFile` or `checkFile`) **throws synchronously** with a
purpose-built message (same channel as unknown-key rejection); the message does not
include an accepted-keys list. `.catch()` on the returned promise does not receive
this error — use `try/catch` around the call.

**Source maps:** for string-source compiles (`compile`) `sources[0]` in the generated
map is `"input.mds"`. For stdin builds via the CLI it is `"<stdin>"`.

**Lint `rules` map:** unknown rule names emit a warning and lint continues — the unknown
name has no effect but `result.lint_warnings` (a `string[]` field, absent when empty)
is populated so callers can surface the issue. Unknown severity values throw
`mds::invalid_options`. On the CLI surface the warning goes to stderr; in binding
surfaces it appears in `lint_warnings`.

**Lint result shape:**
```ts
{ version: 1, files: [{ file: string, diagnostics: LintDiagnostic[] }], truncated: boolean, lint_warnings?: string[] }
// LintDiagnostic: { rule, severity, message, help?: string | null, fixable, fix_edits?: ... | null, span?: LintSpan | null }
// help, span, and fix_edits are always-present keys in the JSON wire format; their value is null when absent.
```
