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
in **fmt-clean**, so `mds fmt --check examples/formatting/demo.mds` exits `0`.

[`needs-formatting/`](needs-formatting/) holds a deliberately-unformatted
fixture. `mds fmt --check examples/formatting/needs-formatting/` exits `1`
**by design** — that is the point. The fixture compiles cleanly but has trailing
whitespace on two directive lines and extra blank lines at EOF, which the
formatter removes. See [needs-formatting/ (exits 1 by design)](#needs-formatting-exits-1-by-design).

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

All commands below are run from the repository root.

```bash
# Show the already-formatted demo is clean (exit 0, writes nothing)
mds fmt --check examples/formatting/demo.mds

# Show the deliberately-unformatted fixture would change (exit 1)
mds fmt --check examples/formatting/needs-formatting/

# Preview pending changes as a unified diff (writes nothing)
mds fmt --diff examples/formatting/needs-formatting/unformatted.mds

# Format every .mds under a directory, INCLUDING _partials
mds fmt examples/formatting/

# Read-only gate for CI: exit 1 if anything under the tree would change
mds fmt --check examples/

# Filter mode: format from stdin to stdout (creates no file)
printf '@if ready:   \nGo\n@end\n' | mds fmt -
```

## needs-formatting/ (exits 1 by design)

`needs-formatting/unformatted.mds` is a template that compiles cleanly
(`mds build examples/formatting/` exits `0`) but has two formatting issues
that `mds fmt` fixes automatically:

1. **Trailing whitespace on directive lines.** `@if audience == "technical":`
   and `@end` each have three trailing spaces. The formatter strips them
   (R4) because the parser discards trailing whitespace on directive lines
   before matching — removing it is provably output-preserving.

2. **Extra trailing blank lines at EOF.** Two blank lines follow `@end`.
   The formatter trims the file's trailing edge to exactly one `\n` (R2),
   matching the compiler's own trailing-edge normalisation.

Running `mds fmt --diff` on the file produces this output (trailing spaces on
the `-@if` and `-@end` lines are present but invisible in most renderers):

```console
$ mds fmt --diff examples/formatting/needs-formatting/unformatted.mds
--- examples/formatting/needs-formatting/unformatted.mds
+++ examples/formatting/needs-formatting/unformatted.mds
@@ -5,8 +5,6 @@
 
 # {{role}} checklist
 
-@if audience == "technical":   
+@if audience == "technical":
 Include code samples and API references.
-@end   
-
-
+@end
```

`--diff` writes to **stdout** and exits `0` (preview only, no files written).

### Idempotence

Running the formatter a second time produces no changes. Verify using the stdin
path so no file on disk is modified:

```console
$ mds fmt - < examples/formatting/needs-formatting/unformatted.mds | mds fmt --check -
```

Exit `0`, no output — the formatted result is already clean, so the second pass
finds nothing to change.

### Build check

```console
$ mds build examples/formatting/needs-formatting/
Compiled to examples/formatting/needs-formatting/unformatted.md
1 built, 0 failed
```

Exit `0` — formatting issues are invisible to the compiler. The fixture is valid
MDS; only `mds fmt` cares about trailing whitespace or extra blank lines.

## Stdin path

`mds fmt -` formats standard input and writes the result to standard output. No
file is created or modified:

```console
$ printf '@if ready:   \nGo\n@end\n' | mds fmt -
@if ready:
Go
@end
```

Trailing whitespace is stripped from the directive line; exactly one final
newline is emitted. The idempotence check above uses this path to verify a
second pass finds nothing to change.

## The safety gate

Every change the formatter applies is re-compiled and compared against the
original compiled output. If they ever diverged, the write would be refused
with `mds::formatter_invariant` — naming the file — rather than silently
writing a file that compiles differently.

Because every rule the formatter applies is **provably output-preserving**,
valid MDS source can never trip the gate. The gate exists to catch formatter
bugs: if a future change accidentally altered compiled output, the write would
be refused. Triggering it requires a bug in the formatter, not a bug in the
template — no fixture demonstrates it.

## Exit codes

| Situation | Exit |
|-----------|------|
| Formatted OK / already clean / diff preview | `0` |
| `--check` found a file that would change, or a syntax / safety-gate error | `1` |
| File not found / not `.mds` / I/O / invalid UTF-8 | `2` |
| Source exceeds the size cap | `3` |

`mds fmt --check examples/formatting/needs-formatting/` exits **`1` by design** —
`unformatted.mds` is intentionally left unformatted to demonstrate the formatter.
`mds fmt --check examples/formatting/` and `mds fmt --check examples/` also exit
**`1`** for the same reason.

## Channel discipline

Formatted content (stdin filter mode) and `--diff` output go to **stdout**; all
status lines, summaries, and errors go to **stderr**. `-q` / `--quiet` suppresses
status and summaries but never errors, and never changes exit codes.
