# @mdscript/mds

JavaScript/TypeScript bindings for the [MDS](../../README.md) compiler.

## Installation

```sh
npm install @mdscript/mds
```

## Node.js usage

Node.js selects the native addon. If the addon is not available, it uses WASM.
Call `await init()` once before you call a compile, check, lint or backend
function. If you call one of these functions before `init()` finishes, it
throws an error. You can call `init()` more than one time. Calls after the
first do nothing.

```ts
import { init, compile, check, compileFile, checkFile, getBackend, isMdsError } from '@mdscript/mds';

await init();

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

WASM file APIs (`compileFile`, `checkFile`, and `lintFile`) differ from native in these cases:

- WASM can report a missing import outside the project as a path outside the project. It checks the project boundary first, so it does not show which files exist outside the project.
  This also applies to a local data file: if a data file outside the project root does not exist, WASM gives the project root refusal, and native gives `mds::file_not_found`.
- WASM refuses an import if `..` in the path as written goes above the project root. This applies even if a link leads to a file inside the project. WASM checks the path text before it reads the file system.
- For a project at the filesystem root, WASM also refuses `/..` as a path above the project root. A native match needs new public API, a spec change, and more WASM bytes.
- WASM reports `mds::import` if it must use one path name for two different files.
  For example, on POSIX, if `link` points to `deep/sub`, `./link/../z.mds` refers to `deep/z.mds`.
  That import works on its own. It conflicts with an import of `./z.mds` from the same directory if that path names a different file.
  On Windows, WASM also accepts paths such as `./link\..\z.mds`. The pair `./z.mds` and `./a\b/../z.mds` can cause the same conflict.
- WASM limits entry and import paths below the project root to 256 segments. This also applies to the resolved file path if a directory link leads to a deeper file. The limit keeps an explicit bound on path resolution.
- WASM uses an errno name, such as `EACCES`, in `cannot read` messages for Node file errors.
  Node's file error message can include an absolute path. Node's system error text has no path, but differs from the Rust text.
  For example, Node gives `permission denied` and Rust gives `Permission denied (os error 13)`.
  For a POSIX name with a backslash, native displays `/` and WASM keeps `\`.
- A later file error can precede an earlier compile error or cycle. WASM reads the modules before it evaluates them. To match native error order, the engine would need to evaluate each module before the scanner reads the next. This difference changes which error WASM reports, not the compiled output.
- WASM limits total module bytes. It also keeps a table for import paths that differ from their resolved file paths below the project root.
  The scanner and the WASM engine limit the table to 65,536 entries and 10 MiB of path text in UTF-8. Native does not use this table.
- WASM refuses a non-UTF-8 directory before it checks the file. Native checks the file first. To match native order, each path check would need both string and byte forms.
- WASM reads each local data file that a module lists before the compile starts. This includes the file of an entry that a runtime variable or a child entry replaces, which native does not read. Such a file counts toward the 256-file and 32 MiB data limits.
- On POSIX, WASM refuses a local data file whose name starts with `\` or has `:` as its second byte, for example `./\x.json` or `./a:b.json`. Native reads these files. The WASM engine checks a data key with no project root.

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
| `init(options?)` | Load the backend. Await it before you call a compile, check, lint or backend function (Node.js and browser) |
| `isMdsError(err)` | Type guard for MDS compiler errors (requires `code` starting with `"mds::"`) |

### Options

#### Option matrix

Each cell names the accepted option type. `basePath` is a string-surface option only;
file-path methods derive the base directory from the file argument.

|           | String source    | File path            |
|-----------|------------------|----------------------|
| `compile` | `CompileOptions` | `CompileFileOptions` |
| `check`   | `CheckOptions`   | `CheckFileOptions`   |
| `lint`    | `LintOptions`    | `LintFileOptions`    |

#### Option types

Optional input fields accept explicit `undefined`. If your TypeScript config
sets `exactOptionalPropertyTypes`, omit `basePath` from file and virtual options.
At runtime, these methods treat `basePath: undefined` as absent.

```ts
// CheckOptions — accepted by check() (string source)
// basePath: defaults to process cwd when omitted. Caution: omitting it resolves
// imports against cwd, which may be the wrong directory. Provide an explicit
// path when the source contains @import or @extends.
// WASM backend: basePath throws mds::invalid_options (no filesystem access);
// set MDS_BACKEND=native to use the native backend with import resolution.
// {basePath: undefined} is treated as absent on both backends.
// {basePath: ''} (empty string) is rejected — omit the key rather than passing an empty string.
interface CheckOptions {
  vars?: Record<string, unknown> | undefined;
  basePath?: string | undefined;
}

