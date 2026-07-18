# @mdscript/mds

JavaScript/TypeScript bindings for the [MDS](../../README.md) compiler.

## Installation

```sh
npm install @mdscript/mds
```

> **Note:** This package is pre-release and not yet published to npm.

## Node.js usage (zero-config)

Node.js auto-selects the native addon and falls back to WASM if unavailable.
No initialization required.

```ts
import { compile, check, compileFile, checkFile, getBackend, isMdsError } from '@mdscript/mds';

// Compile MDS source to Markdown
const result = compile('Hello {name}', { vars: { name: 'world' } });
console.log(result.output);       // "Hello world"
console.log(result.warnings);     // string[]
console.log(result.dependencies); // string[] of imported file paths

// Validate without rendering
const checked = check('Hello {name}', { vars: { name: 'world' } });

// File-based operations (resolves @import directives)
const fileResult = await compileFile('./my-template.mds');
await checkFile('./my-template.mds');

// Which backend is active?
console.log(getBackend()); // 'native' | 'wasm'
```

## Browser usage

The browser entry requires an explicit `init()` call before any compile/check
operations. `init()` is idempotent and safe to call multiple times.

```ts
import { init, compile, check, isMdsError } from '@mdscript/mds';

await init();
// or with a custom WASM URL:
await init({ wasmUrl: '/assets/mds_bg.wasm' });

const result = compile('# {title}', { vars: { title: 'Hello' } });
```

> `compileFile` and `checkFile` are not available in browser environments.

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

```ts
// CompileOptions — accepted by compile() and compileFile()
interface CompileOptions {
  vars?: Record<string, unknown>;
  sourceMap?: boolean;      // generate Source Map v3; result gains a `sourceMap` field
  sourcesContent?: boolean; // embed source text in map (requires sourceMap: true)
                            // ⚠ Privacy: embeds the full template source
}

// CheckOptions — accepted by check() and checkFile() only
// Source-map options are NOT accepted; passing them throws mds::invalid_options
interface CheckOptions {
  vars?: Record<string, unknown>;
}

// LintOptions — accepted by lint() (string-source)
interface LintOptions {
  vars?: Record<string, unknown>;
  rules?: Record<string, 'off' | 'info' | 'warn' | 'error'>;
  basePath?: string; // base directory for @import resolution; required when the source
                     // contains @import or @extends. Ignored by the WASM backend.
}

// LintFileOptions — accepted by lintFile() and lintVirtual()
// basePath is NOT accepted: lintFile derives the base directory from the file path;
// lintVirtual resolves imports against the caller-supplied module map, not the filesystem.
interface LintFileOptions {
  vars?: Record<string, unknown>;
  rules?: Record<string, 'off' | 'info' | 'warn' | 'error'>;
}

// InitOptions
interface InitOptions {
  wasmUrl?: string | URL | Response | BufferSource;
}
```

**Strict unknown-option rejection:** passing any key not listed above throws
`Error { code: 'mds::invalid_options' }` immediately, before calling the backend.
This applies to `compile`, `compileFile`, `check`, `checkFile`, `lint`, `lintFile`,
and `lintVirtual`.

**Source maps:** for string-source compiles (`compile`) `sources[0]` in the generated
map is `"input.mds"`. For stdin builds via the CLI it is `"<stdin>"`.

**Lint `rules` map:** unknown rule names are silently accepted (a typo has no effect);
unknown severity values throw `mds::invalid_options`.

**Lint result shape:**
```ts
{ version: 1, files: [{ file: string, diagnostics: LintDiagnostic[] }], truncated: boolean }
// LintDiagnostic: { rule, severity, message, help?, fixable, span? }
```
