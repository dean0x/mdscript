# MDS Language Specification (v0.4)

## 1. Overview

MDS (Markdown Script) is a domain-specific language for composing, reusing, and compiling LLM prompts.

- **Input**: `.mds` files (Markdown-native syntax with lightweight directives)
- **Output**: Compiled Markdown (`.md`) or a JSON messages array (`.json`) — determined intrinsically by template content (see §4.10)
- **Compiler**: Rust
- **Audience**: Prompt engineers, AI developers

---

## 2. Design Principles

1. Looks like Markdown, not code
2. Minimal new syntax: leverage existing conventions (YAML frontmatter, `@` directives)
3. Composable: imports, functions, modules
4. Deterministic: same input always produces same output
5. Fail fast: clear errors with file:line:col, no partial output

---

## 3. File Format

- Extension: `.mds`
- Encoding: UTF-8
- Structure: optional frontmatter → directives/content (order-independent for directives)

---

## 4. Syntax

### 4.1 Variables (YAML Frontmatter)

```mds
---
name: Alice
items: [apple, banana]
premium: true
count: 3
config:
  debug: false
  greeting: Hello
---
```

**Rules:**

- Standard YAML between `---` fences at file start
- Types supported: string, number, boolean, array, object (nested YAML mappings)
- Runtime vars (CLI `--vars vars.json`) override frontmatter values
- Object values support dot-notation field access: `{{config.key}}`, `{{a.b.c}}`
- Objects cannot be interpolated directly; access a specific field instead

**Resource limits:**

