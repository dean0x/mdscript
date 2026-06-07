# MDS Language Specification (v0.2)

## 1. Overview

MDS (Markdown Script) is a domain-specific language for composing, reusing, and compiling LLM prompts.

- **Input**: `.mds` files (Markdown-native syntax with lightweight directives)
- **Output**: Compiled Markdown/plain text strings
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
- Object values support dot-notation field access: `{config.key}`, `{a.b.c}`
- Objects cannot be interpolated directly; access a specific field instead

---

### 4.2 Interpolation

```mds
Hello {name}!
```

**Rules:**

- Single braces: `{identifier}` or dot path `{obj.field}`
- Valid interpolation: a valid identifier (`[a-zA-Z_][a-zA-Z0-9_]*`), dot path (`{config.key}`, `{a.b.c}`), or function call
- Escaping: `\{` produces a literal `{` in output; `\}` produces a literal `}` in output
- Inside fenced code blocks (triple backtick): no interpolation occurs (raw passthrough)
- Undefined variable → compilation error (not silent empty string)

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
- Equality is **strict**, no type coercion: `@if count == "3":` is false when count is the number 3
- `NaN == NaN` is false (IEEE 754)
- `@elseif` branches are evaluated in order; first matching branch wins (short-circuit)
- `@elseif` must appear before `@else:`; `@else:` cannot be followed by `@elseif`
- Cannot combine negation with comparison: `@if !var == "x":` is a parse error. Use `@if var != "x":` instead
- `@if !!var:` (double negation) is a parse error
- Maximum 256 `@elseif` branches per `@if` block
- Nesting: plain `@end`, resolved by innermost matching

---

### 4.4 Loops

```mds
@for item in items:
- {item}
@end
```

Key-value iteration over objects:

```mds
@for key, value in config:
{key} = {value}
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
Hello {name}, welcome!
@end
```

With default arguments:

```mds
@define greet(name = "World"):
Hello {name}!
@end

{greet()}         @# → Hello World!
{greet("Alice")}  @# → Hello Alice!
```

Invocation:

```mds
{greet("Alice")}
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

{utils.greet("Alice")}
```

**Merge import** - exports merge directly into current scope:

```mds
@import "./base.mds"

{greet("Alice")}
```

**Selective import** - pick specific exports by name:

```mds
@import { greet, farewell } from "./utils.mds"

{greet("Alice")}
{farewell("Alice")}
```

**Rules:**

- Relative paths only (no bare module names)
- `as alias` namespaces all exports: access via `{alias.name}`
- Without alias (merge): exports enter current scope (name collision → compilation error)
- Selective: only listed names are brought into scope
- Circular imports → compilation error
- Import resolution is recursive (imports can import)

---

### 4.7 Exports

MDS supports three export styles:

**Named export** - export a locally defined symbol:

```mds
@define greet(name):
Hello {name}!
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
Hello {name}!
@end

@define welcome(name, role):
Welcome {name}, you're joining as {role}.
@end

@export hello
@export welcome
```

```mds
# prompts/formatting.mds
@define bullet_list(items):
@for item in items:
- {item}
@end
@end

@define numbered_list(items):
@for item in items:
1. {item}
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

{prompts.hello(user)}

You have access to:
{prompts.bullet_list(tools)}
```

Output:
```markdown
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
| `@message {role}:` | Expression — the role is evaluated at runtime from the variable |

```mds
---
role: assistant
---

@message {role}:
This role comes from the variable.
@end
```

**Output modes:**

| Mode | Invocation | Behaviour |
|------|------------|-----------|
| Text (default) | `compile_str` / `mds build` | `@message` body rendered inline; role markers ignored — backward compatible |
| Messages | `compile_messages_str` / `compile_messages_virtual` / `mds build --format messages` | JSON array of `{role, content}` objects |

```mds
# text mode output:
You are a helpful assistant.
Hello!

