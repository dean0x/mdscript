# @mds/mds

JavaScript/TypeScript bindings for the [MDS](../../README.md) compiler.

## Installation

```sh
npm install @mds/mds
```

> **Note:** This package is pre-release and not yet published to npm.

## Node.js usage (zero-config)

Node.js auto-selects the native addon and falls back to WASM if unavailable.
No initialization required.

```ts
import { compile, check, compileFile, checkFile, getBackend, isMdsError } from '@mds/mds';

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
operations. `init()` is idempotent — safe to call multiple times.

```ts
import { init, compile, check, isMdsError } from '@mds/mds';

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
| `native` | Native only — throws if addon unavailable |
| `wasm` | WASM only |

```sh
MDS_BACKEND=wasm node my-script.js
```

## Error handling

Use `isMdsError` to distinguish MDS compiler errors from other exceptions:

```ts
import { compile, isMdsError } from '@mds/mds';

try {
  compile(source);
} catch (err) {
  if (isMdsError(err)) {
    console.error(err.code);    // e.g. "mds::undefined_variable"
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
| `compile(source, options?)` | Compile MDS source string to Markdown |
| `check(source, options?)` | Validate MDS source without rendering |
| `compileFile(path, options?)` | Compile an MDS file, resolving imports |
| `checkFile(path, options?)` | Validate an MDS file, resolving imports |
| `getBackend()` | Returns the active backend: `'native'` or `'wasm'` |
| `init(options?)` | Initialize the WASM backend (browser/explicit WASM only) |
| `isMdsError(err)` | Type guard for MDS compiler errors (requires `code` starting with `"mds::"`) |

### Options

```ts
// CompileOptions / FileOptions
{ vars?: Record<string, unknown> }

// InitOptions
{ wasmUrl?: string | URL | Response | BufferSource }
```
