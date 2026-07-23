# Formatting with `mds fmt`

`mds fmt` is an opinionated, **safety-gated** auto-formatter for MDS source. It
makes small, mechanical fixes to directive lines and the file's trailing edge —
and nothing else. Every rewrite is re-compiled and refused if it would change
the compiled output, so formatting a file can never alter what it produces.

> **Upgrading a legacy project?** Run `mds lint --fix` first to auto-migrate
> `{x}` → `{{x}}` interpolation syntax (the `legacy-interpolation` rule), then
> run `mds fmt` to normalize formatting. `mds fmt` itself performs no legacy
> detection — the migration step is always `mds lint --fix` first.

[`demo.mds`](demo.mds) is a complete, already-formatted template. It is checked
in **fmt-clean**, so `mds fmt --check examples/formatting/` exits `0`.

## What it changes

- **Line endings** — `CRLF` / `CR` normalized to `LF` (everywhere except the
  verbatim body of `@message` / `@define`).
- **Directive lines** — trailing whitespace stripped (e.g. `@if ready:  ` →
  `@if ready:`). Only directive lines; see below.
- **Final newline** — the file ends with exactly one `\n` (missing one is
  added; extra trailing blank lines are trimmed). An empty or whitespace-only
  source formats to 0 bytes (empty output file).

## What it preserves byte-for-byte

- **Body text** — including Markdown hard breaks (two trailing spaces on a prose
  line survive) and whitespace-only lines.
- **Interior blank lines** — blank runs anywhere in the document are never
  collapsed (the old blank-collapse rule was removed in v0.4.0).
- **Code fences and frontmatter internals** — blank-line structure inside them
  is untouched (only `CRLF` → `LF` applies).
- **`@message` / `@define` bodies** — copied completely verbatim, including
  trailing whitespace and even `CRLF`.

## Try it

```bash
# Show the file is already clean (exit 0, writes nothing)
mds fmt --check examples/formatting/demo.mds

# Format in place (default mode; no --write flag exists)
mds fmt examples/formatting/demo.mds

# Preview pending changes as a unified diff (writes nothing)
mds fmt --diff examples/formatting/demo.mds

# Format every .mds under a directory, INCLUDING _partials
mds fmt examples/formatting/

# Read-only gate for CI: exit 1 if anything under the tree would change
mds fmt --check examples/

# Filter mode: format from stdin to stdout (creates no file)
printf '@if ready:   \nGo\n@end\n' | mds fmt -
```

## What a diff looks like

Given a messy file with trailing whitespace on a directive line and extra blank
lines at the end, `mds fmt --diff` shows exactly what it would fix:

```diff
--- messy.mds
+++ messy.mds
@@ -1,8 +1,5 @@
-@define entry(kind, summary):
+@define entry(kind, summary):
 - **{{kind}}:** {{summary}}
 @end

 {{entry("Added", "New capabilities.")}}
-
-
-
```

(The first hunk line loses trailing spaces after the colon; the trailing blank
run at EOF collapses to a single final newline.)

## Exit codes

| Situation | Exit |
|-----------|------|
| Formatted OK / already clean / diff preview | `0` |
| `--check` found a file that would change, or a syntax / safety-gate error | `1` |
| File not found / not `.mds` / I/O / invalid UTF-8 | `2` |
| Source exceeds the size cap | `3` |

## Channel discipline

Formatted content (stdin filter mode) and `--diff` output go to **stdout**; all
status lines, summaries, and errors go to **stderr**. `-q` / `--quiet` suppresses
status and summaries but never errors, and never changes exit codes.

## The safety gate

Because `mds fmt` only applies provably output-preserving rules, valid input can
never trip the gate. If a formatter bug ever produced a divergence, the write is
refused with `mds::formatter_invariant` (naming the file) rather than silently
writing a file that compiles differently.