# messages mode output:
[
  { "role": "system", "content": "You are a helpful assistant." },
  { "role": "user",   "content": "Hello!" }
]
```

**Rules:**

- Role must be a non-empty string; an empty or whitespace-only bare-word role is
  a parse error
- Bare-word roles are always literal strings — they never look up variables
- Dynamic roles (`{expr}`) must evaluate to a non-empty, non-whitespace string at
  runtime: a non-string value → type error; a string that trims to empty →
  type error (the same rejection applies at runtime as at parse time)
- Outer whitespace of the body is trimmed; inner whitespace is preserved
- Empty bodies (trims to empty string) are silently skipped
- Frontmatter is excluded from message content
- Nested `@message` blocks are a parse error
- In messages mode, text outside any `@message` block emits a warning and is ignored
- A template with no `@message` blocks compiled in messages mode is a compile error
- `@if` and `@for` around `@message` blocks work identically in both modes; the same iterable rules apply (see §4.4)
- `@include` is silently ignored in messages mode and emits a warning; included module bodies are not surfaced as messages — compose with `@message` blocks directly

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
- {tool}
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

## 5. Compilation Model

| Phase | Description | Errors |
|-------|-------------|--------|
| 1. Parse | Tokenize → AST (frontmatter, directives, text nodes) | Syntax errors (unexpected token, unclosed block) |
| 2. Resolve | Recursively load imports, build dependency graph | File not found, circular import |
| 3. Validate | Check all references, types, arity | Undefined var/function, type mismatch, wrong arg count |
| 4. Evaluate | Execute directives (expand loops, resolve conditions, call functions) | Iterate non-array, recursion detected |
| 5. Render | Flatten evaluated tree → final Markdown string | (none expected) |

### Frontmatter Preservation

When the input file has YAML frontmatter, the compiled output preserves it:

- The original frontmatter content is prepended to the output between `---` fences
- The `type: mds` key (used for `.md` file detection) is stripped from the output frontmatter
- If stripping `type: mds` leaves the frontmatter empty, no fences are emitted
- Runtime variable overrides affect the body but do not alter the output frontmatter
- Only the root module's frontmatter appears in output; imported modules' frontmatter is not emitted

### Error Format

```
mds::undefined_var

  × undefined variable 'username'
   ╭─[src/welcome.mds:1:7]
 1 │ Hello {username}!
   ·       ────┬───
   ·           ╰── not defined
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
6. **Shadowing**: inner scope wins, no warning (intentional override)

---

## 7. CLI Interface

### 7.1 Commands

| Command | Purpose |
|---------|---------|
| `mds build [FILE]` | Compile an `.mds` template to Markdown |
| `mds check [FILE]` | Validate a template without rendering |
| `mds init [FILENAME]` | Create a starter `.mds` file |

### 7.2 `mds build`

```bash
mds build                                  # Auto-detect single .mds in current dir
mds build template.mds                     # Compile to template.md (next to source)
mds build template.mds -o output.md        # Compile to a specific file
mds build template.mds -o -               # Compile to stdout
mds build template.mds --out-dir dist      # Compile to dist/template.md
mds build template.mds --vars vars.json    # With variable overrides from JSON file
mds build template.mds --set name=Alice    # Set a single variable
mds build template.mds --set name=Alice --set count=3  # Multiple variables
echo "Hello {name}!" | mds build -         # Compile from stdin (writes to stdout)
```

**Options:**

| Option | Description |
|--------|-------------|
| `-o, --output <PATH>` | Output file path, or `-` for stdout. Mutually exclusive with `--out-dir`. |
| `--out-dir <DIR>` | Output directory. Creates `<stem>.md` inside it. Created if absent. |
| `--vars <FILE>` | JSON file with runtime variable overrides. |
| `--set KEY=VALUE` | Set a single variable. Repeatable. Values are coerced to boolean, number, null, or array when possible. |
| `--format <FORMAT>` | Output format: `markdown` (default) or `messages` (JSON `[{role, content}]` array). In `messages` mode, output goes to stdout or `-o <path>`; `--out-dir` and `mds.json` `output_dir` are ignored. |
| `-q, --quiet` | Suppress status messages and warnings on stderr. |

**Output path resolution** (precedence order, highest first):

1. `-o -` → stdout
2. `-o <path>` → exact path
3. Stdin input with no `-o`/`--out-dir` → stdout
4. `--out-dir <dir>` → `<dir>/<stem>.md`
5. `mds.json` `build.output_dir` → `<config_dir>/<output_dir>/<stem>.md`
6. Default → `<source_dir>/<stem>.md`

### 7.3 `mds check`

```bash
mds check                                  # Auto-detect single .mds in current dir
mds check template.mds                     # Validate a specific file
mds check template.mds --set name=Alice    # Validate with variable overrides
echo "@if flag:" | mds check -             # Validate from stdin
```

Exits 0 if the template is valid, non-zero on any error. Same `--vars`/`--set`/`--quiet` options as `mds build`.

### 7.4 `mds init`

```bash
mds init                                   # Creates hello.mds in current directory
mds init my-prompt.mds                     # Creates my-prompt.mds
mds init my-prompt.mds --force             # Overwrite if file already exists
```

Creates a compilable starter template. Path traversal (e.g. `../escaped.mds`) is rejected.

### 7.5 Auto-Detection

When no `FILE` argument is given to `mds build` or `mds check`, the compiler scans the current directory for `.mds` files:

- **Exactly one found** → compile that file.
- **Zero found** → error with hint to run `mds init`.
- **Multiple found** → error listing the files with a hint to specify one.

### 7.6 `mds.json` Project Config

Place `mds.json` in the project root (or any ancestor directory). The compiler walks up from the input file to find it.

