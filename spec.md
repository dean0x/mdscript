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
- Resolved import paths stay inside the project root (see §5 Project Root); a path that escapes it is a compilation error
- Import resolution is recursive (imports can import)

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
- Symlinked files and symlinked directories inside the tree are skipped; a symlinked entry root is rejected at startup.
- Output extension per file is intrinsic (`.md` or `.json`).
- With `--out-dir <out>`, mirrors the source subtree under `<out>/`; without it, writes next to source.
- `-o` is rejected for a directory input.
- Continue-on-error: all compilable files are attempted; a summary (`N built, N failed`) is printed when any file fails or when `--quiet` is not passed; non-zero exit when any failed. Under `--quiet`, the summary is suppressed on a fully-successful run and emitted when any file fails, so the non-zero exit is never unexplained.
- When the directory contains no `.mds` files, exits 0 with a "no files found" message.
- **Stale-flip cleanup**: when a file's kind changes (e.g., markdown → messages), the old-extension sibling (`.md` or `.json`) is removed automatically.
- stdin (`mds build -`) with `--out-dir`: the fallback output name is `output.md` (markdown) or `output.json` (messages).

**Options:**

| Option | Description |
|--------|-------------|
| `-o, --output <PATH>` | Output file path, or `-` for stdout. Mutually exclusive with `--out-dir`. Rejected for directory input. Warns if the extension contradicts the template kind. |
| `--out-dir <DIR>` | Output directory. Mirrors subtree (dir mode) or writes `<stem>.<ext>` inside it (file mode). Created if absent. |
| `--vars <FILE>` | JSON file with runtime variable overrides. |
| `--set KEY=VALUE` | Set a single variable. Repeatable. Values are coerced to boolean, number, null, or array when possible. Repeating a key emits a warning; the last value wins. |
| `--set-string KEY=VALUE` | Set a single variable as a **string**, bypassing type coercion. Repeatable. Use when the value must remain a string (e.g. a numeric-looking ID). Repeating a key emits a warning; the last value wins. |
| `--source-map` | Generate a source-map sidecar (`<output>.map`, e.g. `-o out.md` → `out.md.map`). Ignored for messages-mode templates (a warning is emitted and no source map is produced). Conflicts with `--no-source-map`. Also enabled globally via `build.source_map = true` in `mds.json`. |
| `--no-source-map` | Disable source-map generation. Overrides `build.source_map = true` in `mds.json`. Conflicts with `--source-map`. |
| `--inline` | Embed the source map as a data-URI comment in the compiled output instead of a sidecar. Requires `--source-map`. |
| `--embed-sources` | Embed source file contents in `sourcesContent[]`. Ships full source text — use with care. Requires `--source-map`. |
| `-q, --quiet` | Suppress status messages on stderr on a successful run. The directory-mode summary is suppressed on a fully-successful run; it is still emitted when any file fails. (Two warning-severity notices — the directory-depth warning and the stale-sibling-unlink failure warning — are emitted regardless of `--quiet`.) |

**Output path resolution** (precedence order, highest first):

1. `-o -` → stdout
2. `-o <path>` → exact path (extension determined by caller; compiler warns on mismatch)
3. Stdin input with no `-o`/`--out-dir` → stdout
4. `--out-dir <dir>` → `<dir>/<stem>.<ext>` (file mode) or `<dir>/<rel/path>.<ext>` (dir mode)
5. `mds.json` `build.output_dir` → `<config_dir>/<output_dir>/<stem>.<ext>`
6. Default → `<source_dir>/<stem>.<ext>`

In all paths, `<ext>` is `md` for Markdown templates and `json` for messages templates.

### 7.3 `mds check`

```bash
mds check                                  # Auto-detect single .mds in current dir
mds check template.mds                     # Validate a specific file
mds check template.mds --set name=Alice    # Validate with variable overrides
echo "@if flag:" | mds check -             # Validate from stdin
mds check src/                             # Validate every non-partial .mds in the tree
```

Exits 0 if all templates are valid, non-zero on any error. Same `--vars`/`--set`/`--set-string`/`--quiet` options as `mds build`. Directory mode follows the same semantics as `mds build <dir>` (partial skipping, symlink rejection, continue-on-error) but does not write any output files. In directory mode the summary line is `N passed, N failed`, emitted under the same `--quiet` rule as `mds build <dir>` (§7.2): suppressed on a fully-successful run, emitted when any file fails.

### 7.4 `mds fmt`

```bash
mds fmt template.mds                       # Format a single file in place
mds fmt src/                               # Format all .mds files under src/
echo "template content" | mds fmt -       # Format from stdin, write to stdout
mds fmt template.mds --check              # Exit non-zero if file would change
mds fmt template.mds --diff               # Print unified diff without writing
```

