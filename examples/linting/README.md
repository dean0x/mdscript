# Linting demo

`mds lint` runs ten static-analysis rules over a template *without executing it*,
catching correctness and style problems that `mds check` does not. This directory
demonstrates the rules, the JSON output, the auto-fixer, and per-rule severity
configuration via `mds.json`.

`demo.mds` compiles cleanly (`mds build examples/linting/demo.mds`) but deliberately
trips four lint rules: **duplicate-import** (error, auto-fixable), **unused-import**,
**unused-variable**, and **redundant-else** (warnings). `_shared.mds` is the sibling
module it imports twice. `config-demo/` is a self-contained sub-example showing
`mds.json` rule overrides (see [Config demo](#config-demo-config-demo)).

`rules-tour.mds` is a second fixture that covers the five rules not demonstrated by
`demo.mds`: **duplicate-export** (error), **unreachable-branch** (error), **empty-block**
(warn), **unused-function** (warn), and **legacy-interpolation** (warn). Because it trips
two error-severity rules, `mds lint rules-tour.mds` exits **`2`** by design. It also
demonstrates `fix_edits` in a non-null state — visible in the JSON output for the
`legacy-interpolation` finding (see [rules-tour.mds](#rules-tour-rules-tourmds)).

All commands below are run from the repository root.

## The ten rules

| Rule | Default severity | Tier | `--fix` behavior |
|------|------------------|------|------------------|
| `duplicate-import`      | error | A | Auto-fixed — removes the duplicate `@import` line. |
| `duplicate-export`      | error | A | Auto-fixed — removes the duplicate `@export` line. |
| `unreachable-branch`    | error | A | Auto-fixed — removes the dead block or branch span. |
| `empty-block`           | warn  | A | Auto-fixed — removes the empty block or branch span. |
| `legacy-interpolation`  | warn  | A | Auto-fixed — rewrites `{x}` → `{{x}}` (migration helper). |
| `unused-import`         | warn  | B | Fixable only on a *standalone* file.† |
| `unused-function`       | warn  | B | Fixable only on a *standalone* file.† |
| `unused-variable`       | warn  | C | Never auto-fixed (report-only). |
| `redundant-else`        | warn  | C | Never auto-fixed (report-only). |
| `shadow-variable`       | off   | C | Never auto-fixed; off by default (see config demo). |

**Tiers.** Tier **A** fixes apply always (gated by a re-verify recompile); Tier **B**
fixes apply only when the file is *standalone*; Tier **C** rules are report-only.

† *Standalone* means the file has no `@import` directives and is not an
`@extends`/partial target. Tier B fixes are additionally gated by a recompile that
must produce byte-identical output. Note that a file which trips `unused-import`
necessarily *has* an import, so it is never standalone — such warnings clear as a
side effect of another fix (e.g. removing a duplicate import) rather than directly.

**Tier A block-spanning fixes.** `empty-block` and `unreachable-branch` auto-fix by
removing the complete block span (`@if…@end`, `@for…@end`, or `@define…@end`).
`unused-function` (Tier B, standalone) removes the whole `@define…@end` block the
same way. The reverify gate still applies fail-closed — if the edited source does not
recompile cleanly or produces different output, the fix is declined and the diagnostic
is left for you to resolve by hand. A few sub-cases (e.g. an `@if` with an empty
*then*-body but non-empty branches) emit no fix span and are always report-only.

## Human output

Human-readable diagnostics go to **stderr**; each message ends with a period.

```console
$ mds lint examples/linting/demo.mds
mds::lint::unused-variable

  ⚠ [unused-variable] Variable 'retries' is defined in frontmatter but never
  │ referenced in the body.
   ╭─[demo.mds:3:1]
 2 │ audience: developers
 3 │ retries: 3
   · ───┬───
   ·    ╰── Variable 'retries' is defined in frontmatter but never referenced in the body.
 4 │ ---
   ╰────
  help: Remove the frontmatter key or reference it in the template body.

mds::lint::duplicate-import

  × [duplicate-import] Duplicate import: './_shared.mds' is imported more than
  │ once.
   ╭─[demo.mds:6:1]
 5 │ @import "./_shared.mds" as shared
 6 │ @import "./_shared.mds" as extra
   · ───┬───
   ·    ╰── Duplicate import: './_shared.mds' is imported more than once.
 7 │
   ╰────
  help: Remove the duplicate import. If different forms are needed (alias vs
        merge), consolidate into one import directive.

mds::lint::unused-import

  ⚠ [unused-import] Import alias 'extra' from './_shared.mds' is never used.
   ╭─[demo.mds:6:1]
 5 │ @import "./_shared.mds" as shared
 6 │ @import "./_shared.mds" as extra
   · ───┬───
   ·    ╰── Import alias 'extra' from './_shared.mds' is never used.
 7 │
   ╰────
  help: Remove the @import or use the alias with @include or as a qualified
        call (`alias.func(...)`).

mds::lint::redundant-else

  ⚠ [redundant-else] The @else body is identical to the @if body — the
  │ conditional produces the same output regardless of the condition.
    ╭─[demo.mds:16:1]
 15 │
 16 │ @if audience == "developers":
    · ─┬─
    ·  ╰── The @else body is identical to the @if body — the conditional produces the same output regardless of the condition.
 17 │ Thanks for reading, {{audience}}.
    ╰────
  help: Remove the @else branch or make its content different from the @if
        body.
```

## JSON output

`--format json` writes a single canonical object to **stdout** (keys sorted
alphabetically at every level); stderr stays empty. Pipe it straight into `jq`.

```console
$ mds lint --format json examples/linting/demo.mds
```

```json
{"files":[{"diagnostics":[{"fix_edits":null,"fixable":false,"help":"Remove the frontmatter key or reference it in the template body.","message":"Variable 'retries' is defined in frontmatter but never referenced in the body.","rule":"unused-variable","severity":"warn","span":{"length":7,"offset":25}},{"fix_edits":null,"fixable":true,"help":"Remove the duplicate import. If different forms are needed (alias vs merge), consolidate into one import directive.","message":"Duplicate import: './_shared.mds' is imported more than once.","rule":"duplicate-import","severity":"error","span":{"length":7,"offset":74}},{"fix_edits":null,"fixable":false,"help":"Remove the @import or use the alias with @include or as a qualified call (`alias.func(...)`).","message":"Import alias 'extra' from './_shared.mds' is never used.","rule":"unused-import","severity":"warn","span":{"length":7,"offset":74}},{"fix_edits":null,"fixable":false,"help":"Remove the @else branch or make its content different from the @if body.","message":"The @else body is identical to the @if body — the conditional produces the same output regardless of the condition.","rule":"redundant-else","severity":"warn","span":{"length":3,"offset":465}}],"file":"demo.mds"}],"truncated":false,"version":1}
```

In directory mode the `file` keys are paths relative to the directory you passed
(e.g. `nested/deep/c.mds`).

## Preview the auto-fix

```console
$ mds lint --fix --diff examples/linting/demo.mds
--- examples/linting/demo.mds
+++ examples/linting/demo.mds
@@ -3,7 +3,6 @@
 retries: 3
 ---
 @import "./_shared.mds" as shared
-@import "./_shared.mds" as extra

 # Lint demo

```

`--fix --diff` (and `--fix --check`) never write, and both route through the same
re-verify gate as a real `--fix`, so the printed preview matches what `--fix` would
write. The preview exit code is `max(1, residual severity)`: 1 signals that fixes
are pending, rising to 2 when findings the fix cannot remove would still be
error-severity afterwards — a preview never exits lower than the `--fix` run it
predicts. Running plain `--fix` removes the duplicate import line (which also
clears the unused-import warning) and leaves the two remaining warnings for you to
resolve by hand. When several fixes are planned and one is declined by the gate,
`--fix` applies the rest and reports `N of M fixes applied`, writing the best state
it could reach.

## rules-tour (`rules-tour.mds`)

`rules-tour.mds` deliberately trips the five rules that `demo.mds` does not exercise.
It compiles cleanly (`mds build` exit `0`) and passes `mds fmt --check` (exit `0`).

### Human output

```console
$ mds lint examples/linting/rules-tour.mds
mds::lint::unused-function

  ⚠ [unused-function] Function 'helper' is defined but never exported or
  │ called.
    ╭─[rules-tour.mds:9:1]
  8 │
  9 │ @define helper():
    · ───────┬──────
    ·        ╰── Function 'helper' is defined but never exported or called.
 10 │ This function is defined but never exported or called.
    ╰────
  help: Export the function with @export or call it somewhere, or remove the
        definition.

mds::lint::duplicate-export

  × [duplicate-export] Duplicate export: 'greet' is exported more than once.
    ╭─[rules-tour.mds:14:1]
 13 │ @export greet
 14 │ @export greet
    · ───┬───
    ·    ╰── Duplicate export: 'greet' is exported more than once.
 15 │
    ╰────
  help: Remove the duplicate @export directive.

mds::lint::unreachable-branch

  × [unreachable-branch] @if condition is always false — the then-body is dead
  │ code.
    ╭─[rules-tour.mds:23:1]
 22 │
 23 │ @if "x" == "y":
    · ─┬─
    ·  ╰── @if condition is always false — the then-body is dead code.
 24 │ This branch is always-false dead code — unreachable-branch fires (error).
    ╰────
  help: Replace the constant condition with a variable or remove the dead
        branch.

mds::lint::empty-block

  ⚠ [empty-block] @if then-body is empty.
    ╭─[rules-tour.mds:27:1]
 26 │
 27 │ @if active:
    · ─┬─
    ·  ╰── @if then-body is empty.
 28 │ @end
    ╰────
  help: Add content inside the @if block or remove it.

mds::lint::legacy-interpolation

  ⚠ [legacy-interpolation] legacy single-brace interpolation `{name}` — use
  │ `{{name}}` (double braces)
    ╭─[rules-tour.mds:30:47]
 29 │
 30 │ Legacy single-brace interpolation fires here: {name}
    ·                                               ───┬──
    ·                                                  ╰── legacy single-brace interpolation `{name}` — use `{{name}}` (double braces)
    ╰────
  help: Run `mds lint --fix` to migrate to `{{x}}` syntax automatically.
```

`mds lint examples/linting/rules-tour.mds` exits **`2`** — the `duplicate-export` and
`unreachable-branch` findings are error-severity.

### JSON output and fix_edits

`legacy-interpolation` is a Tier A rule that rewrites source text rather than removing
whole lines. Its fix is expressed as a `fix_edits` array (not `fix_removals`), making it
the only finding in the tour with a non-null `fix_edits` value in the JSON:

```console
$ mds lint --format json examples/linting/rules-tour.mds
```

```json
{"files":[{"diagnostics":[{"fix_edits":null,"fixable":true,"help":"Export the function with @export or call it somewhere, or remove the definition.","message":"Function 'helper' is defined but never exported or called.","rule":"unused-function","severity":"warn","span":{"length":14,"offset":74}},{"fix_edits":null,"fixable":true,"help":"Remove the duplicate @export directive.","message":"Duplicate export: 'greet' is exported more than once.","rule":"duplicate-export","severity":"error","span":{"length":7,"offset":167}},{"fix_edits":null,"fixable":true,"help":"Replace the constant condition with a variable or remove the dead branch.","message":"@if condition is always false — the then-body is dead code.","rule":"unreachable-branch","severity":"error","span":{"length":3,"offset":489}},{"fix_edits":null,"fixable":true,"help":"Add content inside the @if block or remove it.","message":"@if then-body is empty.","rule":"empty-block","severity":"warn","span":{"length":3,"offset":587}},{"fix_edits":[{"end":657,"new_text":"{{name}}","start":651}],"fixable":true,"help":"Run `mds lint --fix` to migrate to `{{x}}` syntax automatically.","message":"legacy single-brace interpolation `{name}` — use `{{name}}` (double braces)","rule":"legacy-interpolation","severity":"warn","span":{"length":6,"offset":651}}],"file":"rules-tour.mds"}],"truncated":false,"version":1}
```

The `legacy-interpolation` entry is the only one with a non-null `fix_edits`: a single
atomic `TextEdit` that replaces the `{name}` span (bytes 651–657) with `{{name}}`.

### Preview the auto-fix

```console
$ mds lint --fix --diff examples/linting/rules-tour.mds
--- examples/linting/rules-tour.mds
+++ examples/linting/rules-tour.mds
@@ -6,12 +6,8 @@
 Hello, {{name}}!
 @end

-@define helper():
-This function is defined but never exported or called.
-@end

 @export greet
-@export greet

 # Rules tour

@@ -20,11 +16,8 @@
 **unreachable-branch** (error, auto-fixable), **unused-function** (warn), and
 **legacy-interpolation** (warn, auto-fixable via fix_edits).

-@if "x" == "y":
-This branch is always-false dead code — unreachable-branch fires (error).
-@end

 @if active:
 @end

-Legacy single-brace interpolation fires here: {name}
+Legacy single-brace interpolation fires here: {{name}}
```

`--fix --diff` (and `--fix --check`) never write. Running plain `--fix` applies the
four auto-fixable findings — the `unused-function` (Tier B, standalone), `duplicate-export`
and `unreachable-branch` (Tier A, line-removal), and `legacy-interpolation` (Tier A,
`fix_edits` rewrite). The `empty-block` warning is left for manual resolution in this run.

## Config demo (`config-demo/`)

`config-demo/` carries its own `mds.json` that overrides three rule severities:

```json
{
  "lint": {
    "rules": {
      "shadow-variable": "info",
      "unused-variable": "error",
      "redundant-else": "off"
    }
  }
}
```

- `shadow-variable` is **off by default** — this enables it at `info`.
- `unused-variable` is **promoted** from its default `warn` to `error`.
- `redundant-else` is **silenced** (`off`).

`loop-shadow.mds` compiles cleanly but trips all three: its `@for role in roles`
loop variable shadows the `role` frontmatter key, `deprecated_flag` is never
referenced, and its `@if`/`@else` bodies are identical.

```console
$ mds lint examples/linting/config-demo/loop-shadow.mds
```

surfaces `shadow-variable` as **info** (☞) and `unused-variable` as an **error**
(exit `2`); `redundant-else` stays silent because the config turned it off.

> **Config discovery is per-file in directory mode.** `mds.json` is found by
> walking *up* from **each input file** independently (cached per directory). This
> means a nested `mds.json` in a subdirectory applies to files in that subtree
> while the rest of the tree uses whatever config is found from their own walk-up.
> So the overrides above apply when you lint `config-demo/` (or the file) directly,
> **and also** when you lint the parent `examples/linting/` — the
> `config-demo/loop-shadow.mds` file picks up `config-demo/mds.json` automatically.
> Files outside `config-demo/` find no `mds.json` and use built-in defaults.

Config errors are strict: an unknown severity value or malformed JSON fails the run
with exit `2`. An unknown *rule name* is handled more leniently by `mds lint`: a
`warning: unknown lint rule …` is printed to stderr, the config still loads, lint
continues, and the unknown rule is not enforced — it is skipped
(forward-compatible: a config naming a rule from a newer release warns instead of
failing on an older binary). Pass `--quiet` to suppress it. Other commands
(`mds build`, `mds check`, `mds fmt`, `mds watch`) also read `mds.json` but do
not emit the unknown-rule warning.

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Clean — no warnings or errors (advisory `info` findings do not raise it). |
| `1` | Warning-severity findings only. |
| `2` | Any error-severity finding, an analysis failure (parse/resolve/config), or a usage error. |
| `3` | A resource limit was exceeded. |

`mds lint examples/linting/demo.mds` exits **`2`** by design — the duplicate import
is an error-severity finding. With `--fix`, the *residual* findings after fixing
determine the exit code.

`mds lint examples/linting/rules-tour.mds` also exits **`2`** — `duplicate-export`
and `unreachable-branch` are both error-severity.

`mds lint examples/linting` (directory mode) exits **`2`** and reports
`1 clean, 0 with warnings, 3 with errors, 0 resource-limited`.

`mds build examples/linting` exits **`0`** — all files compile cleanly regardless of
lint findings. `mds fmt --check examples/linting` exits **`0`** — all source files
are formatter-clean.