```json
{
  "build": {
    "output_dir": "dist"
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `build.output_dir` | string | Relative path to output directory. Must not contain `..` components. |

Maximum config file size: 1 MB.

### 7.7 Exit Codes

| Code | Meaning |
|------|---------|
| `0` | Success |
| `1` | Template error (syntax, undefined variable, arity mismatch, recursion, etc.) |
| `2` | I/O or file-system error (file not found, not an MDS file, I/O failure) |
| `3` | Resource limit exceeded (output too large, too many iterations, message count exceeds `MAX_MESSAGE_COUNT` (10,000), or cumulative message content exceeds 50 MB) |

---

## 8. Complete Example

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
- {item}
@end
@end

Hello {name}!

Your items:
{list(items)}

@if tier == "premium":
Thanks for being a premium member!
@elseif tier == "pro":
Thanks for being a pro member!
@else:
Upgrade for premium features.
@end

@if !debug:
You have {count} items.
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

## 9. Editor Integration

### 9.1 File Association

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

### 9.2 Frontmatter Detection

The MDS compiler also accepts `.md` files that contain MDS directives. To explicitly mark a `.md` file as MDS, add `type: mds` to the frontmatter:

```mds
---
type: mds
name: Alice
---

Hello {name}!
```

The compiler uses this detection order:
1. `.mds` extension → always treated as MDS
2. `.md` extension + `type: mds` frontmatter → treated as MDS
3. `.md` extension without `type: mds` → rejected (not compiled)

### 9.3 MDS-Specific Highlighting (Roadmap)

File association gives standard Markdown highlighting, but `@` directives and `{var}` interpolation appear as plain text. Full MDS highlighting requires dedicated editor support:

**Phase 1 - TextMate injection grammar (VS Code, Sublime Text)**

A single JSON file (`mds.tmLanguage.json`) that injects into the Markdown grammar scope, adding keyword highlighting for `@import`, `@if`, `@elseif`, `@else`, `@for`, `@define`, `@end`, `@export`, `@include` and interpolation highlighting for `{var}`. Shipped as a VS Code extension.

**Phase 2 - Tree-sitter grammar (Neovim, Helix, Zed)**

A `tree-sitter-mds` grammar that extends Markdown parsing. Provides structural parsing, enabling code folding, text object selections, and indentation rules in addition to highlighting.

**Phase 3 - LSP server**

A language server (Rust) providing diagnostics, completions, go-to-definition for `@import` paths, hover info for variables, and validation errors. Works across all editors that support LSP.

**Markdown Preview**: The recommended approach is to compile `.mds` → `.md` and preview the output. The CLI supports this: `mds build input.mds | less` or pipe to any Markdown viewer.

---

## 10. What's NOT in v0.2

These are intentionally deferred to keep the language simple and the compiler focused:

- TypeScript/JS *language* features (note: runtime bindings for calling the compiler from JS/TS *are* provided via the `@mdscript/mds` npm package; this item refers to in-template scripting, which is out of scope)
- Unbounded recursion: direct recursion is rejected; indirect chains are capped at depth 128 (see §4.5)
- Macros, async functions, streaming
- URL-based imports (remote modules)
- Source maps
- Template inheritance
- Function calls in `@if` conditions (e.g. `@if length(items) == 0:`) — not supported
- Function calls in `@for` iterables (e.g. `@for item in split(csv, ","):`) — not supported
- Parenthesized sub-expressions in conditions (e.g. `@if (a || b) && c:`) — not supported
- Negative indexing in `slice()` — clamped to 0 instead
- Array element indexing (`{items[0]}`) — not supported

---

## 11. Grammar Summary

```
file            := frontmatter? (directive | text)*
frontmatter     := "---\n" yaml_content "---\n"
directive       := import | export | define | include | if_block | for_block | message_block

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
role            := bare_role | "{" message_role_expr "}"
bare_role       := <any non-empty text up to the trailing ":"> (* literal string; no identifier validation *)
message_role_expr := qualified_call | member_access | function_call | identifier

text            := (raw_text | interpolation | escaped_brace)*
interpolation   := "{" (qualified_call | member_access | function_call | identifier) "}"
qualified_call  := identifier "." identifier "(" arguments? ")"
member_access   := identifier ("." identifier)+
function_call   := identifier "(" arguments? ")"
arguments       := argument ("," argument)*
argument        := quoted_string | number | "true" | "false" | "null" | function_call | member_access | identifier
dot_path        := identifier ("." identifier)*
escaped_brace   := "\{" | "\}"

identifier      := [a-zA-Z_][a-zA-Z0-9_]*
identifier_list := identifier ("," identifier)*
quoted_string   := "\"" dq_chars "\"" | "'" sq_chars "'"
dq_chars        := (escape_seq | [^"\\])*
sq_chars        := (escape_seq | [^'\\])*
escape_seq      := "\\\\" | "\\\"" | "\\'"
quoted_path     := "\"" path_chars "\""
```

---

## 12. Status

v0.2.0 - Language enrichment release. Adds built-in functions (18 functions for string, array, and type-conversion operations), default function arguments, and logical operators (`&&`, `||`) in `@if` conditions with short-circuit evaluation and operator-precedence semantics.

v0.1.0 - Initial public release. The core compiler is feature-complete as described in this specification, including negation in `@if` conditions (`!dot_path`), equality/inequality comparisons (`==`, `!=`), the `@elseif` directive, and `NaN`/`Infinity` rejection at parse time.