Formats `.mds` templates: normalizes CRLF to LF (everywhere, including inside frontmatter and code fences), strips trailing whitespace on directive lines, and ensures exactly one trailing newline. An empty or whitespace-only source formats to 0 bytes (an empty output file). Interior blank lines and blank-line structure within frontmatter and code fences are left verbatim (blank-line collapsing was removed in v0.4.0 to preserve the interior-verbatim whitespace contract). Body-text trailing whitespace (Markdown hard breaks) and the byte-for-byte content of `@message`/`@define` bodies are left untouched.

Every rewrite is **safety-gated**: the formatter re-compiles both the original and formatted sources and refuses to write if compiled output would change (`mds::formatter_invariant`), so a formatting bug can never corrupt a template.

| Option | Description |
|--------|-------------|
| `--check` | Exit non-zero without writing if any file would change. |
| `--diff` | Print a unified diff of proposed changes without writing. |
| `-q, --quiet` | Suppress per-file status messages and the directory summary on a successful run. The summary is still emitted when any file fails to format. Exception: under `--check`, a run where files would reformat but none failed exits 1 with no summary — the would-reformat count is treated as status output and is suppressed by `--quiet` (mirrors the same rule for `mds lint --fix --check`). (Two notices bypass `--quiet`: the directory-depth warning and the all-files-excluded diagnostic.) |

### 7.5 `mds lint`