// CompileOptions — accepted by compile() (string source)
// Extends CheckOptions: inherits vars and basePath.
// basePath behaves the same as for CheckOptions (see above).
interface CompileOptions extends CheckOptions {
  // Generate Source Map v3 and add `sourceMap` to the result.
  sourceMap?: boolean | undefined;
  // Privacy: This option puts the full template source in the map.
  // Requires sourceMap: true.
  sourcesContent?: boolean | undefined;
}

// CompileFileOptions — accepted by compileFile()
// basePath is NOT accepted: the base directory is derived from the file path.
// basePath?: never blocks assigning a CompileOptions variable to this type.
interface CompileFileOptions {
  vars?: Record<string, unknown> | undefined;
  sourceMap?: boolean | undefined;
  sourcesContent?: boolean | undefined;
  basePath?: never; // not accepted; present to produce a compile-time error when a string-surface variable is passed
}

// CheckFileOptions — accepted by checkFile()
// basePath is NOT accepted (base directory from file path).
// basePath?: never blocks assigning a CheckOptions variable to this type.
// Source-map options are NOT accepted; passing them throws mds::invalid_options.
interface CheckFileOptions {
  vars?: Record<string, unknown> | undefined;
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
  vars?: Record<string, unknown> | undefined;
  rules?: Record<string, 'off' | 'info' | 'warn' | 'error'> | undefined;
  basePath?: string | undefined;
}

// LintFileOptions — accepted by lintFile() and lintVirtual()
// basePath is NOT accepted: lintFile derives the base directory from the file path;
// lintVirtual resolves imports against the caller-supplied module map, not the filesystem.
// basePath?: never blocks assigning a LintOptions variable to this type.
interface LintFileOptions {
  vars?: Record<string, unknown> | undefined;
  rules?: Record<string, 'off' | 'info' | 'warn' | 'error'> | undefined;
  basePath?: never; // not accepted; present to produce a compile-time error when a string-surface variable is passed
}

// InitOptions
interface InitOptions {
  wasmUrl?: string | URL | Response | BufferSource | undefined;
}
```

**Unknown-option rejection:** passing an unrecognised key to a public method throws
`Error { code: 'mds::invalid_options' }` before calling the backend; the error names
the offending key(s) and lists the accepted keys. **All option-validation errors throw
synchronously** before any I/O begins — this includes unknown-key rejection from every
method and the `basePath`-rejection guard on `compileFile`/`checkFile`. For the
Promise-returning file-path methods (`compileFile`, `checkFile`, `lintFile`), `.catch()`
on the returned promise does **not** capture option-validation errors — use `try/catch`
around the call.

**Source maps:** for string-source compiles (`compile`) the source string is named
`"input.mds"` in the generated map's `sources` (`"<stdin>"` for stdin builds via the
CLI). It is `sources[0]` unless the string `@extends` a base: the map of an `@extends`
chain lists the chain's root base first, as a compile of the same file does.

**Lint `rules` map:** unknown rule names emit a warning and lint continues — the unknown
name has no effect but `result.lint_warnings` (a `string[]` field, absent when empty)
is populated so callers can surface the issue. Unknown severity values throw
`mds::invalid_options`. On the CLI surface the warning goes to stderr; in binding
surfaces it appears in `lint_warnings`.

**Lint result shape:**
```ts
{ version: 1, files: [{ file: string, diagnostics: LintDiagnostic[] }], truncated: boolean, lint_warnings?: string[] }
// LintDiagnostic: { rule, severity, message, help: string | null, fixable, fix_edits: Array<{start, end, new_text}> | null, span: LintSpan | null }
// help, span, and fix_edits are REQUIRED keys — always present in the JSON wire format, `null` (never `undefined`) when absent.
```

**`files[].file` key:** `lint()` sets this to `"input.mds"` (string-source); `lintFile()` sets it to the file's path; `lintVirtual()` sets it to the caller-supplied entry key. The CLI additionally relabels stdin input as `"<stdin>"` — this asymmetry does not apply to the binding surfaces.