| Limit | Value |
|-------|-------|
| `MAX_FRONTMATTER_SIZE` | 1 MiB per frontmatter block |
| `MAX_FRONTMATTER_NODES` | 200,000 YAML nodes per block, counted during parsing so alias expansion stops before the tree is built |
| `MAX_FRONTMATTER_FLOW_DEPTH` | 1024 levels of flow-collection (`[`/`{`) nesting, checked before the parser scans (bounds libyaml's O(depth²) flow scan) |
| `MAX_FILE_SIZE` | 10 MiB per source, including strings passed to the string APIs |

Exceeding one of these returns `mds::resource_limit` (exit 3). YAML the parser
itself refuses — syntax errors, duplicate keys, nesting deeper than 128 levels,
its alias-repetition limit — returns `mds::yaml`, as does value nesting deeper
than 64 levels. These bounds are pinned by the `parse_frontmatter_yaml` tests in
`crates/mds-core/src/resolver/frontmatter.rs` and by the
`yaml_parse_sites_are_funnelled` test.

---

### 4.2 Interpolation

```mds
Hello {{name}}!
```

**Rules:**

- Double braces: `{{identifier}}` or dot path `{{obj.field}}`
- Valid interpolation: a valid identifier (`[a-zA-Z_][a-zA-Z0-9_]*`), dot path (`{{config.key}}`, `{{a.b.c}}`), or function call
- A single `{` or `}` is always literal text — no escaping needed for lone braces
- Escaping: `\{{` produces a literal `{{` in output (no `\}}` escape — a lone `}}` is just two `}` characters)
- Non-recursive: interpolated values are never re-scanned — the output of `{{x}}` is always plain text, never further interpreted as MDS
- Escape adjacency: `\` immediately before `{{` is claimed by the `\{{` escape — `\{{x}}` emits literal `{{` followed by the text `x}}`. To get a backslash followed by an interpolated value, write `\ {{x}}` (backslash, space, then `{{x}}`).
- Inside fenced code blocks (triple backtick or tilde; also indented or blockquoted fences): no interpolation occurs (raw passthrough)
- Undefined variable → compilation error (not silent empty string)

**Migration from single-brace syntax:** run `mds lint --fix` (applies the `legacy-interpolation` rule to auto-convert `{x}` → `{{x}}`), then `mds fmt` to normalize formatting.

**Fence out-of-scope:**
- 4-space indented code blocks (CommonMark style, without fence markers) are **not** recognized as passthrough regions — interpolation IS parsed inside them.
- Leaving a blockquote does not implicitly close a blockquoted fence: `> ``` ... content without > prefix ... > ` ``` `` — the fence closes only on an explicit matching closer (`> ` `` ` or `> ~~~`). Without an explicit closer the fence extends to end-of-file.
- **Unclosed code fence is a hard error** (`mds::syntax "unclosed code fence"`): a fence that is never closed by end-of-file causes compilation to fail rather than silently extend to end-of-file.

---

### 4.3 Conditionals

```mds
@if premium:
Thanks for being premium!
@end
```

With else:

```mds
@if premium:
Premium content here.
@else:
Free tier content here.
@end
```

**Negation** (`!`):

```mds
@if !debug_mode:
Production content here.
@end
```

**Equality comparison** (`==` / `!=`):

```mds
@if role == "admin":
Admin panel content.
@elseif role == "mod":
Moderator controls.
@else:
Regular user view.
@end
```

Comparison RHS must be a string, number, boolean, or null literal:

```mds
@if count == 0:
No results found.
@end

@if active == true:
Service is active.
@end

@if status != "disabled":
Feature is available.
@end
```

Single-quoted string literals are equally valid in comparisons:

```mds
@if role == 'admin':
Admin panel content.
@end

@if status != 'disabled':
Feature is available.
@end
```

Escape sequences (`\\`, `\"`, `\'`) are supported inside both single- and double-quoted comparison literals, matching function argument strings (see §4.5).

**`@elseif`** chains:

```mds
@if tier == "enterprise":
Enterprise features.
@elseif tier == "pro":
Pro features.
@elseif tier == "starter":
Starter features.
@else:
Free tier.
@end
```

**Rules:**

- Condition forms:
  - Truthy check: `@if var:` or `@if config.debug:`
  - Negation: `@if !var:` or `@if !config.debug:`
  - Equality: `@if var == "value":` / `@if var != "value":` (both double and single quotes are valid: `@if var == 'value':`)
  - Logical AND: `@if a && b:` — true when both operands are truthy (short-circuits on first false)
  - Logical OR: `@if a || b:` — true when any operand is truthy (short-circuits on first true)
  - Compound: `@if a && b || c:` — `||` has lower precedence than `&&`; operators inside quoted strings are not parsed as operators
  - Maximum 16 leaf operands per logical expression
- Falsy values: `false`, `null`, empty string `""`, empty array `[]`, empty object `{}`, `0`, `NaN`
- Everything else is truthy
- Equality is **strict**, no type coercion: comparing values of different types (e.g. `@if count == "3":` when `count` is the number `3`) is a **runtime error** (`mds::type_mismatch`). Convert explicitly: `@if string(count) == "3":` or `@if count == 3:`
- `NaN == NaN` is false (IEEE 754)
- `@elseif` branches are evaluated in order; first matching branch wins (short-circuit)
- `@elseif` must appear before `@else:`; `@else:` cannot be followed by `@elseif`
- Cannot combine negation with comparison: `@if !var == "x":` is a parse error. Use `@if var != "x":` instead
- `@if !!var:` (double negation) is a parse error
- Maximum 256 `@elseif` branches per `@if` block
- Nesting: plain `@end`, resolved by innermost matching

**Comparison semantics — type × operator:**

| LHS type | RHS type | `==` | `!=` | Notes |
|----------|----------|------|------|-------|
| string | string | structural | structural | `"3" == "3"` → true |
| number | number | IEEE 754 | IEEE 754 | `NaN == NaN` → false |
| boolean | boolean | structural | structural | |
| null | null | true | false | |
| any | **different type** | **error** | **error** | `mds::type_mismatch` — no implicit coercion |

**Truthiness vs equality — key distinction:**

| Value | `@if` (truthy check) | `@if x == false:` (equality) |
|-------|----------------------|-------------------------------|
| `false` | falsy | true (same type, same value) |
| `""` | falsy | — (must compare with `""` literal) |
| `0` | falsy | — (must compare with `0` literal) |
| `"0"` | **truthy** (non-empty string) | requires `x == "0"` |
| `"false"` | **truthy** (non-empty string) | requires `x == "false"` |

**`--set-string` truthiness footgun:** `--set-string count=0` sets `count` to the string `"0"`, not the number `0`. The string `"0"` is truthy (`@if count:` is true) even though the number `0` is falsy. Similarly, `--set-string flag=false` sets `flag` to the string `"false"` which is truthy. Compare with string literals explicitly when using `--set-string`: `@if count == "0":`.

---

### 4.4 Loops

```mds
@for item in items:
- {{item}}
@end
```

Key-value iteration over objects:

```mds
@for key, value in config:
{{key}} = {{value}}
@end
```

**Rules:**

- `@for item in iterable:` iterates over arrays; the iterable can be a variable name or dot path (`config.items`)
- `@for key, value in obj:` iterates over object entries in sorted key order
- Loop variables are block-scoped to the `@for...@end`
- Loop variable shadows any outer variable with the same name
- Iterating over a non-array with single variable → compilation error (use `key, value` for objects)
- Iterating with `key, value` over a non-object → compilation error

---

### 4.5 Functions

Definition:

```mds
@define greet(name):
Hello {{name}}, welcome!
@end
```

With default arguments:

```mds
@define greet(name = "World"):
Hello {{name}}!
@end

{{greet()}}
{{greet("Alice")}}
```

> **Note — no comment syntax:** MDS has no comment syntax. Unknown `@directives` (any
> `@word` not recognized by the compiler) are **syntax errors**, not comments. The `@#`
> annotation style used in some older examples is not valid MDS.

Invocation:

```mds
{{greet("Alice")}}
```

**Rules:**

- Functions are pure text templates (no side effects)
- Arguments are positional
- Functions can call other functions; direct recursion is rejected at compile time, and indirect call chains are bounded by a maximum call depth of 128
- Function body has its own scope; params shadow outer vars
- Parameters may have default values: `@define name(param = default):` — defaults are string, number, boolean, or null literals
- Required parameters must appear before optional (defaulted) parameters
- String arguments accept both double-quoted (`"value"`) and single-quoted (`'value'`) literals; both support `\\`, `\"`, and `\'` escape sequences
- Literal argument types: strings `"x"`, numbers `42`, `-1.5`, booleans `true`/`false`, null

**Built-in functions:**

MDS provides 18 built-in functions that can be called without `@define`:

| Function | Args | Description |
|----------|------|-------------|
| `upper(s)` | 1 | Convert string to uppercase |
| `lower(s)` | 1 | Convert string to lowercase |
| `trim(s)` | 1 | Strip leading/trailing whitespace |
| `replace(s, from, to)` | 3 | Literal string replacement |
| `split(s, sep)` | 2 | Split string into array |
| `starts_with(s, prefix)` | 2 | Returns true/false |
| `ends_with(s, suffix)` | 2 | Returns true/false |
| `contains(s_or_arr, needle)` | 2 | Works on string and array |
| `slice(s_or_arr, start[, end])` | 2–3 | Extract substring (char indices) or sub-array; clamps to bounds |
| `join(arr, sep)` | 2 | Join array of strings |
| `length(s_or_arr)` | 1 | String character count or array element count |
| `first(arr)` | 1 | First element or null for empty |
| `last(arr)` | 1 | Last element or null for empty |
| `reverse(s_or_arr)` | 1 | Reverse string (by Unicode scalar value) or array. Note: string reversal operates on Unicode scalar values, not grapheme clusters — combining diacriticals and multi-codepoint sequences (e.g. flag emoji) will not reverse correctly |
| `sort(arr)` | 1 | Sort homogeneous array (strings or numbers) |
| `unique(arr)` | 1 | Deduplicate (order-preserving) |
| `string(v)` | 1 | Convert any value to string |
| `number(v)` | 1 | Convert string/boolean/null to number |

User-defined functions shadow built-ins with the same name.

---

### 4.6 Imports

MDS supports three import styles:

**Alias import** - namespaces all exports under an alias:

```mds
@import "./utils.mds" as utils

{{utils.greet("Alice")}}
```

**Merge import** - exports merge directly into current scope:

```mds
@import "./base.mds"

{{greet("Alice")}}
```

**Selective import** - pick specific exports by name:

```mds
@import { greet, farewell } from "./utils.mds"

{{greet("Alice")}}
{{farewell("Alice")}}
```

**Rules:**

- Relative paths only (no bare module names)
- `as alias` namespaces all exports: access via `{{alias.name}}`
- Without alias (merge): exports enter current scope (name collision → compilation error)
- Selective: only listed names are brought into scope
- Circular imports → compilation error
- Resolved import paths stay inside the project root (see §5 Project Root and "Filesystem constraints" below); a path that escapes it is a compilation error
- Import resolution is recursive (imports can import)

#### Filesystem constraints

The resolver applies the rules below to the paths it opens — the entry file, each
`@import` target (body directives and frontmatter `imports:` entries alike), and the
base directory of a string compile — on the native filesystem backend (`NativeFs`:
the CLI, the Rust API, and the file-path entry points of the napi and Python
bindings). The in-memory backend (`VirtualFs`: `compile_virtual`, `check_virtual`,
`lint_virtual`, and the WASM binding) has no symlinks and no host paths; the NUL-byte,
forbidden-character, empty-path and segment-count rules apply to its entry keys and
import paths alike, and the containment rule to its import paths. A `VirtualFs` may
also carry aliases (`VirtualFs::with_aliases`, and so `compile_virtual_fs`,
`check_virtual_fs`, `lint_virtual_fs` and the WASM `moduleAliases` option): other
keys an import may resolve to, each mapped to the key of the module it names, which
becomes the key the import resolves to. Every alias and every key it names is
checked as a key an import can resolve to, and the map is bounded (§5 `mds::io`,
`mds::resource_limit`; #414). Each rule is a compilation error, reported with the
code shown.

| Constraint | Rule | Code and message |
|---|---|---|
| Relative form | An import path starts with `./` or `../`; bare module names and absolute paths are refused before any filesystem access. | `mds::import` — `import path must be relative (start with './' or '../'): "<path>"` |
| Empty path | An empty import path is refused. An empty entry path is refused by the resolver before the backend is called, on every entry API: the file-path APIs, `ModuleCache::resolve_path*`, `resolve_key` and `resolve_virtual_intrinsic*`, and the entry key of `compile_virtual`, `check_virtual` and `lint_virtual`. | Import: `mds::import` — `import path is empty`. Entry: `mds::io` — `entry path is empty` |
| NUL bytes | A path containing U+0000 is refused before it reaches the operating system. An entry path containing it is refused by the resolver before the backend is called, on every entry API listed for an empty path, so a custom `FileSystem` backend is covered too. | Import: `mds::import` — `import path contains null byte`. Entry: `mds::io` — `entry path contains null byte: "<path>"` (the path escaped per §7.5) |
| Forbidden characters | A path carrying any of the 80 codepoints of `mds::is_forbidden_path_char` is refused: every C0 control including TAB (U+0009) and LF (U+000A), DEL, every C1 control, U+061C, U+200E, U+200F, U+202A–U+202E, U+2066–U+2069, U+2028, U+2029 and U+FEFF. An import path, an entry path or virtual entry key (every entry API listed for an empty path) and a base directory are each refused by the resolver before any backend is called, so a custom `FileSystem` backend is covered too; on the native filesystem a resolved canonical path carrying one anywhere — an entry, import or base directory reached through a symbolic link into a hostile-named directory, or a project located under one — is refused as well. U+0000 keeps its own message (row above). Each message names the codepoint and shows the path as the caller wrote it, escaped with `mds::escape_path_for_message`, so it carries no forbidden character itself and never the absolute resolved path. Where each check runs is listed under "Forbidden-character enforcement" below. | Import: `mds::import` — `import path contains forbidden character U+XXXX: "<path>"` (a frontmatter `imports:` entry: `imports[<n>]: invalid path "<path>": contains forbidden character U+XXXX`). Entry: `mds::io` — `entry path contains forbidden character U+XXXX: "<path>"`. Base directory: `mds::io` — `base directory contains forbidden character U+XXXX: "<path>"`. Resolved path: `mds::io` — `resolved path contains forbidden character U+XXXX: "<path as written>"` |
| Symlink rejection | A path whose final component is a symbolic link is refused. The check canonicalizes the parent directory, joins the file name as written, and refuses the result when its own file type (not followed) is a symbolic link — on Windows also a junction or any other name-surrogate reparse point; it then canonicalizes it, and a result outside the canonical parent is refused too. A name that differs from the on-disk name only in case, on a case-insensitive volume, is not a link: it resolves as the operating system resolves it, and the module is keyed by its on-disk spelling, so every spelling of one file is one module. Symbolic links in parent directories are followed, and the resolved path is then subject to the containment rule. Applies to the entry file (a `ModuleCache::resolve_key` key included), each import target, and a base directory passed to `ModuleCache::resolve_source*` (through `FileSystem::anchor_base_dir`, before the project root is anchored there); the CLI applies the same check to the `--vars` file, and to the directory argument of `mds build`, `mds check`, `mds fmt`, `mds lint` and `mds watch` before it is walked — typed with or without a trailing `/` or `/.`, through which the operating system would follow the link — refusing a symlinked one as `mds::io`, `directory argument must not be a symlink: "<path>"`, exit 2 (#413). The string-compile functions (`compile_str_with`, `check_str_with`, `lint_str_with` and the bindings' `basePath`/`base_path`) resolve their base directory to its canonical form first, so a symbolic link there is followed rather than refused, and the resolved directory anchors containment. | `mds::import` — `symlinks are not allowed in imports: <path>`. Directory argument (CLI): `mds::io` — `directory argument must not be a symlink: "<path>"` |
| Root containment | After resolution the canonical path must lie inside the project root (§5 Project Root). A `..` sequence or a symlinked parent that leads outside the root is refused; on the virtual backend, `..` above the virtual root is refused. | `mds::import` — `import path escapes project directory: "<path>"` |
| Path encoding | An entry path or base directory that is not valid UTF-8 is refused at the public API boundary rather than converted lossily. On the native filesystem a resolved canonical path that is not valid UTF-8 — an entry, import or base directory reached through a symbolic link into a directory whose name is not — is refused as well, never keyed by its lossy form (which would name a different file). On the CLI this is exit 2 (§7.9), and a `-o` or `--out-dir` (`mds build`, `mds watch`) that is not valid UTF-8 is refused before any input is read or anything is created (#390). | `mds::io` — `path is not valid UTF-8` (entry path), `base_dir path is not valid UTF-8` (base directory), `resolved path is not valid UTF-8: "<path as written>"` (resolved path), or `-o/--output is not valid UTF-8: "<value>"` and `--out-dir is not valid UTF-8: "<value>"` (CLI; U+FFFD for each invalid sequence) |
| Segment count | An import path of more than 256 segments is refused on both backends (on the virtual backend it is counted after it resolves against the importing directory), as is an entry file path resolved through `FileSystem::resolve_entry`. | `mds::resource_limit` — `import path exceeds maximum segment count (256)` |

Enforced by `validate_relative_import`, `validate_entry_path`, `check_segment_count`,
`NativeFs::check_symlink`, `NativeFs::anchor_base_dir` and `NativeFs::check_path_traversal`
(`crates/mds-core/src/fs.rs`), `validate_import_path` and `resolve_entry_key`
(`crates/mds-core/src/resolver.rs`) and `path_to_str` / `resolve_base_dir`
(`crates/mds-core/src/lib.rs`); pinned by the `native_resolve_entry_*`,
`native_normalize_in_dir_*`, `native_anchor_base_dir_*`, `native_resolve_key_*`,
`vfs_resolve_entry_*`, `vfs_normalize_in_dir_*` and `case_*` tests,
`symlinked_base_dir_refused_by_resolve_source_followed_by_string_api` and (Windows)
`junction_final_component_is_refused` in `fs.rs`,
`case_mismatched_entry_and_import_are_not_symlink_errors` in
`crates/mds-cli/tests/security.rs`,
`custom_backend_entry_validation_runs_before_backend`,
`virtual_entry_apis_validate_the_entry_key` and `nul_in_entry_path_is_io_error` in
`crates/mds-core/tests/api_surface.rs`, `symlink_import_rejected` and
`path_traversal_import_rejected` in `crates/mds-cli/tests/security.rs`, and the
`*_rejects_non_utf8_*` tests in `crates/mds-core/tests/api_surface.rs`. Directory-mode
commands additionally skip symlinked entries inside the tree (§7.2).

##### Forbidden-character enforcement

`mds::is_forbidden_path_char` (#265) is checked where each input enters, before any
filesystem backend is called:

| Input | Checked by | Code |
|---|---|---|
| An import path — a body directive (`@import`, `@extends`, …) or a frontmatter `imports:` entry | `validate_import_path` in the resolver, for every backend, and `validate_relative_import` again in both built-in backends' `normalize_in_dir`; the frontmatter parser classifies the path with the same `import_path_violation` | `mds::import` |
| An entry path or virtual entry key (every entry API listed for an empty path, so the WASM `filename` too) | `validate_entry_path` in the resolver (`resolve_entry_key`), for every backend, and again in both built-in backends' `resolve_entry` | `mds::io` |
| The base directory of a string compile (`compile_str_with`, `check_str_with`, `lint_str_with`, the bindings' `basePath`/`base_path`) and of the formatter's safety gate (`format_str_with`/`format_str_named`, so `mds fmt`) | `resolve_base_dir`, on the form as given and on its canonical form | `mds::io` |
| The working directory — the base directory of `mds build`, `mds check`, `mds lint` and `mds fmt` reading `-` (stdin), and of a string compile given none | `resolve_base_dir`, on the working directory; the message names it `"."` (`resolved path contains forbidden character U+XXXX: "."`), never its absolute path | `mds::io` |
| A base directory passed to `ModuleCache::resolve_source*` | the resolver, for every backend, and `NativeFs::anchor_base_dir` again | `mds::io` |
| A resolved canonical path | `NativeFs`, on every path it canonicalizes — entry, import and base directory — scanned whole, not only its final component | `mds::io` |
| A file the CLI opens itself — a `mds lint`/`mds fmt` file argument, the `--vars` file | `NativeFs::check_symlink`, on the path as given and on its canonical form (`path contains forbidden character U+XXXX: "<path>"`); a `mds lint`/`mds fmt` file argument is refused with the same message before its existence and `.mds`-extension checks, so a missing or non-`.mds` hostile path reports it too, and `mds watch` refuses a `--vars` path the same way before it probes it or watches its directory | `mds::io` |
| The directory argument of `mds build`, `mds check`, `mds fmt`, `mds lint` and `mds watch` | the CLI, before the directory is walked, whatever spelling reaches it (`.`, `..` and `sub/..` included): on the path as typed (`path contains forbidden character U+XXXX: "<path>"`) and on its canonical form — a symlinked or hostile-named directory above it (`resolved path contains forbidden character U+XXXX: "<path as typed>"`), so no file of the tree is read; a file inside the walked tree whose own path below the argument carries one still fails on its own (§7.2) (#413) | `mds::io`, exit 2 |
| `-o`/`--output` and `--out-dir` (`mds build`, `mds watch`); `mds.json` `build.output_dir`, checked when the config is loaded — by `build`, `watch`, `lint` and directory-mode `fmt`, not by `check`, which does not read `mds.json`; `mds init <filename>` | the CLI, before any input is read or any file is written — `-o`, `--out-dir` and `build.output_dir` both as written and in the form they resolve to (a symlink, or a working directory above a relative value, leading into a hostile-named directory: `<setting> resolved path contains forbidden character U+XXXX: "<value as written>"`; a location that does not exist yet is resolved through its deepest existing ancestor) | `mds::io`, exit 2 |
| The entry path and imports of `@mdscript/mds`'s `compileFile`/`checkFile`/`lintFile` on the WASM backend | the package's JS pre-scanner (`packages/mds/src/util/path-chars.ts`, used by `module-scanner.ts`), before it opens any file: the entry path and each import string as written, then each resolved path — with the same codes and messages as the native backend | `mds::import` / `mds::io` |

The one input outside that list is a path a custom `FileSystem` backend passed to
`ModuleCache::with_fs` produces itself — a key it rewrites, a link it follows. The
resolver still refuses every caller-supplied path above before calling such a backend,
but it never sees the backend's own paths, so a custom backend MUST apply
`mds::is_forbidden_path_char` to them itself (the `FileSystem` trait's "Security
Contract" in `crates/mds-core/src/fs.rs`).

Pinned by `forbidden_path_char_class_is_exactly_80` and
`escape_path_for_message_leaves_no_forbidden_char` in
`crates/mds-core/src/lint/diagnostic.rs`, `validate_import_path_refuses_every_forbidden_char`
in `crates/mds-core/src/resolver_tests.rs`, `normalize_in_dir_refuses_every_forbidden_char`,
`resolve_entry_refuses_every_forbidden_char`, `native_anchor_base_dir_refuses_forbidden_chars`
and `native_canonical_path_through_a_hostile_directory_is_refused` in `fs.rs`, the tests
in `crates/mds-core/tests/forbidden_path_chars.rs` (import, entry-key, base-directory,
custom-backend and symlinked-hostile-directory cases) and
`crates/mds-cli/tests/forbidden_paths.rs` (the directory-walker matrix over `build`,
`check`, `fmt`, `lint` and `watch`, directory and single-file arguments, the output
locations and `mds init`), and U-FP1–U-FP5 in `packages/mds/__test__/forbidden-path-chars.spec.mjs`,
which compare the JS pre-scanner with the native and WASM engines over all 80
codepoints.

---

### 4.7 Exports

MDS supports three export styles:

**Named export** - export a locally defined symbol:

```mds
@define greet(name):
Hello {{name}}!
@end

@export greet
```

**Re-export from** - re-export a symbol from another module without importing it locally:

```mds
@export greet from "./greetings.mds"
@export farewell from "./greetings.mds"
```

**Wildcard re-export** - re-export everything from another module:

```mds
@export * from "./formatting.mds"
```

**Rules:**

- Only exported symbols are visible to importers
- If no `@export` directives exist: everything is exported (default-public)
- Once any `@export` is present: only explicitly exported symbols are visible
- Exportable: functions, the prompt body (as `prompt`)
- `@export from` does not bring the symbol into the current file's scope
- `@export *` re-exports all exports from the target module
- Name collisions across wildcard re-exports → compilation error

---

### 4.8 Includes

```mds
@import "./header.mds" as header

@include header
```

**Rules:**

- Renders an imported module's compiled prompt body inline
- Every module with text content has an implicit `prompt` export
- `@include alias` renders that module's prompt body at the include site
- Module must be imported first via `@import`
- A module with only function definitions and no body text → `@include` produces empty string (warning)

---

### 4.9 Module System Summary

A complete barrel/index file example:

```mds
# prompts/greetings.mds
@define hello(name):
Hello {{name}}!
@end

@define welcome(name, role):
Welcome {{name}}, you're joining as {{role}}.
@end

@export hello
@export welcome
```

```mds
# prompts/formatting.mds
@define bullet_list(items):
@for item in items:
- {{item}}
@end
@end

@define numbered_list(items):
@for item in items:
1. {{item}}
@end
@end

@export bullet_list
@export numbered_list
```

```mds
# prompts/index.mds - barrel file
@export * from "./greetings.mds"
@export * from "./formatting.mds"
```

```mds
# main.mds - consumer
---
user: Alice
tools: [search, code, browse]
---

@import "./prompts/index.mds" as prompts

{{prompts.hello(user)}}

You have access to:
{{prompts.bullet_list(tools)}}
```

Output:
```markdown
---
user: Alice
tools: [search, code, browse]
---


Hello Alice!

You have access to:
- search
- code
- browse
```

---

### 4.10 Messages (@message)

`@message` blocks structure a template as a sequence of chat messages, enabling output as a JSON array instead of plain text.

```mds
@message system:
You are a helpful assistant.
@end

@message user:
Hello!
@end
```

**Role forms:**

| Form | Meaning |
|------|---------|
| `@message system:` | Bare word — the role is the literal string `"system"` |
| `@message {{role}}:` | Expression — the role is evaluated at runtime from the variable |

```mds
---
role: assistant
---

@message {{role}}:
This role comes from the variable.
@end
```

**Intrinsic output shape:**

Output format is decided by the template content, not a flag. A template containing
any `@message` block compiles to a JSON array; all other templates compile to Markdown.
Detection is **static**: the presence of a `@message` block anywhere in the parse tree
(even inside an `@if` branch that is never taken at runtime) makes the template a
messages template.

| Kind | When | Output |
|------|------|--------|
| Markdown | No `@message` blocks anywhere in template | Plain text / Markdown string |
| Messages | Any `@message` block present (anywhere, even dead-coded) | Pretty-printed `[{role, content}, …]` JSON array |

```mds
# Source (messages template — contains @message blocks):
@message system:
You are a helpful assistant.
@end

@message user:
Hello!
@end

# Compiled output (messages kind):
[
  { "role": "system", "content": "You are a helpful assistant." },
  { "role": "user",   "content": "Hello!" }
]
```

A messages template that produces zero messages at runtime emits `[]` — this is
valid, not an error.

**Mixed content is a hard compile error:**

Loose top-level prose or interpolations alongside `@message` blocks — content that
would be rendered in the Markdown path — are rejected with `mds::mixed_content`
rather than silently dropped or auto-wrapped. There is no "text mode" that renders
`@message` bodies inline; the template kind is fixed at compile time.

**Rules:**

- Role must be a non-empty string; an empty or whitespace-only bare-word role is
  a parse error
- Bare-word roles are always literal strings — they never look up variables
- Dynamic roles (`{{expr}}`) must evaluate to a non-empty, non-whitespace string at
  runtime: a non-string value → type error; a string that trims to empty →
  type error (the same rejection applies at runtime as at parse time)
- Outer whitespace of the body is trimmed; inner whitespace is preserved
- Empty bodies (trims to empty string) are silently skipped
- Frontmatter is excluded from message content
- Nested `@message` blocks are a parse error
- Top-level prose/interpolations alongside `@message` blocks → `mds::mixed_content` compile error
- A top-level `@include` in a messages template emits a warning (included module bodies are not surfaced as messages — compose with `@message` blocks directly)
- `@if` and `@for` around `@message` blocks work normally; the same iterable rules apply (see §4.4)

**Control flow inside @message:**

```mds
---
admin: true
tools: [search, code]
---

@message system:
@if admin:
You have admin privileges.
@end
Available tools:
@for tool in tools:
- {{tool}}
@end
@end
```

**Resource limits:**

| Limit | Value |
|-------|-------|
| `MAX_MESSAGE_COUNT` | 10,000 messages per compilation |
| Cumulative content size | 50 MB total across all message bodies |

Exceeding either limit returns a `resource_limit` error rather than allowing runaway memory use.

---

### 4.11 Template Inheritance (@extends / @block)

Template inheritance lets a **child** template reuse a **base** template's skeleton while selectively overriding named regions.

#### Overview

A **base** template defines named placeholder regions with `@block name:` ... `@end`. It compiles standalone; its block bodies serve as defaults.

A **child** template declares `@extends "./base.mds"` and then provides `@block name:` ... `@end` overrides. The child must contain **only** block overrides (plus optional blank lines) — any other content is a compile error.

The compiler splices overridden blocks into the base skeleton, validates and evaluates the merged result as a single unit.

#### Syntax

```mds
# base.mds — defines three placeholder blocks
You are a {{role}} assistant.

@block instructions:
Analyze data carefully.
@end

@block tools:
@end

@block output_format:
Respond in plain text.
@end
```

```mds
# child.mds — overrides instructions and tools; inherits output_format default
---
role: data analysis
---
@extends "./base.mds"
@block instructions:
Perform statistical analysis.
@end
@block tools:
You have access to: Python, R
@end
```

Compiled output:

```
---
role: data analysis
---
You are a data analysis assistant.

Perform statistical analysis.

You have access to: Python, R

Respond in plain text.
```

The blank lines between sections come from the base skeleton (the blank line between each `@end` and the next `@block` directive is part of the skeleton and is carried through verbatim).

#### Rules

**Directive placement:**

- `@extends` must be the first directive after the optional frontmatter — only one `@extends` is allowed
- `@block name:` ... `@end` declares a named region in the base, or overrides it in a child
- `@block` is top-level only; it cannot appear inside `@if`, `@for`, `@define`, `@message`, or another `@block`

**Child body constraints:**

- A child template may contain only `@block` overrides (plus blank lines between them)
- Any other content (text, `@import`, `@if`, etc.) outside a block override is a compile error
- A child may override a block multiple times — last definition wins (per parse order)

**Block ownership and scope:**

- Block names must be declared in the **root base** template; a child cannot introduce new block names
- Blocks share the merged scope — all frontmatter variables, functions, and imports are available inside any block body
- Block name collides with `@define` → `mds::name_collision`; duplicate `@block` in the same module → `mds::name_collision`

**Frontmatter merging:**

- Frontmatter from all ancestors is deep-merged in order: base < intermediate < child < runtime vars
- Nested mappings are merged key-by-key; arrays replace wholesale; scalars: child wins
- Reserved keys (`imports`, `type`, `extends`) are excluded from the merged scope
- Per-file `imports:` entries in frontmatter are each resolved against their own file's location
- The **deep-merged** frontmatter (base < child, reserved keys excluded) is emitted in the compiled output — not just the child's raw frontmatter. Both base-only and child-only keys appear; child wins on collisions

**Named asymmetry — `@extends` vs standalone:**

`@extends` templates and standalone templates differ in how frontmatter is emitted:

| Template type | Emitted frontmatter |
|---------------|---------------------|
| **Standalone** | Raw-verbatim: the original source YAML between `---` fences, byte-for-byte (comments and quoting preserved) |
| **`@extends` child** | Canonically re-serialized: serde-yaml output of the deep-merged mapping (comments and original quoting are normalized away; YAML structure is canonical) |

This asymmetry is intentional: standalone templates can round-trip YAML comments and non-canonical quoting, while `@extends` templates must emit a merged structure that has no single "source" YAML string. Runtime `--set`/`--set-string` variables do not alter the emitted frontmatter in either case — they affect only the compiled body.

**Intrinsic output with inheritance:**

Output kind follows the same intrinsic rule as §4.10: a template (or any base it
extends) containing a `@message` block anywhere produces a messages array; otherwise
the compiled output is Markdown. `@block` bodies render in the merged base skeleton;
`@message` blocks inside `@block` bodies participate in the messages array.

Example — messages template via inheritance (base contains `@message` inside `@block`):

```mds
# base.mds
@block context:
@message system:
You are a {{role}} assistant.
No additional context.
@end
@end
```

```mds
# child.mds
---
role: research
---
@extends "./base.mds"
@block context:
@message system:
You are a {{role}} assistant.
Focus on peer-reviewed sources.
@end
@end
```

Compiled output (messages kind — intrinsic because `@message` is present):

```json
[
  { "role": "system", "content": "You are a research assistant.\nFocus on peer-reviewed sources." }
]
```

**Whitespace contract:**

Block bodies follow the **interior-verbatim with trailing-edge normalization** contract:
- Leading blank lines and interior blank runs inside a block body are preserved verbatim.
- Only the trailing edge is normalized: trailing whitespace is stripped and exactly one final newline is appended; `\r` is stripped unconditionally.
- This differs from `@message` and `@define` bodies, which still edge-trim (`.trim()`) — they strip leading and trailing blank lines. Block bodies do not.

For the base skeleton:
- Skeleton whitespace around a spliced block carries through to the output verbatim, except that `@end` consumes the single newline immediately following it. A blank line between two `@block` declarations in the base renders as one blank line between the corresponding bodies in the output; back-to-back `@block` declarations (no separating blank line) render with no separator between bodies.
- Spacing before and after a spliced block is determined by the surrounding base skeleton, not the block body.

**Error codes:**

| Error | Trigger | Code |
|-------|---------|------|
| E1 | `@extends` not first directive | `mds::extends` |
| E2 | Two `@extends` in one file | `mds::extends` |
| E3 | Child content outside `@block` overrides | `mds::extends` |
| E4 | Child overrides a block not declared by the root base | `mds::extends` |
| E5 | Circular inheritance (A→B→A, or self-extension) | `mds::circular_import` |
| E7 | `@block` name collides with `@define` | `mds::name_collision` |
| E8 | Duplicate `@block` in same module | `mds::name_collision` |
| E9 | `@block` nested inside another `@block` | `mds::syntax` |
| E10 | Base file not found | `mds::file_not_found` |

**Resource limits:**

| Limit | Value |
|-------|-------|
| `MAX_BLOCKS_PER_MODULE` | 256 blocks per module |
| `MAX_FRONTMATTER_MERGE_DEPTH` | 64 levels of nested YAML merging |

---

## 5. Compilation Model

| Phase | Description | Errors |
|-------|-------------|--------|
| 1. Parse | Tokenize → AST (frontmatter, directives, text nodes) | Syntax errors (unexpected token, unclosed block) |
| 2. Resolve | Recursively load imports, build dependency graph | File not found, circular import |
| 3. Validate | Check all references, types, arity | Undefined var/function, type mismatch, wrong arg count |
| 4. Evaluate | Execute directives (expand loops, resolve conditions, call functions) | Iterate non-array, recursion detected |
| 5. Render | Flatten evaluated tree → final Markdown string | (none expected) |

### Project Root

The compiler establishes a project root once per compilation. The root is used for two purposes: enforcing import containment (a resolved path that escapes it is a compilation error) and computing relative paths in Source Map v3 `sources[]` entries (see §7.5).

**Discovery.** Starting from a base directory, the compiler walks upward, examining each ancestor level for the presence of a `.git` or `.mdsroot` marker. The nearest ancestor directory that holds either marker becomes the project root.

Two base directories are possible:

- For a file compile, the walk starts from the directory containing the compiled file.
- For a string compile that provides an explicit base directory (the `basePath` option in the JavaScript API; the `base_path` parameter in the Python binding), the walk starts from that base directory.

**`.git` and `.mdsroot` are not ordered relative to each other.** The rule is nearest-ancestor: whichever marker appears in the closest ancestor wins. A consequence: placing a `.mdsroot` in a subdirectory that is nearer to the input than an existing `.git` narrows the containment boundary — files above that `.mdsroot` but still inside the repository are then outside the project root, and imports to them are rejected.

**Marker, not configuration.** The contents of `.mdsroot` are never read. An empty file works; a directory named `.mdsroot` also works.

**Depth limit.** The walk ascends at most 256 directory levels. If no marker is found within that range, the starting directory itself becomes the project root, silently.

**`mds.json` discovery is a separate, independent walk** (see §7.8). It shares only the 256-level depth limit. Finding (or not finding) `mds.json` has no effect on the project root, and project-root resolution has no effect on `mds.json` discovery.

### Frontmatter Preservation

When the input file has YAML frontmatter, the compiled output preserves it:

- The original frontmatter content is prepended to the output between `---` fences
- The `type: mds` key (used for `.md` file detection) is stripped from the output frontmatter
- If stripping `type: mds` leaves the frontmatter empty, no fences are emitted
- Runtime variable overrides affect the body but do not alter the output frontmatter
- Only the root module's frontmatter appears in output; imported modules' frontmatter is not emitted
- When `@extends` is used, the compiled output contains the **deep-merged** frontmatter (base keys + child keys, child wins on collision). Unlike standalone frontmatter — which is preserved byte-for-byte from the source — the merged result is serde-canonicalized: key order and quoting are normalized by the YAML serializer, and YAML comments are dropped.

### Error Format

```
mds::undefined_var

  × undefined variable 'username'
   ╭─[src/welcome.mds:1:7]
 1 │ Hello {{username}}!
   ·        ────┬────
   ·            ╰── not defined
   ╰────
  help: define 'username' in frontmatter or imports
```

Errors include a diagnostic code (`mds::*`), file path, line number, column, a visual span, and a contextual explanation. Compilation fails fast on first error; no partial output.

### Error Codes

Each `MdsError` the compiler produces, and each error a binding synthesises at its
boundary, carries a `code` of the form `mds::<name>`. On the CLI the code is the first
line of the rendered diagnostic (above) and the `error.code` field of the
`mds lint --format json` envelope; on napi and WASM it is `err.code`; on Python it is
`MdsError.code`; in `@mdscript/mds` it is the `code` property `isMdsError()` checks.
Codes are stable identifiers a consumer may branch on; renaming or removing one is a
breaking change.

"Exit" is the CLI exit class (§7.9) for `mds build`, `mds check`, `mds fmt` and
`mds watch` startup — 1 template or content error, 2 I/O or file-system error,
3 resource limit — followed by the `mds lint` code, which reports analysis failures
as 2 except for the two carve-outs shown. The CLI's own stream and I/O failures are
described below the table.

| Code | Meaning | Raised by | Exit | Surfaces |
|---|---|---|---|---|
| `mds::syntax` | Parse error: unexpected token, unclosed block, malformed directive | parser | 1 / 2 | all |
| `mds::undefined_var` | Variable not defined in frontmatter, imports or runtime vars | validator, evaluator | 1 / 2 | all |
| `mds::undefined_fn` | Function not defined with `@define` or imported | validator | 1 / 2 | all |
| `mds::arity` | Wrong number of arguments in a call | validator | 1 / 2 | all |
| `mds::builtin` | A built-in function rejected its arguments at runtime | evaluator | 1 / 2 | all |
| `mds::type_error` | `@for` over a value that is not an array | evaluator | 1 / 2 | all |
| `mds::type_mismatch` | Cross-type `==` / `!=` comparison | evaluator | 1 / 2 | all |
| `mds::circular_import` | Import graph contains a cycle | resolver | 1 / 2 | all |
| `mds::file_not_found` | Entry file or import target does not exist, or a directory above it does not, or the path cannot otherwise be resolved — a symlink loop, a name too long, a directory that cannot be searched (native filesystem) | `NativeFs`, CLI input check; `@mdscript/mds`'s WASM-backend file pre-scanner, with the native message | 2 / 2 | CLI, Rust, napi, Python, `@mdscript/mds` |
| `mds::import` | An `@import` the resolver refuses: not `./`/`../`-relative, empty, NUL byte, a forbidden path character, symlinked final component, escapes the project root, or another import-directive violation (§4.6 "Filesystem constraints"); on `@mdscript/mds`'s WASM backend also, on POSIX, an import whose own path names a symlinked directory and leaves it through `..` (`./link/../x.mds`), which its by-name virtual filesystem would resolve to a different file than the native backend reads (Windows applies `..` lexically before it follows a link, so there both backends read the same file) | resolver, `NativeFs`, `VirtualFs`, `@mdscript/mds` WASM-backend pre-scanner | 1 / 2 | all |
| `mds::name_collision` | A merge import or definition redefines a name already in scope | resolver | 1 / 2 | all |
| `mds::not_mds` | An entry file, or the target of an `@import`, `@export … from`, frontmatter import or `@extends`, is not an MDS file (neither a `.mds` file nor a `.md` file whose frontmatter declares `type: mds`). The message names the path as the caller typed it — the entry path as passed, or the string as written in the template — escaped, never the resolved absolute path (#417) | resolver; `mds fmt`/`mds lint` file-argument check; `@mdscript/mds`'s WASM-backend pre-scanner, through the resolver's own check (`preflightModule`) | 2 / 2 | all |
| `mds::io` | Filesystem or I/O failure; a path or base directory that is not valid UTF-8; an entry path or virtual entry key that is empty or contains a NUL byte; an entry path, entry key, base directory or resolved canonical path carrying a forbidden path character (§4.6); a `VirtualFs::with_aliases` alias refused — the alias, or the module key it names, not a normalized module key, the alias a module key itself, or the module key naming no module (`module alias "<alias>": <reason>`, #414); on the CLI also a `--vars` file that is a symlink, a directory argument that is a symlink or the filesystem root (#413), an output path that is the entry file itself (#425), a `-o`/`--out-dir`/`build.output_dir`/`mds init` path carrying a forbidden path character (for `-o`/`--out-dir`/`build.output_dir`, as written or as resolved), a `-o`/`--out-dir` value that is not valid UTF-8, a working directory that cannot be determined when a relative `-o`/`--out-dir` or a source map's base needs it (`cannot determine current directory: <reason>`, the words mds-core uses when a string compile given no base directory cannot determine it, #390), a working directory that auto-detection cannot list (`cannot read directory .: <reason>`, #390), a `build.output_dir` containing `..`, a `lint --fix` rewrite refused by the compile-equivalence check, the stream and I/O failures listed below the table (#157), and an `mds watch` file-mode rebuild refused because the entry's path as typed now leads to a different file than the one being watched (a symlinked directory on it retargeted: `watched entry now resolves to a different file: "<path>"; restart mds watch to follow it`, #417), or a directory-mode rebuild refused because the directory argument, or a source below it, as typed now leads to a different directory or file than the one being watched (`watched directory now resolves to a different directory: "<dir>"; restart mds watch to follow it`, `watched file now resolves to a different file: "<dir>/<path below it>"; restart mds watch to follow it`; §7.2, #413) | `mds-core` API boundary, resolver, `NativeFs`, `VirtualFs`, CLI, `@mdscript/mds` WASM-backend pre-scanner | 2 / 2 | all |
| `mds::resource_limit` | A documented limit exceeded (§4.1 resource-limits table, `SECURITY.md`), CLI stdin over 10 MiB included; bindings also raise it before compilation for oversized sources, module and alias maps, and their counts (as `VirtualFs::with_aliases` does for an alias map); `@mdscript/mds`'s WASM-backend pre-scanner raises the native error for a path over the segment cap, a file over the size cap and a module over the module count, and one of its own for modules that together pass 10 MiB | evaluator, resolver, `VirtualFs`, bindings, `@mdscript/mds` WASM-backend pre-scanner | 3 / 3 | all |
| `mds::yaml` | Frontmatter YAML the parser itself refuses (syntax, duplicate keys, nesting beyond the parser's limits — §4.1) | resolver | 1 / 2 | all |
| `mds::json` | Malformed JSON, or a non-object root, in `load_vars_str` and other JSON sites | `mds-core` vars API | 1 / 2 | Rust, CLI |
| `mds::invalid_vars` | `--vars` file is malformed JSON or not an object (`load_vars_file`) | `mds-core` vars API | 1 / 2 | CLI, Rust |
| `mds::var_conflict` | Same key given to both `--set` and `--set-string` | CLI | 1 / 1 | CLI |
| `mds::module_not_found` | Virtual backend: a key absent from the module map | `VirtualFs` | 1 / 2 | napi, WASM, Python, Rust |
| `mds::recursion` | A `@define` calls itself, directly or indirectly | evaluator | 1 / 2 | all |
| `mds::export` | `@export` of a name that is not defined, or an invalid re-export | resolver | 1 / 2 | all |
| `mds::extends` | Template inheritance error (E1–E10, §4.11) | resolver | 1 / 2 | all |
| `mds::mixed_content` | Content outside `@message` blocks in a messages template | evaluator | 1 / 2 | all |
| `mds::expected_markdown` | Rust API only: `CompileResult::into_markdown()` on a messages result | `mds-core` API | n/a | Rust |
| `mds::expected_messages` | Rust API only: `CompileResult::into_messages()` on a markdown result | `mds-core` API | n/a | Rust |
| `mds::formatter_invariant` | The formatter's rewrite failed the compile-equivalence gate — a formatter defect; nothing is written | formatter | 1 / n/a | CLI (`fmt`), Rust |
| `mds::internal` | A panic caught at a binding boundary, or a result that could not be serialised; the raw payload is attached as `detail` only under the off-by-default `debug-panics` feature (`SECURITY.md`). On the CLI, the error `mds lint --format json` records for an input whose analysis panicked, with the fixed message `internal compiler error` and nothing of the panic (§5 "Panics on the CLI") | napi, WASM, Python; `mds lint` | n/a / 101 | napi, WASM, Python, CLI (`lint --format json`) |
| `mds::invalid_options` | Malformed or type-incorrect options: unknown keys, wrong types, `basePath` on file methods or on the WASM backend, source-map options on `check`, an empty `basePath`, a WASM `moduleAliases` entry that is not a normalized module key or names no module (`options.moduleAliases["<alias>"]: <reason>`) | napi, WASM, Python, `@mdscript/mds` | n/a | napi, WASM, Python, `@mdscript/mds` |
| `mds::filename_collision` | `options.modules` already contains the entry `filename` | WASM (surfaced through `@mdscript/mds`) | n/a | WASM, `@mdscript/mds` |
| `mds::invalid_backend_result` | The selected backend returned a result of an unexpected shape | `@mdscript/mds` | n/a | `@mdscript/mds` |

CLI-authored errors that are not `MdsError`s — an unreadable, oversized or malformed
`mds.json`, `mds init` refusing a `..` path — carry no `mds::` code; they exit 1 under
`build`/`check`/`fmt` and 2 under `lint`. A `build.output_dir`
containing a `..` component or a forbidden path character, a `-o`/`--out-dir` value
or `mds init` filename containing a forbidden path character, a `-o`/`--out-dir` value
that is not valid UTF-8, a relative output location with no working directory to
resolve it against, and a working directory that auto-detection cannot list, are `mds::io`
(exit 2) instead (#390).

**Streams and I/O failures on the CLI (#157).** Under `mds build`, `mds check`,
`mds fmt`, `mds init` and `mds lint`, and for clap's own help, version and usage output:

- A closed stdout or stderr — the pipe's reader is gone — never changes the exit code.
  With stdout closed, the run writes nothing more to stdout and finishes; with stderr
  closed, it drops its diagnostics and still writes every output.
- These I/O failures are `mds::io` and exit at least 2 (a resource limit keeps its 3):
  an output, `.map` sidecar or starter file that cannot be written, a symlink at the
  destination refused, an output directory that cannot be created, a stale sibling or
  sidecar that cannot be removed, a stale sidecar that cannot be read, a stdout write
  that fails for any other reason than a closed pipe — reported once however many
  writes in a row it fails, and again if stdout fails anew after a write has landed —
  and stdin that cannot be read or is not valid UTF-8. In directory
  mode the other files are still processed, and the run exits 2 when any file's failure
  is in the exit-2 class, a source that cannot be read included (§7.2
  continue-on-error); a file whose `--diff` output a failing stdout lost counts as
  failed — `with errors` under `mds lint` (§7.5) — as a file whose rewrite fails does.
  The `mds.json` errors above are not in this list. A `mds lint --fix` rewrite that
  fails is in it, reported as `mds::io` in every mode, in one wording (§7.5).
- Stdin over the 10 MiB cap is `mds::resource_limit`, exit 3, under `build`, `check`,
  `fmt` and `lint`; exactly 10 MiB is accepted. Under `mds lint` a source file over the
  cap is `mds::resource_limit` too, as a file argument or a directory's entry, in every
  mode and format; a directory still lints its other files and exits 3 (§7.5).
- On Unix the Rust runtime treats EBADF on a standard stream — a descriptor that is
  not open for the direction used — as success: a write to it is dropped, and a read
  from it is end of input. Such a stream is therefore not reported as a failure.

Under `mds watch` (#157), a closed stderr only drops the status lines: the session keeps
watching. With `-o -`, stdout is the session's product, so a reader that is gone ends the
session: the write that finds the pipe closed — the startup write or a rebuild's — is
followed by `Stopped watching (stdout closed).` on stderr (not under `--quiet`) and exit
0. A stdout write that fails for another reason is `mds::io`, reported once however many
writes in a row it fails, and again if stdout fails anew after a write has landed; watching
continues. The lost content is not recorded as written, so the next rebuild writes it
again even when its output has not changed — saving the source unchanged retries it.
Once the session is live — its startup
compile finished and every watch armed — an output failure, whether a rebuild's output
file or stdout write or a stderr that fails other than by a closed pipe, is reported
where stderr still works and does not change the exit code: Ctrl+C exits 0. A session
that ends before it is live — at a startup error, or at a startup write that finds
stdout's reader gone — exits by the rules above, so a stderr that failed other than by a
closed pipe lifts its exit code to at least 2.

**Panics on the CLI (#389).** A panic in `mds` is an internal compiler error. Stderr
gets exactly two lines, `mds: internal compiler error` and `note: this is a bug in mds;
please report it at <repository>/issues` (the repository `mds-cli`'s manifest names),
and the run exits 101. The panic's message and source location are never shown. A panic
compiling one file of a batch — a file of `mds build`, `mds check`, `mds fmt` or
`mds lint` given a directory, and any compile of an `mds watch` session — fails that
file alone: only the compile, format or analysis of the file is caught, never a write
or delete of an output. The file counts as failed in the run's summary (`mds lint`:
under "with errors"), the run goes on with the other files, and it exits 101 when it
has finished. `mds watch` takes it as a failed compile and keeps watching — the
next edit rebuilds — and exits 101 when it is stopped. The text is the panic's one
report: no error line names the file, and the only error object made of it is
`mds lint --format json`'s — a directory's entry
`{"file": …, "error": {"code": "mds::internal", "message": "internal compiler error",
"help": null, "span": null}}`, or, for a panic in the `--fix` pipeline of a file
argument, the run's error document `{"version": 1, "error": {…}}` with the same error.
A panic on a thread other than the one running the
command ends the process at once with the same text and exit 101, and so does a second
panic while the first is still unwinding — a destructor that panics, or the panic Rust
raises when an unwind reaches a function that cannot unwind. Ending the process at once
runs no destructors, so an output being written at that moment can be left part-way:
its temporary file (`.mds-tmp-….tmp`) can stay beside it, and stdout's reader can get
part of the product. A panic on the command's own thread otherwise unwinds to the end
of the run, which removes such a temporary file on the way. 101 wins over every other
exit code: a closed pipe, an I/O failure and `mds watch`'s rule once live change
nothing. The panic is recorded before the text is written, and the text is written once
with its write error ignored, so with stderr closed or failing — or held by another
thread's write, if the run ends first — the run still exits 101. With `RUST_BACKTRACE`
set to anything but `0`, a `stack backtrace:` line and the panicking thread's frames
follow the text, from where the panic hook captured them; `full` shows the same frames
with each frame's address added. Each line is WIRE-escaped; the message is still not
shown. A panic that cannot unwind at all and follows no other (an undefined-behaviour
check that a build with debug assertions compiles in) on the command's own thread
prints the text, and then the process aborts. Only a build with `mds-cli`'s
never-shipped `debug-panics` feature (`SECURITY.md`) prints the message and location,
after the text. A build with debug assertions — `cargo install --debug`, or a profile
that turns them on — also holds a test-only trigger that panics on purpose when
`MDS_TEST_PANIC` is `main`, `thread`, `compile:<file stem>`, `notify` or `ctrlc`; a
release build compiles it out unless its profile turns debug assertions on.

The `mds::syntax`-through-`mds::formatter_invariant` rows correspond one-to-one to the
`MdsError` variants in `crates/mds-core/src/error.rs`; the last four are synthesised
by the bindings and do not exist in `mds-core`.

---

## 6. Scoping Rules

1. **File scope**: frontmatter vars visible everywhere in that file
2. **Runtime override**: `--vars` JSON values override frontmatter vars of the same name
3. **Block scope**: `@for` loop vars scoped to their `@for...@end` block
4. **Function scope**: params scoped to function body, shadow outer vars
5. **Import scope**: namespaced (aliased) or merged (unaliased), never implicit leaking
6. **Shadowing**: inner scope wins, no warning (intentional override). Teams that want visibility into shadowed variables can enable the opt-in `shadow-variable` lint (info severity, default-off) in `mds.json`.

---

## 7. CLI Interface

### 7.1 Commands

| Command | Purpose |
|---------|---------|
| `mds build [FILE\|DIR]` | Compile an `.mds` template (or a directory of templates) |
| `mds check [FILE\|DIR]` | Validate a template or directory without rendering |
| `mds fmt [FILE\|DIR]` | Auto-format `.mds` templates in place (safety-gated) |
| `mds lint [FILE\|DIR]` | Static-analysis lint of `.mds` templates |
| `mds init [FILENAME]` | Create a starter `.mds` file |

### 7.2 `mds build`

Output extension is **intrinsic**: markdown templates → `.md`; messages templates → `.json`.

```bash
mds build                                  # Auto-detect single .mds in current dir
mds build template.mds                     # Markdown → template.md; messages → template.json
mds build template.mds -o output.md        # Compile to a specific path (warns if ext contradicts kind)
mds build template.mds -o -               # Compile to stdout (kind-appropriate bytes)
mds build template.mds --out-dir dist      # Markdown → dist/template.md; messages → dist/template.json
mds build template.mds --vars vars.json    # With variable overrides from JSON file
mds build template.mds --set name=Alice    # Set a single variable
mds build template.mds --set name=Alice --set count=3  # Multiple variables
mds build template.mds --source-map        # Generate a source-map sidecar (.md.map)
echo "Hello {{name}}!" | mds build -         # Compile from stdin → stdout
mds build src/                             # Compile every non-partial .mds in the tree (next to source)
mds build src/ --out-dir dist              # Mirror subtree: src/a/b.mds → dist/a/b.md (or .json)
```

**Directory mode** (`mds build <dir>`):

- Compiles every non-partial `.mds` file in the tree (recursively).
- `_`-prefixed files are partials and are skipped (not compiled to output).
- Symlinked files and symlinked directories inside the tree are skipped.
- The directory argument itself is checked before the walk, by the same rule in `mds build`, `mds check`, `mds fmt`, `mds lint` and `mds watch` (#413), each failure `mds::io`, exit 2, naming the directory as typed: one whose final component is a symlink — typed with or without a trailing `/` or `/.` — is refused (`directory argument must not be a symlink: "<dir>"`); the filesystem root as the directory to walk is refused, whatever spelling reaches it — `/`, `/..`, or `.` in a working directory of `/` (`directory argument must not be the filesystem root: "<dir>"`) — while a root working directory still serves `-` (stdin) and a file argument (#371); and a forbidden path character in it, as typed or in its canonical form, is refused (§4.6). `.`, `./`, `..` and `sub/..` are the directory they resolve to, and `link/..` — which names no link — is accepted as the directory the operating system resolves it to (on Unix, the one above the link's target). `mds watch` walks the directory argument as typed for as long as it runs, so before every compile below it, it checks that the argument still resolves to the directory it watches; once it does not — a symlink on it, such as the link of `link/..`, retargeted — the rebuild is refused (`mds::io`, `watched directory now resolves to a different directory: "<dir>"; restart mds watch to follow it`) and nothing is written. It checks each source's own path the same way: once a directory below the argument is replaced by a symbolic link — which the walk skips, but a rebuild of a source it already knows would follow — that source's rebuild is refused (`mds::io`, `watched file now resolves to a different file: "<dir>/<path below it>"; restart mds watch to follow it`) and nothing is written. When the working directory `mds watch` was started in is deleted and recreated at the same path, it moves back into it before the next rebuild, so `.`, like any path typed relative to it, resolves again.
- Output extension per file is intrinsic (`.md` or `.json`).
- With `--out-dir <out>`, mirrors the source subtree under `<out>/`; without it, writes next to source. A source that is not under the build root (not reachable for a walked tree; defence in depth) is written flat as `<out>/<stem>.<ext>` with a warning naming both paths; the warning is not suppressed by `--quiet`.
- `-o` is rejected for a directory input.
- Continue-on-error: all compilable files are attempted; a summary (`N built, N failed`) is printed when any file fails or when `--quiet` is not passed. The run exits 1 when any file failed, and 2 when a failure among them is in the exit-2 class of §7.9 — the I/O and file-system errors (`mds::io`, `mds::file_not_found`, `mds::not_mds`) — whether it is the file's own, such as a source that cannot be read, a path carrying a forbidden character or an `@import` of a file that does not exist, or its output's: an output or `.map` sidecar that could not be written, or an output directory that could not be created. Each is reported and counted as failed (§5 "Streams and I/O failures", #157). A file that fails with a template error or a resource limit is counted as failed and leaves the exit at 1. `mds check <dir>` and `mds fmt <dir>` follow the same rule. Under `--quiet`, the summary is suppressed on a fully-successful run and emitted when any file fails, so the non-zero exit is never unexplained.
- A file whose path below the directory argument carries a forbidden path character (§4.6) — in its own name or in a subdirectory of the walk — is still collected, and fails on its own (`mds::io`, the name shown escaped) while its siblings are processed; `mds check`, `mds fmt`, `mds lint` and `mds watch` in directory mode do the same. `mds build`, `mds check`, `mds fmt` and `mds lint` then exit 2, and `mds watch` keeps running.
- When the directory contains no `.mds` files at all, exits 1 with `no .mds files found in <dir>; nothing was built` on stderr — emitted even under `--quiet`, like the all-excluded diagnostic — so an empty tree cannot pass a CI gate silently. (Changed in v0.4.3; previously exited 0.) `mds watch <dir>` is unaffected: it starts on an empty tree and compiles files created later.
- When the directory contains `.mds` files but every one of them is a `_`-prefixed partial, exits 1 with `<n> .mds file(s) found in <dir> but all are _-prefixed partials; nothing was built` on stderr — emitted even under `--quiet`, the same bypass as the two diagnostics above. (Changed in v0.4.3; previously `0 built, 0 failed`, exit 0.) `mds fmt <dir>` and `mds lint <dir>` are unaffected: they format and lint partials, so a partials-only tree is real work for them. `mds watch <dir>` is unaffected: it still starts.
- **Stale-flip cleanup**: when a file's kind changes (e.g., markdown → messages), the old-extension sibling (`.md` or `.json`) is removed automatically. A sibling that cannot be removed is an error, not a warning: it is reported as `mds::io` and the run exits 2, though the file itself was built and is not counted as failed (#157).
- stdin (`mds build -`) with `--out-dir`: the fallback output name is `output.md` (markdown) or `output.json` (messages).

**Options:**

| Option | Description |
|--------|-------------|
| `-o, --output <PATH>` | Output file path, or `-` for stdout. Mutually exclusive with `--out-dir`. Rejected for directory input. Warns if the extension contradicts the template kind — once the write is certain, so not for an output refused as the entry file itself (#425). A path carrying a forbidden path character (§4.6) is refused before any input is read (`mds::io`, exit 2). A relative path is resolved against the working directory, so when that cannot be determined — deleted while the shell is still in it, say — it is refused before any input is read (`mds::io`, exit 2, `cannot determine current directory: <reason>`, #390); so is `-o -` with `--source-map --inline`, whose map's `sources` are relative to the working directory. A value that is not valid UTF-8 is refused before any input is read, as `--out-dir` is (`mds::io`, exit 2, `-o/--output is not valid UTF-8: "<value>"`, #390). |
| `--out-dir <DIR>` | Output directory. Mirrors subtree (dir mode) or writes `<stem>.<ext>` inside it (file mode). Created if absent, when the output is written — never for an output refused as the entry file itself (#425). A path carrying a forbidden path character (§4.6) is refused before any input is read (`mds::io`, exit 2), as is a path that is not valid UTF-8 (`--out-dir is not valid UTF-8: "<value>"`, #390) and a relative path when the working directory cannot be determined (`cannot determine current directory: <reason>`, #390) — never resolved against `.` instead. |
| `--vars <FILE>` | JSON file with runtime variable overrides. A key repeated at any depth warns with its dotted/bracketed path (e.g. `x.a`, `x[2].a`); the last value wins. |
| `--set KEY=VALUE` | Set a single variable. Repeatable. Values are coerced to boolean, number, null, or array when possible. Repeating a key emits a warning; the last value wins. |
| `--set-string KEY=VALUE` | Set a single variable as a **string**, bypassing type coercion. Repeatable. Use when the value must remain a string (e.g. a numeric-looking ID). Repeating a key emits a warning; the last value wins. |
| `--source-map` | Generate a source-map sidecar (`<output>.map`, e.g. `-o out.md` → `out.md.map`). Ignored for messages-mode templates (a warning is emitted and no source map is produced). Conflicts with `--no-source-map`. Also enabled globally via `build.source_map = true` in `mds.json`. |
| `--no-source-map` | Disable source-map generation. Overrides `build.source_map = true` in `mds.json`. Conflicts with `--source-map`. |
| `--inline` | Embed the source map as a data-URI comment in the compiled output instead of a sidecar. Requires `--source-map`. |
| `--embed-sources` | Embed source file contents in `sourcesContent[]`. Ships full source text — use with care. Requires `--source-map`. |
| `-q, --quiet` | Suppress status messages on stderr on a successful run. The directory-mode summary is suppressed on a fully-successful run; it is still emitted when any file fails. (The directory-depth warning is emitted regardless of `--quiet`. A stale sibling or sidecar that cannot be removed is an error, and `--quiet` never suppresses an error.) |

**Output path resolution** (precedence order, highest first):

1. `-o -` → stdout
2. `-o <path>` → exact path (extension determined by caller; compiler warns on mismatch)
3. Stdin input with no `-o`/`--out-dir` → stdout
4. `--out-dir <dir>` → `<dir>/<stem>.<ext>` (file mode) or `<dir>/<rel/path>.<ext>` (dir mode)
5. `mds.json` `build.output_dir` → `<config_dir>/<output_dir>/<stem>.<ext>`
6. Default → `<source_dir>/<stem>.<ext>`

In all paths, `<ext>` is `md` for Markdown templates and `json` for messages templates.

**Status lines.** `Compiled to`, `Source map written to` and `Removed stale map` name each file as the user named it, never by a canonical or absolute path they did not type (#390). Under route 4 the output is named below `--out-dir` exactly as typed — `mds build src --out-dir out` prints `Compiled to out/a/b.md`, an out-dir reached through a symlink is named by the link, and an absolute one is shown as typed; in directory mode without it, below the directory argument as typed (`src/a/b.md`). Under route 5 it is named below the directory the input reached `mds.json` by, one `..` per step up, as a config error names `mds.json` itself: `mds build page.mds` prints `Compiled to ./dist/page.md`, `mds build src` with `mds.json` beside `src` prints `Compiled to src/../dist/b.md`. Routes 2 and 6 name the output by the path they build from what was typed (`-o out.md` → `out.md`, `mds build page.mds` → `./page.md`). The warning that `build.source_map` has no effect when writing to stdout names that `mds.json` the same way (`warning: source_map in ./mds.json has no effect …`), and an output directory that cannot be created is named as its output is. `mds watch` announces the entry and the directory argument as typed — `Watching page.mds`, `Watching directory .`, an auto-detected entry by its file name — though it matches the events it watches for against their canonical paths, and names each output the same way in these lines: the startup `Compiled to`, a rebuild's `Recompiled` (`Recompiled ./page.md (0 deps) in 3ms`; `Recompiled <stdout> …` under `-o -`) and, in directory mode, a deleted source's `Removed <output> (source deleted)` and the warning that it could not be removed. The text of an error writing an output, and of the error or warning that a stale output of the other kind could not be removed, names the output as these lines do, in `mds build` and `mds watch` alike, and the cause after it names no path (§7.10). An output beside its source is named beside the entry as typed in file mode (`mds watch page.mds` → `./page.md`) and below the directory argument as typed in directory mode (`mds watch .` → `./a/b.md`). The file name file mode derives from the entry — beside it, below `--out-dir` or below `build.output_dir` — is the entry's as the filesystem resolves it, which can differ from the name as typed, and so from the one `mds build` derives, for an entry typed in another case than its name on a case-insensitive volume or by a Windows short name (`mds watch PAGE.mds` → `./page.md`, where `mds build PAGE.mds` prints `./PAGE.md`). The table in §7.10 lists these lines with the tests that pin each.

A route that fails to resolve — route 5 with a `build.output_dir` containing `..` (§7.8) — is refused by `mds build` and `mds watch` alike (`mds::io`, exit 2, `mds.json output_dir '<dir>' must not contain '..' components`), and nothing is written. `mds watch` resolves its route once, at startup, and every rebuild writes where it resolved, so it refuses such a route at startup, in file and directory mode — in file mode whether or not the startup compile succeeds (a failed compile is reported before the refusal) — and never falls back to stdout: under `mds watch`, only `-o -` writes to stdout. A relative `-o` or `--out-dir` is resolved against the working directory, which must exist: when it cannot be determined, `mds build` and `mds watch` alike refuse the run before any input is read (`mds::io`, exit 2, `cannot determine current directory: <reason>`) — `mds watch` at startup, in file and directory mode, where it used to report the failed write and fail again on every rebuild — and nothing is written; the location is never resolved against `.` instead (#390).

Whichever route resolves it, an output that is the entry file itself is refused before anything is written or any directory created — `mds::io`, exit 2, `output would overwrite the entry file: "<entry>"; write it elsewhere with -o <file> or --out-dir <dir>`, the entry named as typed (#425) — and without the `-o` extension-mismatch warning, which announces a write. The two paths are compared as the files they name, canonical with canonical, so another spelling of the entry — through `..`, a case variant on a case-insensitive volume, or a symlinked directory leading back to it — is the entry too. An output whose directory does not exist yet is resolved as the write will create it: a created directory is a plain one, so `-o newdir/../page.md` is `page.md`, and a symlink after the `..` is followed; a symlink at the output path itself is refused by the write (below), and a hard link to the entry is another name the write replaces by rename, so it is written and the entry keeps its content. Route 6 reaches the entry for a `.md` entry that declares `type: mds` and compiles to Markdown, as do routes 4 and 5 naming the entry's directory; route 2 can name any entry. `mds watch` in file mode refuses the same output at startup (exit 2) and, on a rebuild, reports the refusal and keeps watching. Directory mode compiles only `.mds` files, whose outputs are `.md` or `.json`, so no output there is its own entry.

**Output writing.** Compiled outputs and `.map` sidecars written by `mds build` and `mds watch`, `.mds` sources rewritten by `mds fmt` and `mds lint --fix`, and the starter file written by `mds init`, are written to a temporary file in the target's directory and then renamed over the target, after a final symlink re-check of the target: a crash, kill or full disk never leaves a truncated file behind, and a destination path that is a symlink is refused. Because the output is created as a sibling temporary file and renamed into place, the destination must be a regular-file path inside a writable directory: device files such as `/dev/null` and FIFOs are not supported as `-o` targets (write to stdout instead). Source rewrites (`fmt`, `lint --fix`) are additionally fsynced before the rename; compiled outputs and sidecars rely on the rename alone — they are regenerable, and an unconditional fsync made directory-mode startup several times slower on macOS. Because the rename gives the target a new inode, a pre-existing target's hard links (other links keep the old content), ACLs, extended attributes, and owner/group are not preserved; permission bits are preserved on Unix. This is enforced for the CLI's write sites by `crates/mds-cli/tests/write_funnel.rs`. A write that fails, a symlink refused at the destination, an output directory that cannot be created, and a stale `.map` sidecar that cannot be read or removed are `mds::io`, exit 2 (#157). Output to stdout (`-o -`, or stdin input with no `-o`) is written and flushed at once: a reader that has gone away (a closed pipe) ends it without an error and the exit code stands; any other stdout failure is `mds::io`, exit 2.

### 7.3 `mds check`

```bash
mds check                                  # Auto-detect single .mds in current dir
mds check template.mds                     # Validate a specific file
mds check template.mds --set name=Alice    # Validate with variable overrides
echo "@if flag:" | mds check -             # Validate from stdin
mds check src/                             # Validate every non-partial .mds in the tree
```

Exits 0 if all templates are valid, non-zero on any error. Same `--vars`/`--set`/`--set-string`/`--quiet` options as `mds build`. Directory mode follows the same semantics as `mds build <dir>` (partial skipping, symlink rejection, continue-on-error, and the three nothing-to-process exits — empty tree, all-excluded, and partials-only — which exit 1 with `…; nothing was checked`) but does not write any output files. In directory mode the summary line is `N passed, N failed`, emitted under the same `--quiet` rule as `mds build <dir>` (§7.2): suppressed on a fully-successful run, emitted when any file fails.

### 7.4 `mds fmt`

```bash
mds fmt template.mds                       # Format a single file in place
mds fmt src/                               # Format all .mds files under src/
echo "template content" | mds fmt -       # Format from stdin, write to stdout
mds fmt template.mds --check              # Exit non-zero if file would change
mds fmt template.mds --diff               # Print unified diff without writing
```

Formats `.mds` templates: normalizes CRLF to LF (everywhere, including inside frontmatter and code fences), strips trailing whitespace on directive lines, and ensures exactly one trailing newline. An empty or whitespace-only source formats to 0 bytes (an empty output file). Interior blank lines and blank-line structure within frontmatter and code fences are left verbatim (blank-line collapsing was removed in v0.4.0 to preserve the interior-verbatim whitespace contract). Body-text trailing whitespace (Markdown hard breaks) and the byte-for-byte content of `@message`/`@define` bodies are left untouched.

Every rewrite is **safety-gated**: the formatter re-compiles both the original and formatted sources and refuses to write if compiled output would change (`mds::formatter_invariant`), so a formatting bug can never corrupt a template. The base directory the gate compiles against (the file's directory; the working directory for stdin) is resolved first, as `mds check` resolves it: one that is refused (§4.6) or cannot be resolved fails `mds fmt` with that error (`mds::io`, exit 2).

A rewrite that fails is `mds::io`, exit 2; in directory mode the other files are still formatted, the file counts as failed, and the run exits 2, as it does for a source that cannot be read (§7.2 continue-on-error). `--diff` output and stdin filter-mode output follow §5 "Streams and I/O failures": into a closed pipe they end without an error and the exit code stands (`--check` still exits 1 when a file would change), and any other stdout failure is `mds::io`, exit 2 (#157). In directory mode that failure is reported once while stdout keeps failing, and every file whose diff it lost counts as failed.

| Option | Description |
|--------|-------------|
| `--check` | Exit non-zero without writing if any file would change. |
| `--diff` | Print a unified diff of proposed changes without writing. |
| `-q, --quiet` | Suppress per-file status messages and the directory summary on a successful run. The summary is still emitted when any file fails to format. Exception: under `--check`, a run where files would reformat but none failed exits 1 with no summary — the would-reformat count is treated as status output and is suppressed by `--quiet` (mirrors the same rule for `mds lint --fix --check`). (Three notices bypass `--quiet`: the directory-depth warning, the all-files-excluded diagnostic, and the empty-tree diagnostic `no .mds files found in <dir>; nothing was formatted` — both diagnostics exit 1.) |

### 7.5 `mds lint`

```bash
mds lint                                   # Auto-detect single .mds in current dir
mds lint template.mds                      # Lint a single file
mds lint src/                              # Lint all .mds files recursively (incl. partials)
mds lint --fix template.mds                # Auto-fix fixable issues in place
mds lint --fix --check template.mds        # Preview --fix: exit = max(1, residual severity); never writes
mds lint --fix --diff template.mds         # Preview --fix: print unified diff without writing (same exit rule)
mds lint --format json template.mds        # Machine-readable JSON output (stdout)
mds lint --quiet template.mds              # Suppress warnings; exit 2 on errors only
cat template.mds | mds lint -             # Lint from stdin
cat template.mds | mds lint --fix -       # Fix from stdin, write fixed source to stdout
```

**Channel discipline:**
- Human-readable diagnostics → **stderr** (via miette).
- `--format json` output → **stdout** (single JSON object, one trailing newline).
- Directory-mode summary → **stderr** (in both human and JSON format modes; stdout remains a single clean JSON document in JSON mode, except under `--fix --diff` which also writes the unified diff to stdout).
- `--quiet` suppresses warning-severity and info human diagnostics, NOT errors.

**Options:**

| Option | Description |
|--------|-------------|
| `--fix` | Apply auto-fixable issues in place. Tier A fixes apply always; Tier B fixes apply only to standalone (non-importing) files. |
| `--check` | With `--fix`: never writes; exit is `max(1, residual severity)` — 1 when a fix is pending and the post-fix residual would be clean or warn-only, 2 when findings the fix cannot remove would remain at error severity. The emitted diagnostics stay pre-fix (the preview reports what is wrong now); only the exit code looks through to the residual. Useful for CI. |
| `--diff` | With `--fix`: print unified diff of pending changes without writing. Exit follows the same `max(1, residual severity)` preview rule as `--check`. |
| `--format <FORMAT>` | Output format: `human` (default, stderr) or `json` (stdout). |
| `--vars <FILE>` | JSON file with runtime variable overrides (forwarded to the check gate). A key repeated at any depth warns with its dotted/bracketed path; the last value wins. |
| `--set KEY=VALUE` | Set a single variable. Repeatable. Type coercion applies. Repeating a key emits a warning; the last value wins. |
| `--set-string KEY=VALUE` | Set a single variable as a string, bypassing type coercion. Repeatable. Repeating a key emits a warning; the last value wins. |
| `-q, --quiet` | Suppress warning/info human diagnostics and the directory summary on clean/warn-only runs; errors still print and the summary still appears when error- or resource-limited files are present. (The directory-depth warning — fires on trees deeper than MAX_DEPTH=64 — is emitted regardless of `--quiet`.) |

**Directory mode** (`mds lint <dir>`):

- Lints every `.mds` file recursively (including `_`-prefixed partials).
- Accumulate-and-continue: per-file errors do not abort the run.
- Each file's nearest `mds.json` loads before the file is read, in either format, as for
  a file argument: a configuration that cannot load is the file's failure (`mds::io`,
  "with errors") even when the file is also unreadable or over the size cap (#309).
- Before any file is linted, every entry is named relative to the lint root; a path that is not valid UTF-8 or that escapes the lint root is an I/O error (`mds::io`, exit 2) for the whole run — no lossy or absolute `file` key is ever emitted. The message names the path escaped as a status line names one, so a newline or other forbidden character in the name shows as its `\uXXXX` literal (#390).
- After processing all files, emits one summary line to stderr:
  `N clean, N with warnings, N with errors, N resource-limited`
  Each file falls in exactly one bucket, so the four counts always sum to the number of
  `.mds` files the walker collected. A `--fix --check` or `--fix --diff` preview counts each
  file as its fix would leave it: a file whose pending fix would clear every finding counts
  as clean, though the preview shows the pending fix (`Would fix:` under `--check`, the
  diff under `--diff`) and the file's own findings (#309).
  - "Clean" — no findings.
  - "With warnings" — warning-severity findings only.
  - "With errors" — error-severity lint findings **or** a per-file analysis failure
    (source read, config load, or lint call failure) **or** a failure of the file's own
    output: a `--fix` rewrite that fails, or a `--fix --diff` diff lost to a stdout that
    fails other than by a closed pipe (§5 "Streams and I/O failures"; `mds fmt <dir>`
    counts such a file as failed, §7.4). These populations are deliberately merged,
    matching the way `mds build`'s "failed" count merges them.
  - "Resource-limited" — files refused with `mds::resource_limit`: a source over the
    10 MiB file cap, in every mode and format, or one over another documented limit (for
    example, exceeding `MAX_BLOCKS_PER_MODULE`). These are counted here, never under
    "with errors"; the file's failure is reported as the entry's, and the directory exits
    3. Before v0.5.0, `--format human` counted a source over the file cap under "with
    errors" and exited 2 (#309).
- Under `--quiet`, the summary is suppressed when the worst outcome is warnings only
  (mirrors `mds fmt`'s contract).  When any file is in the error or resource-limited
  bucket, the summary is always emitted so the non-zero exit is never unexplained.
  **Exception:** `mds lint --fix --check --quiet <dir>` exits 1 with zero
  stderr bytes when pending fixes exist but no file is in the error or
  resource-limited bucket — the `--fix --check` pending-fix signal is treated as
  status output and is suppressed by `--quiet` alongside the summary line.
- A directory with no `.mds` files at all exits 2 (the usage-error code) with
  `no .mds files found in <dir>; nothing was linted` on stderr; a directory whose
  every `.mds` file lies under a default-excluded directory exits 2 with a
  diagnostic carrying the skip count. Both bypass `--quiet`, print no summary
  line, and write nothing to stdout even under `--format json`.
- The JSON stdout envelope (`{"files":…,"truncated":…,"version":1}`) is unchanged
  regardless of `--quiet` or directory mode — no `"summary"` key is added.

**`--quiet` and `--fix` status messages:** `fix rejected: <reason>`, `Partially fixed:`,
`Would fix:`, and the `diagnostic cap (N) reached` notice are status output gated by
`--quiet` in **all three input modes** (directory, single file, stdin). `Fixed: <path>` is
gated by `--quiet` in single-file and directory modes (both `--format human` and `--format
json`); stdin writes fixed source to stdout rather than writing back to a file, so no
path-bearing `Fixed:` line appears there. All of these status messages go to **stderr**
regardless of `--format`; the JSON stdout envelope is unaffected. Error-severity diagnostics
and the exit code are unaffected by `--quiet`.

The `diagnostic cap (N) reached` notice is printed when an input's own findings stop at the
cap, in a report as well as under `--fix`, `--fix --check` and `--fix --diff`, before
anything else about that input — a directory's entry prefixed with its path (#309). It
describes the findings the input was linted with, so it also prints when `--fix` clears the
capped set and `"truncated"` is `false`. A report and a preview print
`diagnostic cap (1000) reached; further findings were suppressed`; under `--fix`, which
writes its fix, the notice adds `— re-run --fix to continue`, since a re-run lints the fixed
source past the findings the cap stopped.

**A `--fix` rewrite that fails** is `mds::io`, worded `cannot write <file>: <cause>` for a
file argument and for a directory's entry, in either format: the file once, as its
`Fixed:` line would name it, then the cause, which names no path — whichever step of the
write failed (#309). It counts under "with errors" and the run exits 2. A human report
shows the findings the fix would have left, framed over the source it could not write, and
then the error. Under `--format json` the failure is the input's one record and no
findings are listed for it: a directory's entry is `{"file":…,"error":…}`, and a file
argument's document is the error envelope `{"version":1,"error":…}` — never the success
envelope of a fix that was not written.

**Names in status lines:** `Clean:`, `Fixed:`, `Partially fixed:` and `Would fix:` name a
file argument as typed — `mds lint docs/page.mds` prints `Clean: docs/page.mds`, and an
absolute argument is shown as typed — and a directory's entry by the walk's path below the
directory argument as typed (`src/a.mds`); stdin is `<stdin>` (#390). `Clean:` is printed
for a file argument's clean human report only, never for stdin or a directory's entries,
and `--quiet` suppresses it. The JSON `file` key is unchanged: a file argument's file name,
a directory entry's path relative to the directory argument. The table in §7.10 lists these
lines with the tests that pin each.

**Exit codes** (lint-specific; differ from `mds build`/`mds check`):

| Code | Meaning |
|------|---------|
| `0` | Clean — no warning- or error-severity findings (`info` findings never raise exit code) |
| `1` | Warning-severity findings only (no errors) |
| `2` | Any error-severity finding, analysis failure (parse/resolve/IO/config), or usage error |
| `3` | Resource limit exceeded |
| `101` | Internal compiler error: a panic (§5 "Panics on the CLI", #389) — wins over every other code |

With `--fix`, residual post-fix findings determine the exit code.

**Config discovery in directory mode**: when linting a directory, `mds lint` locates
the nearest `mds.json` by walking up from **each input file** independently (cached
per directory, so a shared parent is not re-read). This means nested subdirectories
can each carry their own rule overrides. A malformed config for one subtree produces
a per-file error entry (with no diagnostics) and contributes to exit code 2 without
aborting analysis of the rest of the tree. This differs from `mds build` directory
mode, which uses a single config located from the directory argument.

**JSON output format** (`--format json`):

```json
{
  "files": [
    {
      "diagnostics": [
        {
          "fix_edits": null,
          "fixable": false,
          "help": "Remove the frontmatter key or reference it in the template body.",
          "message": "Variable 'foo' is defined in frontmatter but never referenced in the body.",
          "rule": "unused-variable",
          "severity": "warn",
          "span": { "length": 3, "offset": 4 }
        }
      ],
      "file": "template.mds"
    }
  ],
  "truncated": false,
  "version": 1
}
```

Keys are in alphabetical order (BTreeMap serialization). Within each `files[].diagnostics` array, diagnostics are ordered by ascending `span.offset`; span-less diagnostics sort last; equal-offset ties preserve rule-execution order (stable sort). (The CLI and binding surfaces always produce results through `LintResultBuilder`; a `LintResult` assembled directly via `LintResult::new` preserves caller-supplied order instead.) In directory mode, `files[].file` is the forward-slash-separated path relative to the lint root (e.g. `src/template.mds`), and the `files[]` array is ordered by the byte-wise string comparison of that relative display path (e.g. `api-utils.mds` sorts before `api/x.mds` because `'-'` (0x2D) < `'/'` (0x2F)). `"truncated": true` when the findings an input is left with were capped by the per-file diagnostic cap of 1,000 (#309): its own findings in a report, the findings `--fix` leaves, and under `--fix --check` / `--fix --diff` the findings the fix would leave — so a preview whose fix would clear a capped set of fixable findings reports `false` while it lists the input's own capped findings. A directory's document is `true` when that holds for any of its entries; an entry recorded as an error carries no findings and does not set it. `"span"` is JSON `null` for diagnostics that lack a source location. When linting from stdin (`mds lint -`), `files[].file` is `"<stdin>"`.

**`lint_warnings` field (binding surfaces only):** The napi, WASM, and Python binding surfaces include an optional top-level `"lint_warnings"` key in the returned result object when non-fatal warnings were produced during linting (for example, unknown rule names in `mds.json`). In the JSON wire form (napi, WASM, and Python `to_dict()` / `to_json()`) the key is absent (not `null`, not `[]`) when no warnings occurred; on the Python live-object surface, `LintResult.lint_warnings` is a property that always exists and returns an empty list when no warnings occurred. In alphabetical key order `"lint_warnings"` sorts between `"files"` and `"truncated"`. The CLI does **not** include `"lint_warnings"` in its `--format json` stdout envelope — it writes warnings to stderr so the JSON stdout remains valid and parseable without modification.

A file that produces a per-file analysis failure in directory mode (malformed config, I/O error) emits a `{"file":"…","error":{"code":"…","message":"…","help":"…","span":…}}` entry without a `"diagnostics"` key and contributes to exit code 2. A file whose analysis panicked emits the same entry with the error `{"code":"mds::internal","message":"internal compiler error","help":null,"span":null}`, counts under "with errors", and the run exits 101 (§5 "Panics on the CLI"). When a stdin source fails the check gate before linting begins, the CLI emits an analysis-failure envelope to stdout: `{"version":1,"error":{"code":"…","message":"…","help":"…","span":…}}`. This envelope carries no `"files"` or `"truncated"` key, and no `"file"` key (unlike the success envelope above). A file argument whose `--fix` rewrite fails gets the same envelope with its `mds::io` error; a directory's entry whose rewrite fails is the error entry above (#309). A JSON consumer MUST handle both the success envelope and the analysis-failure envelope and MUST NOT assume a `"file"` key is present in error results.

#### Sanitization invariant (v1)

Under `"version": 1`, the following guarantees are normative. The prior behavior of
passing raw control bytes through to JSON is superseded.

The **escaped class** is:

| Codepoints | Why |
|------------|-----|
| C0 (U+0000–U+001F) except `\t` (U+0009) | Terminal escape-sequence injection (CWE-150) |
| `\n` (U+000A) | Line forging in any consumer that prints or line-splits the value |
| DEL (U+007F) | Interpreted as a destructive backspace by some terminals |
| C1 (U+0080–U+009F) | Terminal control, incl. NEL (U+0085) |
| U+061C, U+200E, U+200F, U+202A–U+202E, U+2066–U+2069 | The complete Unicode `Bidi_Control=Yes` set (12 codepoints) — they visually reorder the line (Trojan Source, CVE-2021-42574). U+061C ARABIC LETTER MARK is the only member outside U+200E–U+2069. |
| U+2028, U+2029 | Terminate a JavaScript string literal |
| U+FEFF | Invisible BOM / ZWNBSP — hides or splits content |

Each is replaced with its six-character `\uXXXX` literal (uppercase hex) before
serialization. `\t` (U+0009) is the sole exemption from the C0 range: it is never
escaped, in either mode.

| Field | Invariant |
|-------|-----------|
| `message`, `help` | Every codepoint in the escaped class above is replaced with its six-character `\uXXXX` literal before serialization. |
| `file` | Sanitized on the same pass as `message`/`help`. Hostile filenames cannot inject control, bidi, or separator characters into this JSON output. A filename occupying one of the **diagnostic** `file` fields — this JSON key, a CLI status line, or a `[file:line:col]` frame header — is escaped with the **full** class including `\n` on each of those, human surfaces included, because it is always rendered on a single line and POSIX permits a newline inside a filename. Two path positions are outside that rule and are **not** escaped: a path interpolated into a diagnostic *message body*, which is prose (see "Residual" below), and a path in a source map or in `CompileResult.dependencies`, which is a functional reference (see "Carve-out" below). |
| `rule` | Fixed ASCII identifier; never contains control bytes by construction. Not sanitized. |
| `lint_warnings` | Binding-surface-only field (absent from the CLI's `--format json` stdout). Each element is a human-readable warning string whose interpolated user-supplied values (rule names from the caller's `rules` option) are WIRE-escaped via the full escaped class during construction, before the string is formed. The surrounding template text is static ASCII and contains no codepoints in the escaped class. |
| `span`, `fix_edits[].start`/`end` | **Raw byte offsets** into the source the finding was linted from — deliberately not sanitized. These are numeric position values and index that source's bytes, not an escaped rendering of them. For a report or a preview that source is the input as read; a finding `--fix` leaves was linted from the fixed source, so its offsets index the fixed source (#309). |
| `fix_edits[].new_text` | WIRE-sanitized. This field is a **display preview** of the replacement text; consumers MUST NOT apply it as a patch payload. Applying fixes is `mds lint --fix`, the functional path, which reads raw bytes directly from the internal `LintDiagnostic` struct and never serializes via this JSON field. |

This invariant applies across all surfaces that emit `"version": 1` JSON: CLI
(`mds lint --format json`), napi (`lintVirtual` / `lint` / `lintFile`), WASM
(`lintVirtual` / `lint`), and Python (`lint_virtual` / `lint` / `lint_file`).
All four surfaces emit byte-identical values on the fields they share, with two exceptions. First, `"lint_warnings"` is a binding-surface-only key: it is absent from the CLI's `--format json` output (the CLI writes unknown-rule warnings to stderr instead). Second, the `"file"` key takes different values by surface and input method: `mds lint -` (CLI) relabels `"input.mds"` to `"<stdin>"` at the output boundary; the string-source `lint()` entrypoint on napi, WASM, and Python retains `"input.mds"`; `lintVirtual` (napi/WASM) and `lint_virtual` (Python) emit the caller-supplied entry key instead. The fields `message`, `help`, `rule`, `severity`, `span`, and `fix_edits` share the same Rust serializer across all surfaces and are designed to be byte-identical, but no live cross-surface differential test currently compares these fields between the CLI and binding surfaces on a source that produces findings — the existing parity test uses a clean source with no diagnostics. For source maps, the same stdin-relabeling asymmetry applies: `mds build -` (CLI) replaces the internal `"input.mds"` entry with `"<stdin>"` in `sources[]`; binding surface string-source compiles carry `"input.mds"` in `sources[]` (`sources[0]`, unless the string `@extends` a base: the chain's root base comes first, as in a compile of the same file), and virtual-FS compiles carry the caller-supplied entry key.

##### Mode is chosen per field, not per surface

The escape class above is fixed. The only thing that varies is whether `\n` is escaped
with it, and that choice is **normatively a property of the field, not of the output
surface**:

> **On the diagnostic surfaces — the `"version": 1` JSON wire, CLI status and warning
> lines, and `[file:line:col]` frame headers — untrusted identifiers, filenames, and
> error causes are escaped in WIRE mode, human terminal output included. Prose — a
> diagnostic message body or help body — is escaped in HUMAN mode on terminal surfaces,
> so that multi-line frames keep rendering.**

The rule governs *diagnostic* output. Two categories of output are named carve-outs and
are not escaped at all, because escaping them would destroy their function rather than
protect it: the command's **product** (compiled template output) and **functional path
references** (source-map `file`/`sources`, `CompileResult.dependencies`). Both are
listed in the table below and the second is specified under "Carve-out" further down.

The discriminator is whether the value is ever *legitimately* multi-line. A filename, a
config key, a `--format` argument, an `io::Error` cause, and a fix-rejection reason are
each displayed on exactly one line, so preserving a raw `\n` in one buys nothing and
lets it forge a standalone line that is byte-identical in form to genuine output
(CWE-117). A diagnostic body genuinely is multi-line, so escaping its newlines would
break the frame.

This rule supersedes any per-surface reading of the earlier "human escapes the class
minus `\n`" formulation: `\n` is escaped on all machine-readable boundaries listed
above, **and** on every identifier / filename / cause field of a diagnostic, on every
surface that renders one — human terminal output included. It says nothing about the
two carve-outs, which are not diagnostics.

Applied, that means:

| Value | Mode | Because |
|-------|------|---------|
| `message`, `help`, warning bodies, `LabeledSpan` text | HUMAN on terminal surfaces, WIRE on the JSON wire | Prose; legitimately multi-line in a rendered frame |
| A filename or path in a diagnostic `file` **field**: the JSON `file` key, a CLI status line, a `[file:line:col]` frame header | WIRE on every surface that renders one; a CLI status line also escapes `\t` (`mds::escape_path_for_message`, the whole forbidden-path class of §4.6) | Single-line by construction; POSIX permits `\n` in a filename and the user never types it |
| `mds.json` rule names and config values, the `rules` keys and severity values a binding's options error names (napi, WASM, Python), an unknown option key (napi, WASM and the `@mdscript/mds` wrapper), `--format` arguments | WIRE on every surface that renders one; mds-core words the `rules` error of all three bindings, and the CLI's `mds.json` `lint.rules` error, with `parse_rule_severities` (#418, #175), and each surface escapes the name, value or key identically | Single-line identifiers. WIRE applies in all rendering contexts — including when a rule name appears inside a warning body (e.g., the unknown-rule-names warning), the name is WIRE-escaped, not HUMAN. This row takes precedence over the residual row below for `mds.json`-sourced values. |
| `io::Error` / `MdsError` causes interpolated into a CLI status or warning line, and the conversion error a binding's `mds::invalid_options` message wraps (`invalid options.rules: …`, Python's `invalid vars: …`) | WIRE on every surface that renders one | Single-line, and they embed paths of their own — or, for a conversion error, text of the caller's own (a symbol description, a class name) |
| A path, identifier or cause interpolated into a diagnostic **message body** | HUMAN on terminal surfaces, WIRE on the JSON wire | Follows the message row above — it is part of prose. This is the **residual** below: it is not covered by the WIRE rows. Exception: `mds.json` rule names and config values that appear inside a warning body are governed by the WIRE row above, not this residual — the more specific row takes precedence. |
| Compiled template output (`mds build -o -`) | not escaped | It is the command's product, not a diagnostic; redirects must stay byte-faithful |
| Source-map `file` / `sources` / `sourcesContent`, and `CompileResult.dependencies` | not escaped | Functional references, not display text; escaping would break resolution. This is the **carve-out** below |

Source excerpts embedded in a rendered diagnostic frame are neutralized
byte-length-preservingly instead of escaped, so span offsets and caret columns stay
exact. The substitute is chosen per UTF-8 width: 1-byte C0/DEL → `?`; 2-byte C1 and
U+061C → U+00A0; 3-byte bidi controls, separators and BOM → U+FFFD.

On the CLI this is enforced at a single choke-point: every diagnostic printed to
stderr — compiler errors and CLI-authored errors alike — has its message, help, and
caret-label text escaped **before** the diagnostic renderer runs. The rendered frame is
never post-processed, so the renderer's own terminal styling is left intact and caret
columns stay aligned.

##### Residual: paths and identifiers inside a message body

The rule above is per field, and a value interpolated into a diagnostic **message body**
is part of that body. It is therefore escaped in HUMAN mode on terminal surfaces, which
preserves `\n`. Two construction sites produce such messages:

- **CLI `miette::miette!()` messages**, which interpolate `mds.json` values and
  filesystem paths.
- **`mds-core` `MdsError` message bodies**, which interpolate `io::Error` causes — the
  operating system's own text, `{e}` in `cannot resolve path {path}: {e}` in
  `crates/mds-core/src/fs.rs` — and template identifiers (`invalid import alias:
  '{alias}'` in `parser_helpers.rs`, `'{name}' is not exported from '{path}'` in
  `resolver.rs`). The paths they name are checked first: an entry, import,
  base-directory or vars-file path carrying a forbidden character of §4.6 — `\n`
  among them — is refused, named escaped, before any other message can name it, and
  the native backend's module-read refusals (`cannot read`, `invalid UTF-8 in`,
  `file too large`) escape their path where they are built as well.

Both are **known residuals, not closed boundaries**, and they are the same defect at two
different construction sites. A hostile path or identifier containing `\n` survives into
the rendered frame and occupies a line of its own there.

The residual is a *weaker* surface than a status line, and deliberately so: everything
inside a rendered frame is indented and `│`-prefixed by the renderer, and that prefix
survives `strip()`, so forged frame content cannot masquerade as a bare CLI status line
the way an unescaped filename in a `Clean: …` line could. No raw control byte reaches
the terminal from either path — HUMAN mode still escapes the whole class except `\n`.

Closing it would mean WIRE-escaping every untrusted interpolation at every `MdsError`
and `miette!()` construction site — over a hundred in `mds-core` alone — and changing
the public `MdsError` message text seen by all three binding layers. That is a larger,
separately-specified change; until it is made, this section is the disclosure, not a
gap someone forgot.

##### Carve-out: functional path references (source maps, `dependencies`)

Source-map documents and `CompileResult.dependencies` are **explicitly outside** the
per-field rule. The paths they carry are emitted **verbatim** — no escaping, no
neutralization — in every one of these positions:

- the sidecar written by `mds build --source-map` (`<output>.map`): its `file` key,
  every entry of `sources`, and every entry of `sourcesContent`;
- the `sourceMap` object embedded in `CompileResult::to_canonical_json()`, and hence in
  the napi / WASM / Python compile results;
- the `dependencies` array of `CompileResult::to_canonical_json()`.

Emitted verbatim means not escaped. It does not mean byte-identical to the internal
module key: on Windows a native `dependencies` entry is the conventional spelling of the
canonical path (`C:\…`, not `\\?\C:\…`) wherever that names the same file — a lossless
respelling, not an escape (pinned on the Windows CI leg by
`windows_dependencies_carry_no_verbatim_prefix` in `crates/mds-core/tests/api_surface.rs`,
and on every platform by the `verbatim` unit tests in `crates/mds-core/src/verbatim.rs`).

The CLI's own **display text** — `file` per the per-field rule above — gets the same
respelling through a separate mechanism, `mds::display_native_path` (a no-op off
Windows): the diagnostic `file` field is WIRE-escaped for control/bidi/separator
characters as the table above describes, and independently of that, any path a CLI
status line or error message shows (an `atomic_write_file` I/O failure below a
canonicalized `--out-dir`, say) is passed through `display_native_path` before display, at the CLI's
`safe_path` choke-point. The two are orthogonal: escaping defends against a hostile
filename, `display_native_path` respells a canonicalization artefact that is not
hostile, just platform-specific.

These are **functional references, not display text**. Source Map v3 `file` and
`sources` are resolved against the filesystem by devtools, bundlers and IDEs;
`dependencies` is a watch/rebuild input for the bundler plugins. Rewriting a path to a
`\uXXXX` literal would produce a path that does not exist, breaking source-map
resolution and dependency tracking in order to defend against a pathological filename.
That is the same product-versus-display distinction that keeps compiled output
unescaped: escaping the artefact corrupts the artefact.

Consequently, and normatively:

> **Consumers of a source map or of `dependencies` MUST treat every path they contain
> as untrusted input.** A path produced by a custom `FileSystem` backend may contain any
> byte that backend's keys permit, including C0 control characters, `\n`, bidi controls
> and U+FEFF; the built-in backends refuse those characters at input (#265, below), but a
> consumer cannot tell which backend produced a map. A consumer that prints such a path
> to a terminal, writes it into a log line, or interpolates it into HTML must escape it
> for that destination itself. JSON string encoding is *not* that escaping: it makes the
> document parseable, and a decoded `"\n"` is a real newline again.

The CLI does not rely on this contract for its own output: the `Compiled to …` and
`Source map written to …` status lines print the path through `safe_path`, so they carry
the WIRE-escaped form even though the sidecar they name does not.

Since #265 the resolver, both built-in backends (`NativeFs`, `VirtualFs`), the CLI and
`@mdscript/mds`'s WASM-backend file pre-scanner MUST refuse every forbidden path
character (`mds::is_forbidden_path_char`: C0 including `\n` and `\t`, DEL, C1 and the
bidi/format hazards) at input — import strings with `mds::import`; entry paths, entry
keys, base directories, resolved canonical paths and the CLI's output locations with
`mds::io` — at the points listed in §4.6 "Forbidden-character enforcement". A path one
of the built-in backends resolves therefore carries none of them. The carve-out still
stands as written — the paths are emitted verbatim — and the MUST above still binds
consumers, because the one residual is a custom `FileSystem` backend passed to
`ModuleCache::with_fs`: the resolver refuses every caller-supplied path before calling
it, but a path the backend produces itself is checked only if that backend applies
`mds::is_forbidden_path_char`, as its contract requires.

##### Escaping is one-way

The transformation is **lossy and non-injective, by design**. A template that
literally contains the six characters `\`, `u`, `0`, `0`, `1`, `B` and a template
containing an actual ESC byte both serialize to the identical six-character
string `\u001B`;
after serialization they are indistinguishable.

Consumers **MUST NOT** un-escape `\uXXXX` sequences back into bytes. Doing so
reconstitutes exactly the injection this invariant prevents — an attacker who
controls a diagnostic message controls what a naive un-escaper writes to your
terminal. The escape exists for display, not for transport.

Round-tripping is an explicit **non-goal**: no backslash-escaping (`\` → `\\`)
will be added to make the mapping reversible, in this or any later wire version. A
consumer that needs the original bytes must read them from the source file using the
raw `span` / `fix_edits` byte offsets, which are deliberately left unsanitized for
precisely this purpose.

##### `--diff` preview output (`mds lint --fix --diff` and `mds fmt --diff`)

Preview output is diff text, not a diagnostic field, and is governed separately: it
is neutralized when stdout is a TTY (where control bytes would execute), and emitted
**byte-faithful when stdout is piped or redirected** (where the diff must remain
applicable). It is not part of the `"version": 1` JSON wire format.

### 7.6 `mds init`

```bash
mds init                                   # Creates hello.mds in current directory
mds init my-prompt.mds                     # Creates my-prompt.mds
mds init my-prompt.mds --force             # Overwrite if file already exists
```

Creates a compilable starter template. Path traversal (e.g. `../escaped.mds`) is rejected. A filename carrying a forbidden path character (§4.6) is refused before anything is written (`mds::io`, exit 2). The file is written through the replace-by-rename primitive of §7.2 "Output writing": a symlink at the target — live or dangling — is refused (`mds::io`, exit 2, `cannot write <path>: refusing to replace a symlink`) rather than written through, and a write that fails is `mds::io`, exit 2 (#157); `--force` replaces a regular file atomically, preserving its permission bits.

### 7.7 Auto-Detection

When no `FILE` argument is given to `mds build`, `mds check`, `mds fmt`, `mds lint` or `mds watch`, the compiler scans the current directory for `.mds` files:

- **Exactly one found** → compile that file, named by its file name as it was found — `mds build` announces `Building page.mds`, `mds watch` `Watching page.mds` — exactly as the same command given `page.mds` would name it, never by the directory's absolute path (#390).
- **Zero found** → error with hint to run `mds init`.
- **Multiple found** → error listing the files with a hint to specify one.
- **The directory cannot be listed** → `mds::io`, exit 2, `cannot read directory .: <reason>`, naming it `.` (#390; was an uncoded error, exit 1 — `mds lint`: 2 — that showed its absolute path).

### 7.8 `mds.json` Project Config

Place `mds.json` in the repository root or any ancestor directory of the input file. The CLI discovers it by walking upward from the input — this discovery walk is independent of the project-root resolution described in §5, and the two walks do not influence each other. Relative paths inside `mds.json` (such as `build.output_dir`) resolve against the directory that contains `mds.json`, not against the project root.

```json
{
  "build": {
    "output_dir": "dist"
  },
  "lint": {
    "rules": {
      "unused-variable": "warn",
      "unused-import": "off"
    }
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `build.output_dir` | string | Relative path to output directory. Must not contain `..` components or a forbidden path character (#265); either is refused with `mds::io`, exit 2. |
| `build.source_map` | bool | Enable source-map generation for all builds (equivalent to `--source-map`). Ignored for messages-mode templates. Default: `false`. |
| `build.embed_sources` | bool | Embed source file contents in `sourcesContent[]` (equivalent to `--embed-sources`). Has no effect when `build.source_map` is `false`. Default: `false`. |
| `lint.rules` | object | Per-rule severity overrides for `mds lint`. Keys are rule names; values are `"off"`, `"info"`, `"warn"`, or `"error"`. Any other value fails config loading (exit 1), naming the rule and the value as the bindings name a `rules` value — `lint.rules["<name>"]: unknown severity "<value>"; expected "off", "info", "warn", or "error"`, or `lint.rules["<name>"] must be a severity string, got <type>` for a value that is not a string — each WIRE-escaped (§7.5). An unknown rule name emits a warning naming it and listing the rules this build recognises, the config still loads, and lint continues — the unknown rule is not enforced (forward compat: a config naming a rule added in a newer release warns instead of failing on an older binary). Under `mds lint`, the warning goes to stderr and is suppressed by `--quiet`; `mds build`, `mds watch` and directory-mode `mds fmt` also read this file but do not emit the unknown-rule warning, and `mds check` does not read it. On the `lint` API surfaces it is returned in `lint_warnings`. |
| `fmt.sort_frontmatter_keys` | bool | **Reserved — accepted, currently inert.** The key is parsed and type-checked (a non-boolean value is a hard config-load error, like any other field) so that `{"fmt": {"sort_frontmatter_keys": true}}` is valid today, but it drives no formatting behaviour in this version: `mds fmt` does not sort frontmatter keys and there is no matching CLI flag. Frontmatter key sorting is deferred to a future version; when it ships, this key will control it without a breaking `mds.json` schema change. Default: `true`. Pinned by `fmt_config_valid_section_loads_cleanly` and `fmt_config_malformed_bool_field_fails_loading` in `crates/mds-cli/src/build.rs`. |

Maximum config file size: 1 MiB (1,048,576 bytes).

### 7.9 Exit Codes

**`mds build`, `mds check`, `mds fmt`, `mds init`:**

| Code | Meaning |
|------|---------|
| `0` | Success |
| `1` | Template error (syntax, undefined variable, arity mismatch, recursion, etc.); in directory mode, also "nothing to process" (no `.mds` files, all under default-excluded directories, or — `build`/`check` only — nothing but `_`-prefixed partials), and a run in which files failed but none with an error of the `2` row below — template errors and resource limits, say (§7.2 continue-on-error) |
| `2` | I/O or file-system error (file not found, not an MDS file, I/O failure, a path that is not valid UTF-8, a path carrying a forbidden path character — §4.6); also an output location (`-o`, `--out-dir`, `build.output_dir`) or `mds init` filename carrying one, a `-o`/`--out-dir` value that is not valid UTF-8, a relative `-o`/`--out-dir` when the working directory cannot be determined, and a working directory that auto-detection cannot list (§7.2, §7.7, #390), a `build.output_dir` containing `..`, a directory argument that is a symlink or the filesystem root (§7.2), and an output that is the entry file itself (§7.2). The CLI's I/O failures listed in §5 "Streams and I/O failures" are `mds::io`, exit 2: an output, `.map` sidecar or starter file that cannot be written (a symlink at the destination included), an output directory that cannot be created, a stale sibling or sidecar that cannot be removed, a stale sidecar that cannot be read, a stdout write that fails other than by a closed pipe, and stdin that cannot be read or is not valid UTF-8 (#157). In directory mode a failure of this row — one of these, or a file's own, such as a source that cannot be read — makes the run exit 2 while the other files are still processed (§7.2 continue-on-error). (Changed in v0.5.0: these exited 1, a directory run whose failed files included one of this row's errors exited 1, and a stale sibling or sidecar that could not be removed, or a stale sidecar that could not be read, only warned.) |
| `3` | Resource limit exceeded (output too large, too many iterations, message count exceeds `MAX_MESSAGE_COUNT` (10,000), cumulative message content exceeds 50 MB, frontmatter over 1 MiB, over 200,000 YAML nodes, or flow-nesting deeper than 1024 levels, or stdin over 10 MiB — changed in v0.5.0; previously exit 1) |
| `101` | Internal compiler error: a panic (§5 "Panics on the CLI", #389) — wins over every other code |

A closed stdout or stderr pipe never changes any of these codes (§5 "Streams and I/O failures").

**`mds lint`** (see §7.5 for per-code meaning):

| Code | Meaning |
|------|---------|
| `0` | Clean — no warning- or error-severity findings |
| `1` | Warning-severity findings only (no errors) |
| `2` | Error-severity finding, analysis failure, or usage error (including a directory with nothing to lint, or a directory entry whose path is not valid UTF-8); also an I/O failure (#157): a `--fix` rewrite that fails, a stdout write that fails other than by a closed pipe — which lifts a clean or warning-only run to 2 — and stdin that cannot be read or is not valid UTF-8, each `mds::io` (a rewrite failure in the one wording of §7.5) |
| `3` | Resource limit exceeded (stdin over 10 MiB included — changed in v0.5.0; previously exit 2); a source file over 10 MiB in every mode and format, a directory's entry included (changed in v0.5.0 for a directory under `--format human`; previously exit 2, #309) |
| `101` | Internal compiler error: a panic (§5 "Panics on the CLI", #389) — wins over every other code |

The code-by-code classification behind these tables is the "Error Codes" registry in §5.

### 7.10 Path labels

The table lists lines of `mds build`, `mds check`, `mds fmt`, `mds lint`, `mds watch` and `mds init` that name a path, the form each names it in, and the tests that pin that form (#390). A row marked **carve-out** names a path in another form than the lines beside it, for the reason it gives, and its tests pin that form as it stands. In each error and warning the table lists, the cause after the path — the operating system's, the file watcher's or tempfile's — names no path: one that carried a path is shown by its kind alone (`permission denied`), and the file watcher's without the paths it lists. A line the table does not list is outside it: among others, the refusals of a directory argument (§7.2) and of a forbidden path character (§4.6), the errors that name `mds.json`, the source frame of a compile error `mds lint` reports, and `mds lint`'s `fix rejected:` and diagnostic-cap notices.

The forms:

- **As typed** — the path exactly as the user typed it, relative or absolute: `..` kept (`sub/../sub/page.mds`), a symlink named by the link.
- **Below *X* as typed** — *X* as typed, joined with the path below it: with `mds build src --out-dir out`, the output of `src/a/b.mds` is `out/a/b.md`; with `mds build .`, it is `./a/b.md`.
- **Below `mds.json` as reached** — the directory the input reached `mds.json` by, `.` beside it and one `..` per step up (`src/..`), joined with `build.output_dir` as written in `mds.json`. Nothing is collapsed, and an absolute `output_dir` is shown as written.
- **Beside the file as typed** — the file's directory as typed, `.` for a bare file name, joined with the output's name: `mds build x.mds` → `./x.md`, `mds build src/a.mds` → `src/a.md`.

On Windows a `/` the user typed stays `/`, and the parts `mds` joins to it take `\` (`mds build src --out-dir out` → `out\a\b.md`). The rows marked Windows pin that their lines carry no verbatim `\\?\` prefix.

| Command | Line | Form | Pinned by |
|---|---|---|---|
| `mds build`, `mds fmt` | the banner of an input auto-detection found: `Building <file>`, `Formatting <file>` | its file name, as found: `Building page.mds` | `auto_detection_names_the_file_as_found_in_the_working_directory` |
| `mds build` | `Compiled to` for a file argument with no `-o`, `--out-dir` or `build.output_dir` | beside the file as typed: `./x.md`, `src/a.md` | `no_listed_build_check_or_lint_run_names_the_working_directory` |
| `mds build` | `Compiled to` under `-o <path>` | as typed: `o2/y.md` | `no_listed_build_check_or_lint_run_names_the_working_directory` |
| `mds build` | `Compiled to` under `--out-dir`, for a file argument or stdin | below `--out-dir` as typed: `o1/x.md`; stdin's output `o3/output.md` | `no_listed_build_check_or_lint_run_names_the_working_directory` |
| `mds build` | `Compiled to` for a directory argument | below the directory argument as typed: `src/sub/b.md` | `no_listed_build_check_or_lint_run_names_the_working_directory` |
| `mds build` | `Compiled to` for a directory argument under `--out-dir` | below `--out-dir` as typed, one reached through a symlink by the link and an absolute one as typed: `out/sub/page.md`, `alias-link/sub/page.md` | `compiled_to_under_a_relative_out_dir_names_the_out_dir_as_typed`, `compiled_to_under_a_symlinked_out_dir_names_the_link_not_its_target`, `dir_build_out_dir_status_line_names_the_out_dir_as_typed` |
| `mds build` | `Compiled to` under `mds.json` `build.output_dir` | below `mds.json` as reached: `./dist/page.md`, `src/../dist/sub/deep.md`, `src/sub/../../dist/deep.md` | `a_config_output_dir_is_named_through_the_directory_mds_json_was_reached_by` |
| `mds build` | `Source map written to` | as its output is named, with `.map`: `./x.md.map`, `out/sub/page.md.map`, `./dist/page.md.map` | `source_map_lines_under_an_out_dir_name_the_out_dir_as_typed`, `a_config_output_dir_names_the_map_written_and_the_stale_map_removed`, `no_listed_build_check_or_lint_run_names_the_working_directory` |
| `mds build` | `Removed stale map` | as its output is named, with `.map`: `./dist/page.md.map` | `a_config_output_dir_names_the_map_written_and_the_stale_map_removed` |
| `mds build` | `cannot create output directory <dir>` | the directory its output is named below: `out/sub`, `./dist` | `an_output_directory_that_cannot_be_created_is_named_as_its_output_is` |
| `mds build` | `warning: source_map in <mds.json> has no effect …` | `mds.json` as reached: `./mds.json`, `src/../mds.json` | `the_config_source_map_warning_names_mds_json_as_reached` |
| `mds build` | the `[<file>:<line>:<column>]` header of a compile error's source frame | the source's path relative to the project root (§5): `sub/err_test.mds` | `r3_error_frame_names_root_relative_path_for_subdir_file` |
| `mds build` | the text of an error writing an output — `cannot write <path>: …`, `cannot create temp file for <path>: …`, `cannot rename temp file to <path>: …` and the write's other errors — and `could not remove stale output <path>: …` | as its `Compiled to` names the output, below a directory argument's `--out-dir` and below `build.output_dir` too: `out/a.md`, `./dist/p.md`, `src/../dist/q.md`, `o5/y.md`; the cause names no temporary file: `cannot create temp file for ro/y.md: permission denied` | `an_error_writing_an_output_names_it_as_its_status_line_does` |
| `mds check` | `OK: <file>` | as typed: `OK: src/a.mds`; stdin `OK: <stdin>` | `no_listed_build_check_or_lint_run_names_the_working_directory` |
| `mds fmt` | `Formatted:`, `Unchanged:`, `Would reformat:` | a file argument as typed; a directory's entry, which only `Formatted:` names, below the directory argument as typed: `Formatted: src/inner/b.mds`; stdin `<stdin>` | `fmt_names_a_file_argument_as_typed_and_a_directory_s_entries_below_it` |
| `mds fmt` | the `---` and `+++` header of a `--diff` | as the status lines name the input | `fmt_names_a_file_argument_as_typed_and_a_directory_s_entries_below_it` |
| `mds lint` | `Clean:` | a file argument as typed: `Clean: sub/page.mds` | `clean_names_a_file_argument_as_typed`, `no_listed_build_check_or_lint_run_names_the_working_directory` |
| `mds lint` | `Fixed:`, `Partially fixed:`, `Would fix:` | a file argument as typed; a directory's entry below the directory argument as typed: `Fixed: fix/deep/f.mds`; stdin `<stdin>` | `lint_names_a_file_argument_as_typed_and_its_findings_by_its_file_name`, `a_partial_fix_of_a_file_is_announced_after_the_findings_it_leaves`, `no_listed_build_check_or_lint_run_names_the_working_directory` |
| `mds lint` | the `---` and `+++` header of a `--fix --diff` | as the status lines name the input | `lint_names_a_file_argument_as_typed_and_its_findings_by_its_file_name` |
| `mds lint` | the `[<file>:<line>:<column>]` header of the source frame of a rule's finding, and the JSON `file` key | a file argument's file name: `warn.mds` for `sub/warn.mds`; a directory's entry's path relative to the directory argument, `/`-separated on every OS: `deep/warn.mds`; stdin `<stdin>` | `lint_names_a_file_argument_as_typed_and_its_findings_by_its_file_name`, `no_listed_build_check_or_lint_run_names_the_working_directory` |
| `mds watch` | `Watching <entry>` | as typed, `..` kept and a symlinked directory named by the link; an auto-detected entry by its file name | `watching_names_the_entry_and_the_directory_as_typed`, `watching_names_a_path_reached_through_a_symlink_by_the_link` |
| `mds watch` | `Watching directory <dir>` | as typed, `.`, `./`, `..` and `sub/..` included | `watching_names_the_entry_and_the_directory_as_typed`, `watching_names_a_path_reached_through_a_symlink_by_the_link`, `watch_dot_forms_watch_the_canonical_directory` |
| `mds watch` | `Compiled to` at startup | as `mds build` names the output: `sub/page.md`, `./x.md`, `src/a.md`, `o2/page.md`, `o3/y.md`, `./dist/p.md` | `watch_startup_names_an_out_dir_and_a_config_output_dir_as_build_does`, `recompiled_and_removed_name_each_output_as_typed` |
| `mds watch` | `Recompiled <output>` | as its `Compiled to`; `<stdout>` under `-o -` | `recompiled_and_removed_name_each_output_as_typed`, `watching_names_a_path_reached_through_a_symlink_by_the_link` |
| `mds watch` | `Removed <output> (source deleted)` | as its `Compiled to` | `recompiled_and_removed_name_each_output_as_typed`, `removed_names_the_output_as_typed_when_the_vars_file_changes_in_the_same_batch` |
| `mds watch` | `warning: could not remove <output>` | as its `Compiled to` | `an_output_that_cannot_be_removed_is_named_as_typed` |
| `mds watch` | **carve-out:** the file name of an output derived from the entry, beside it or below `--out-dir` | the entry's name as the volume holds it, which can differ from the name as typed — for an entry typed in another case on a case-insensitive volume, say: `mds watch PAGE.mds` → `./page.md`, where `mds build PAGE.mds` names `./PAGE.md` | `an_entry_typed_in_another_case_names_its_output_by_the_name_on_disk` |
| `mds watch` | `failed to watch directory <dir>` and `warning: failed to watch <dir>`, for the directory argument, the entry's directory, or a directory below either | as typed; a directory below either, below it as typed: `./lib`, `src/lib` | `a_watched_directory_is_named_as_the_user_typed_it` |
| `mds watch` | `warning: failed to watch vars directory <dir>` | the `--vars` file's directory as typed: `nodir`; `.` for a bare file name | `a_vars_directory_that_cannot_be_watched_is_named_as_typed`, `a_watched_directory_is_named_as_the_user_typed_it` |
| `mds watch` | **carve-out:** `failed to watch directory <dir>`, `warning: failed to watch <dir>` and `warning: failed to watch external dep dir <dir>`, for a dependency's directory outside the entry's directory and the directory argument, unless it is the `--vars` file's | the canonical path the compile reports, since no path the user typed leads to it | `a_watched_directory_is_named_as_the_user_typed_it` |
| `mds watch` | the text of an error writing an output — `cannot write <path>: …` and the write's other errors — and `warning: could not remove stale output <path>: …` | as its `Compiled to`, beside the entry and beside a directory argument's source too: `w1/page.md`, `d1/a.md`, `o2/y.md`; a stale output below `--out-dir` as typed: `o5/a.md` | `watch_names_an_output_as_its_status_line_does_in_an_error_writing_it` |
| `mds init` | `Created <file>`, its `Try: mds build <file>` hint, and `<file> already exists` | as typed; `hello.mds` with no argument | `init_names_the_file_it_creates_as_typed` |
| `mds build`, `mds watch` | the refusal of a relative `-o` or `--out-dir` where the working directory cannot be determined: `cannot determine current directory: <reason>` | names no path | `a_relative_output_location_is_refused_where_the_working_directory_is_gone`, `watch_with_a_relative_out_dir_stops_at_startup_where_the_working_directory_is_gone` |
| `mds build`, `mds check`, `mds fmt`, `mds lint`, `mds watch` | auto-detection in a working directory that cannot be listed: `cannot read directory .: <reason>` | `.` | `auto_detection_in_a_working_directory_it_cannot_list_names_it_as_a_dot` |
| `mds build` (Windows) | `Compiled to` and `Source map written to` below an `--out-dir` that exists | below `--out-dir` as typed, with no verbatim `\\?\` prefix | `compiled_to_and_map_lines_under_an_existing_out_dir_carry_no_verbatim_prefix`, `dir_build_out_dir_status_line_has_no_verbatim_prefix_on_windows` |
| `mds watch` (Windows) | `Watching`, `Watching directory` and `Recompiled` | as typed, with no verbatim `\\?\` prefix | `watch_banner_and_recompiled_lines_carry_no_verbatim_prefix` |

The `failed to watch` lines print only when the file watcher refuses a directory, which these tests have no way to make it do for a directory that exists, so their rule is pinned by a unit test of the function that names the directory — and `crates/mds-cli/tests/print_discipline.rs` checks that each of them names it through that function — and the cause after the colon by a unit test of the function that drops the paths the watcher lists. The `--vars` file's directory is pinned by a run as well, whole line and cause included, its bare-file-name arm on macOS only: its watcher refuses the empty path that stands for the working directory, where the Linux and Windows watchers watch the working directory instead. The case-variant row's test runs where the volume does not tell case apart (macOS by default), and each Windows row's tests on Windows only. `crates/mds-cli/tests/path_labels.rs` checks that every row cites at least one test, that each test the table cites is defined in exactly one file and not marked ignored, and that the table cites, row by row, the tests that file lists for it.

---

## 8. Lint Rule Catalog

Rules default to the severities shown below — **not all rules default to `warn`**. Override per rule in `mds.json` or via the `rules` option (library API). Severity values: `"warn"`, `"error"`, `"info"`, `"off"`.

| Rule | Default Severity | Fixable | Tier | Description |
|------|-----------------|---------|------|-------------|
| `unused-variable` | warn | no | C | A frontmatter variable is defined but never referenced in the template body. |
| `unused-import` | warn | suggestion¹ | B | An `@import` statement imports a name that is never used in the file. |
| `unused-function` | warn | suggestion¹ | B | A `@define` function is defined but never called in the file. |
| `shadow-variable` | info (default-off²) | no | C | A variable declared in an inner scope (e.g. `@for`) shadows an outer-scope variable of the same name. |
| `empty-block` | warn | yes (A)³ | A | A control-flow block (`@if`, `@elseif`, `@else`, `@for`, `@define`, `@message`) has an empty or whitespace-only body. |
| `redundant-else` | warn | no | C | An `@else` block whose body is structurally identical to the preceding `@if`/`@elseif` then-body (detected via structural equality). Tier C — never auto-fixed. |
| `unreachable-branch` | **error** | yes (A)³ | A | A branch condition (`@if`/`@elseif`) is always-true (with later branches) or always-false, making some code dead. |
| `duplicate-import` | **error** | yes (A) | A | The same file is imported more than once in a single file (modulo alias). |
| `duplicate-export` | **error** | yes (A) | A | The same export name is defined more than once in a single file. |

¹ **`unused-import` is report-only in practice**: `fixable` is always `false` for this rule. A file that triggers `unused-import` contains at least one `@import` directive, making it non-*structural-standalone* (see below); Tier B fixes require a structural-standalone file. The rule is still useful — the warning clears as a side effect of applying other fixes (e.g. removing a duplicate import that was also the unused one). To silence it, set `"unused-import": "off"` in `mds.json`.

² **`shadow-variable` is default-off**: it emits at `info` severity but is suppressed at the `info` level by default (only shown when explicitly enabled via `mds.json`). `info`-severity findings never affect the exit code.

³ **Tier A block-spanning fixes**: The fix planner uses `end_offset` (threaded into `IfBlock`, `ForBlock`, `DefineBlock` AST nodes) to perform whole-block removal — the complete span from the opening directive through the matching `@end`. The reverify gate still applies fail-closed: if the resulting source does not recompile cleanly or produces different output, the fix is reported but not written to disk. Previously (before the `end_offset` work landed) the planner could only remove the opening directive line, leaving `@end` orphaned and causing the gate to always refuse; that limitation is now resolved.

**Tier concepts:**

- **Structural-standalone** (gates Tier B `--fix`): a file with no `@import`, `@extends`, or use as a partial target. A file that triggers `unused-import` is, by definition, not structural-standalone.
- **Compile-clean** (gates the output-equality reverify for Tier B): a file that compiles successfully without any runtime `--vars`. The reverify checks that removing the unused import or function produces byte-identical compiled output.

**Tier A** fixes always apply (`--fix`) and are gated by a post-fix reverify (recompile-success + no-new-diagnostics + output byte-equality). The reverify lints the fixed source under the file's own name, so a partial's fixed source is checked as a partial and its findings compare with the original's (#309). **Tier B** fixes apply only when the file is structural-standalone. **Tier C** rules are report-only — never auto-fixed.

---

## 9. Complete Example

### Input: `welcome.mds`

```mds
---
name: Alice
items: [apple, banana]
tier: premium
count: 2
debug: false
---

@import "./footer.mds" as footer

@define list(items):
@for item in items:
- {{item}}
@end
@end

Hello {{name}}!

Your items:
{{list(items)}}

@if tier == "premium":
Thanks for being a premium member!
@elseif tier == "pro":
Thanks for being a pro member!
@else:
Upgrade for premium features.
@end

@if !debug:
You have {{count}} items.
@end

@include footer
```

### Output: `welcome.md`

```markdown
---
name: Alice
items: [apple, banana]
tier: premium
count: 2
debug: false
---
Hello Alice!

Your items:
- apple
- banana

Thanks for being a premium member!

You have 2 items.

[footer content here]
```

---

## 10. Editor Integration

### 10.1 File Association

MDS files use the `.mds` extension. To get Markdown syntax highlighting immediately, configure your editor to treat `.mds` as Markdown:

**VS Code** (`settings.json`):
```json
"files.associations": { "*.mds": "markdown" }
```

**Neovim** (`init.lua`):
```lua
vim.filetype.add({ extension = { mds = "markdown" } })
```

**Vim** (`~/.vimrc`):
```vim
autocmd BufNewFile,BufRead *.mds setfiletype markdown
```

**Emacs** (`init.el`):
```elisp
(add-to-list 'auto-mode-alist '("\\.mds\\'" . markdown-mode))
```

**Zed** (`settings.json`):
```json
"file_types": { "Markdown": ["mds"] }
```

**Helix** (`languages.toml`):
```toml
[[language]]
name = "markdown"
file-types = ["md", "markdown", "mds"]
```

**Sublime Text** - create `MDS.sublime-settings` in `Packages/User/`:
```json
{ "extensions": ["mds"] }
```

**JetBrains IDEs** (IntelliJ, WebStorm, PyCharm): Settings → Editor → File Types → Markdown → add `*.mds` pattern.

### 10.2 Frontmatter Detection

The MDS compiler also accepts `.md` files that contain MDS directives. To explicitly mark a `.md` file as MDS, add `type: mds` to the frontmatter:

```mds
---
type: mds
name: Alice
---

Hello {{name}}!
```

The compiler uses this detection order:
1. `.mds` extension → always treated as MDS
2. `.md` extension + `type: mds` frontmatter → treated as MDS
3. `.md` extension without `type: mds` → rejected (not compiled)

### 10.3 MDS-Specific Highlighting (Roadmap)

File association gives standard Markdown highlighting, but `@` directives and `{{var}}` interpolation appear as plain text. Full MDS highlighting requires dedicated editor support:

**Phase 1 - TextMate injection grammar (VS Code, Sublime Text)**

A single JSON file (`mds.tmLanguage.json`) that injects into the Markdown grammar scope, adding keyword highlighting for `@import`, `@if`, `@elseif`, `@else`, `@for`, `@define`, `@end`, `@export`, `@include` and interpolation highlighting for `{{var}}`. Shipped as a VS Code extension.

**Phase 2 - Tree-sitter grammar (Neovim, Helix, Zed)**

A `tree-sitter-mds` grammar that extends Markdown parsing. Provides structural parsing, enabling code folding, text object selections, and indentation rules in addition to highlighting.

**Phase 3 - LSP server**

A language server (Rust) providing diagnostics, completions, go-to-definition for `@import` paths, hover info for variables, and validation errors. Works across all editors that support LSP.

**Markdown Preview**: The recommended approach is to compile `.mds` → `.md` and preview the output. The CLI supports this: `mds build input.mds -o - | less` or pipe to any Markdown viewer. (`mds build` without `-o -` writes `input.md` beside the source and emits only a status line on stderr.)

---

## 11. Out of Scope

These are intentionally deferred to keep the language simple and the compiler focused:

- TypeScript/JS *language* features (note: runtime bindings for calling the compiler from JS/TS *are* provided via the `@mdscript/mds` npm package; this item refers to in-template scripting, which is out of scope)
- Unbounded recursion: direct recursion is rejected; indirect chains are capped at depth 128 (see §4.5)
- Macros, async functions, streaming
- URL-based imports (remote modules)
- Function calls in `@if` conditions (e.g. `@if length(items) == 0:`) — not supported
- Function calls in `@for` iterables (e.g. `@for item in split(csv, ","):`) — not supported
- Parenthesized sub-expressions in conditions (e.g. `@if (a || b) && c:`) — not supported
- Negative indexing in `slice()` — clamped to 0 instead
- Array element indexing (`{items[0]}`) — not supported

---

## 12. Grammar Summary

```
file            := frontmatter? extends? (directive | text)*
frontmatter     := "---\n" yaml_content "---\n"
extends         := "@extends" quoted_path
directive       := import | export | define | include | if_block | for_block | message_block | block

import          := alias_import | merge_import | selective_import
alias_import    := "@import" quoted_path "as" identifier
merge_import    := "@import" quoted_path
selective_import := "@import" "{" identifier_list "}" "from" quoted_path

export          := named_export | reexport | wildcard_reexport
named_export    := "@export" identifier
reexport        := "@export" identifier "from" quoted_path
wildcard_reexport := "@export" "*" "from" quoted_path

define          := "@define" identifier "(" params? "):" body "@end"
params          := param ("," param)*
param           := identifier | identifier "=" cond_value
include         := "@include" identifier
if_block        := "@if" condition ":" body ("@elseif" condition ":" body)* ("@else:" body)? "@end"
condition       := or_expr
or_expr         := and_expr ("||" and_expr)*
and_expr        := simple_cond ("&&" simple_cond)*
simple_cond     := "!" dot_path | dot_path ("==" | "!=") cond_value | dot_path
cond_value      := quoted_string | number | "true" | "false" | "null"
number          := "-"? [0-9]+ ("." [0-9]+)?   (* not NaN or Infinity; those are rejected at parse time *)
for_block       := "@for" loop_vars "in" dot_path ":" body "@end"
loop_vars       := identifier | identifier "," identifier
message_block   := "@message" role ":" body "@end"
block           := "@block" identifier ":" body "@end"
                   (* grammar is context-free; `@block` is additionally constrained to top-level only by the parser — see §4.11 Rules *)
role            := bare_role | "{{" message_role_expr "}}"
bare_role       := <any non-empty text up to the trailing ":"> (* literal string; no identifier validation *)
message_role_expr := qualified_call | member_access | function_call | identifier

text          := (raw_text | interpolation | escaped_open)*
raw_text      := <any run containing no "{{" and no "\{{"; single "{"/"}" is ordinary text>
interpolation := "{{" ws (qualified_call | member_access | function_call | identifier) ws "}}"
escaped_open  := "\{{"        (* emits literal "{{" *)
qualified_call  := identifier "." identifier "(" arguments? ")"
member_access   := identifier ("." identifier)+
function_call   := identifier "(" arguments? ")"
arguments       := argument ("," argument)*
argument        := quoted_string | number | "true" | "false" | "null" | function_call | member_access | identifier
dot_path        := identifier ("." identifier)*
identifier      := [a-zA-Z_][a-zA-Z0-9_]*
identifier_list := identifier ("," identifier)*
quoted_string   := "\"" dq_chars "\"" | "'" sq_chars "'"
dq_chars        := (escape_seq | [^"\\])*
sq_chars        := (escape_seq | [^'\\])*
escape_seq      := "\\\\" | "\\\"" | "\\'"
quoted_path     := "\"" path_chars "\""
```

---

## 13. Status

v0.4.0 - Breaking change release. **Interpolation syntax changed from `{x}` to `{{x}}`** — single `{`/`}` are now always literal text, and `\{{` is the escape for a literal `{{`; run `mds lint --fix` to auto-migrate legacy templates (the `legacy-interpolation` lint rule). `@message {{role}}:` dynamic role syntax updated to use double braces; new `fix_edits` field on `LintDiagnostic` across all binding surfaces. Code fences now correctly recognize tilde fences (`~~~`), indented fences, and blockquoted fences (e.g. `> ``` ...`) as passthrough regions — interpolation and directives are not parsed inside them (#149). Interior whitespace in block bodies and `mds fmt` output now follows the **interior-verbatim with trailing-edge normalization** contract — leading blank lines and interior blank runs are preserved verbatim; only the trailing edge normalizes to one final newline. (`@message` and `@define` bodies still edge-trim via `.trim()` — they strip leading and trailing blank lines.) The `mds fmt` blank-line collapsing rule (R3) has been removed (#150, #151). Cross-type equality comparisons (`string == number`, `boolean != null`, etc.) are now a runtime error (`mds::type_mismatch`) instead of silently returning `false`/`true` — both sides must be the same type (#152). A new `--set-string` CLI flag forces a variable to remain a string regardless of its value, bypassing type coercion; using a key in both `--set` and `--set-string` is now a hard error (#152). `@extends` children now emit the **deep-merged** frontmatter (base < child, reserved keys excluded) instead of only the child's raw frontmatter — base-only keys appear in the compiled output (#154).

v0.3.0 - Auto-formatter (`mds fmt`), intrinsic output format (Markdown vs JSON messages decided by content not a flag), native Python bindings (PyO3).

v0.2.0 - Language enrichment release. Adds built-in functions (18 functions for string, array, and type-conversion operations), default function arguments, and logical operators (`&&`, `||`) in `@if` conditions with short-circuit evaluation and operator-precedence semantics.

v0.1.0 - Initial public release. The core compiler is feature-complete as described in this specification, including negation in `@if` conditions (`!dot_path`), equality/inequality comparisons (`==`, `!=`), the `@elseif` directive, and `NaN`/`Infinity` rejection at parse time.