```bash
mds lint                                   # Auto-detect single .mds in current dir
mds lint template.mds                      # Lint a single file
mds lint src/                              # Lint all .mds files recursively (incl. partials)
mds lint --fix template.mds                # Auto-fix fixable issues in place
mds lint --fix --check template.mds        # Preview --fix: exit 1 if any file would change
mds lint --fix --diff template.mds         # Preview --fix: print unified diff without writing
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
| `--check` | With `--fix`: exit 1 if any file would change; never writes. Useful for CI. |
| `--diff` | With `--fix`: print unified diff of pending changes without writing. |
| `--format <FORMAT>` | Output format: `human` (default, stderr) or `json` (stdout). |
| `--vars <FILE>` | JSON file with runtime variable overrides (forwarded to the check gate). |
| `--set KEY=VALUE` | Set a single variable. Repeatable. Type coercion applies. Repeating a key emits a warning; the last value wins. |
| `--set-string KEY=VALUE` | Set a single variable as a string, bypassing type coercion. Repeatable. Repeating a key emits a warning; the last value wins. |
| `-q, --quiet` | Suppress warning/info human diagnostics and the directory summary on clean/warn-only runs; errors still print and the summary still appears when error- or resource-limited files are present. (The directory-depth warning — fires on trees deeper than MAX_DEPTH=64 — is emitted regardless of `--quiet`.) |

**Directory mode** (`mds lint <dir>`):

- Lints every `.mds` file recursively (including `_`-prefixed partials).
- Accumulate-and-continue: per-file errors do not abort the run.
- After processing all files, emits one summary line to stderr:
  `N clean, N with warnings, N with errors, N resource-limited`
  Each file falls in exactly one bucket, so the four counts always sum to the number of
  `.mds` files the walker collected.
  - "Clean" — no findings.
  - "With warnings" — warning-severity findings only.
  - "With errors" — error-severity lint findings **or** a per-file analysis failure
    (source read, config load, or lint call failure). These two populations are
    deliberately merged, matching the way `mds build`'s "failed" count merges them.
  - "Resource-limited" — files where `mds::lint` returned `MdsError::ResourceLimit`
    (for example, exceeding `MAX_BLOCKS_PER_MODULE`). These are counted here, never
    under "with errors".
- Under `--quiet`, the summary is suppressed when the worst outcome is warnings only
  (mirrors `mds fmt`'s contract).  When any file is in the error or resource-limited
  bucket, the summary is always emitted so the non-zero exit is never unexplained.
  **Exception:** `mds lint --fix --check --quiet <dir>` exits 1 with zero
  stderr bytes when pending fixes exist but no file is in the error or
  resource-limited bucket — the `--fix --check` pending-fix signal is treated as
  status output and is suppressed by `--quiet` alongside the summary line.
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

**Exit codes** (lint-specific; differ from `mds build`/`mds check`):

| Code | Meaning |
|------|---------|
| `0` | Clean — no warning- or error-severity findings (`info` findings never raise exit code) |
| `1` | Warning-severity findings only (no errors) |
| `2` | Any error-severity finding, analysis failure (parse/resolve/IO/config), or usage error |
| `3` | Resource limit exceeded |

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

Keys are in alphabetical order (BTreeMap serialization). Within each `files[].diagnostics` array, diagnostics are ordered by ascending `span.offset`; span-less diagnostics sort last; equal-offset ties preserve rule-execution order (stable sort). (The CLI and binding surfaces always produce results through `LintResultBuilder`; a `LintResult` assembled directly via `LintResult::new` preserves caller-supplied order instead.) In directory mode, `files[].file` is the forward-slash-separated path relative to the lint root (e.g. `src/template.mds`), and the `files[]` array is ordered by the byte-wise string comparison of that relative display path (e.g. `api-utils.mds` sorts before `api/x.mds` because `'-'` (0x2D) < `'/'` (0x2F)). `"truncated": true` when the result set was capped by the per-file diagnostic cap of 1,000. `"span"` is JSON `null` for diagnostics that lack a source location. When linting from stdin (`mds lint -`), `files[].file` is `"<stdin>"`.

**`lint_warnings` field (binding surfaces only):** The napi, WASM, and Python binding surfaces include an optional top-level `"lint_warnings"` key in the returned result object when non-fatal warnings were produced during linting (for example, unknown rule names in `mds.json`). In the JSON wire form (napi, WASM, and Python `to_dict()` / `to_json()`) the key is absent (not `null`, not `[]`) when no warnings occurred; on the Python live-object surface, `LintResult.lint_warnings` is a property that always exists and returns an empty list when no warnings occurred. In alphabetical key order `"lint_warnings"` sorts between `"files"` and `"truncated"`. The CLI does **not** include `"lint_warnings"` in its `--format json` stdout envelope — it writes warnings to stderr so the JSON stdout remains valid and parseable without modification.

A file that produces a per-file analysis failure in directory mode (malformed config, I/O error) emits a `{"file":"…","error":{"code":"…","message":"…","help":"…","span":…}}` entry without a `"diagnostics"` key and contributes to exit code 2. When a stdin source fails the check gate before linting begins, the CLI emits an analysis-failure envelope to stdout: `{"version":1,"error":{"code":"…","message":"…","help":"…","span":…}}`. This envelope carries no `"files"` or `"truncated"` key, and no `"file"` key (unlike the success envelope above). A JSON consumer MUST handle both the success envelope and the analysis-failure envelope and MUST NOT assume a `"file"` key is present in error results.

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
| `span`, `fix_edits[].start`/`end` | **Raw byte offsets** into the unmodified source — deliberately not sanitized. These are numeric position values and must reflect the original source exactly. |
| `fix_edits[].new_text` | WIRE-sanitized. This field is a **display preview** of the replacement text; consumers MUST NOT apply it as a patch payload. Applying fixes is `mds lint --fix`, the functional path, which reads raw bytes directly from the internal `LintDiagnostic` struct and never serializes via this JSON field. |

This invariant applies across all surfaces that emit `"version": 1` JSON: CLI
(`mds lint --format json`), napi (`lintVirtual` / `lint` / `lintFile`), WASM
(`lintVirtual` / `lint`), and Python (`lint_virtual` / `lint` / `lint_file`).
All four surfaces emit byte-identical values on the fields they share, with two exceptions. First, `"lint_warnings"` is a binding-surface-only key: it is absent from the CLI's `--format json` output (the CLI writes unknown-rule warnings to stderr instead). Second, the `"file"` key takes different values by surface and input method: `mds lint -` (CLI) relabels `"input.mds"` to `"<stdin>"` at the output boundary; the string-source `lint()` entrypoint on napi, WASM, and Python retains `"input.mds"`; `lintVirtual` (napi/WASM) and `lint_virtual` (Python) emit the caller-supplied entry key instead. The fields `message`, `help`, `rule`, `severity`, `span`, and `fix_edits` share the same Rust serializer across all surfaces and are designed to be byte-identical, but no live cross-surface differential test currently compares these fields between the CLI and binding surfaces on a source that produces findings — the existing parity test uses a clean source with no diagnostics. For source maps, the same stdin-relabeling asymmetry applies: `mds build -` (CLI) replaces the internal `"input.mds"` entry with `"<stdin>"` in `sources[]`; binding surface string-source compiles always carry `"input.mds"` in `sources[0]`, and virtual-FS compiles carry the caller-supplied entry key.

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
| A filename or path in a diagnostic `file` **field**: the JSON `file` key, a CLI status line, a `[file:line:col]` frame header | WIRE on every surface that renders one | Single-line by construction; POSIX permits `\n` in a filename and the user never types it |
| `mds.json` rule names and config values, `--format` arguments | WIRE on every surface that renders one | Single-line identifiers. WIRE applies in all rendering contexts — including when a rule name appears inside a warning body (e.g., the unknown-rule-names warning), the name is WIRE-escaped, not HUMAN. This row takes precedence over the residual row below for `mds.json`-sourced values. |
| `io::Error` / `MdsError` causes interpolated into a CLI status or warning line | WIRE on every surface that renders one | Single-line, and they embed paths of their own |
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
- **`mds-core` `MdsError` message bodies**, which interpolate paths and `io::Error`
  causes (`cannot read {path}: {e}`, `invalid UTF-8 in {path}: {e}` in
  `crates/mds-core/src/fs.rs`) and template identifiers (`invalid import alias:
  '{alias}'` in `parser_helpers.rs`).

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

These are **functional references, not display text**. Source Map v3 `file` and
`sources` are resolved against the filesystem by devtools, bundlers and IDEs;
`dependencies` is a watch/rebuild input for the bundler plugins. Rewriting a path to a
`\uXXXX` literal would produce a path that does not exist, breaking source-map
resolution and dependency tracking in order to defend against a pathological filename.
That is the same product-versus-display distinction that keeps compiled output
unescaped: escaping the artefact corrupts the artefact.

Consequently, and normatively:

> **Consumers of a source map or of `dependencies` MUST treat every path they contain
> as untrusted input.** A path may contain any byte a filesystem permits, including C0
> control characters, `\n`, bidi controls and U+FEFF. A consumer that prints such a path
> to a terminal, writes it into a log line, or interpolates it into HTML must escape it
> for that destination itself. JSON string encoding is *not* that escaping: it makes the
> document parseable, and a decoded `"\n"` is a real newline again.

The CLI does not rely on this contract for its own output: the `Compiled to …` and
`Source map written to …` status lines print the path through `safe_path`, so they carry
the WIRE-escaped form even though the sidecar they name does not.

Closing this differently — rejecting control characters in filenames at the input
boundary rather than escaping them at output — is a plausible longer-term design and is
deliberately not specified here.

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

Creates a compilable starter template. Path traversal (e.g. `../escaped.mds`) is rejected.

### 7.7 Auto-Detection

When no `FILE` argument is given to `mds build` or `mds check`, the compiler scans the current directory for `.mds` files:

- **Exactly one found** → compile that file.
- **Zero found** → error with hint to run `mds init`.
- **Multiple found** → error listing the files with a hint to specify one.

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
| `build.output_dir` | string | Relative path to output directory. Must not contain `..` components. |
| `build.source_map` | bool | Enable source-map generation for all builds (equivalent to `--source-map`). Ignored for messages-mode templates. Default: `false`. |
| `build.embed_sources` | bool | Embed source file contents in `sourcesContent[]` (equivalent to `--embed-sources`). Has no effect when `build.source_map` is `false`. Default: `false`. |
| `lint.rules` | object | Per-rule severity overrides for `mds lint`. Keys are rule names; values are `"warn"`, `"error"`, or `"off"`. Unknown severity values cause a hard config-load error. An unknown rule name emits a warning naming it and listing the rules this build recognises, the config still loads, and lint continues — the unknown rule is not enforced (forward compat: a config naming a rule added in a newer release warns instead of failing on an older binary). Under `mds lint`, the warning goes to stderr and is suppressed by `--quiet`; `mds build`, `mds check`, `mds fmt`, and `mds watch` also read this file but do not emit the unknown-rule warning. On the `lint` API surfaces it is returned in `lint_warnings`. |

Maximum config file size: 1 MB.

### 7.9 Exit Codes

**`mds build`, `mds check`, `mds fmt`, `mds init`:**

| Code | Meaning |
|------|---------|
| `0` | Success |
| `1` | Template error (syntax, undefined variable, arity mismatch, recursion, etc.) |
| `2` | I/O or file-system error (file not found, not an MDS file, I/O failure) |
| `3` | Resource limit exceeded (output too large, too many iterations, message count exceeds `MAX_MESSAGE_COUNT` (10,000), or cumulative message content exceeds 50 MB) |

**`mds lint`** (see §7.5 for per-code meaning):

| Code | Meaning |
|------|---------|
| `0` | Clean — no warning- or error-severity findings |
| `1` | Warning-severity findings only (no errors) |
| `2` | Error-severity finding, analysis failure, or usage error |
| `3` | Resource limit exceeded |

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

**Tier A** fixes always apply (`--fix`) and are gated by a post-fix reverify (recompile-success + no-new-diagnostics + output byte-equality). **Tier B** fixes apply only when the file is structural-standalone. **Tier C** rules are report-only — never auto-fixed.

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
