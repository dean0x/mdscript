# Linting demo

`mds lint` runs nine static-analysis rules over a template *without executing it*,
catching correctness and style problems that `mds check` does not. This directory
demonstrates the rules, the JSON output, the auto-fixer, and per-rule severity
configuration via `mds.json`.

`demo.mds` compiles cleanly (`mds build examples/linting/demo.mds`) but deliberately
trips four lint rules: **duplicate-import** (error, auto-fixable), **unused-import**,
**unused-variable**, and **redundant-else** (warnings). `_shared.mds` is the sibling
module it imports twice. `config-demo/` is a self-contained sub-example showing
`mds.json` rule overrides (see [Config demo](#config-demo-config-demo)).

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
```

## JSON output

`--format json` writes a single canonical object to **stdout** (keys sorted
alphabetically at every level); stderr stays empty. Pipe it straight into `jq`.

```console
$ mds lint --format json examples/linting/demo.mds
```

```json
{"files":[{"diagnostics":[{"fixable":false,"help":"Remove the @else branch or make its content different from the @if body.","message":"The @else body is identical to the @if body — the conditional produces the same output regardless of the condition.","rule":"redundant-else","severity":"warn","span":{"length":3,"offset":463}},{"fixable":true,"help":"Remove the duplicate import. If different forms are needed (alias vs merge), consolidate into one import directive.","message":"Duplicate import: './_shared.mds' is imported more than once.","rule":"duplicate-import","severity":"error","span":{"length":7,"offset":74}},{"fixable":false,"help":"Remove the frontmatter key or reference it in the template body.","message":"Variable 'retries' is defined in frontmatter but never referenced in the body.","rule":"unused-variable","severity":"warn","span":{"length":7,"offset":25}},{"fixable":false,"help":"Remove the @import or use the alias with @include or as a qualified call (`alias.func(...)`).","message":"Import alias 'extra' from './_shared.mds' is never used.","rule":"unused-import","severity":"warn","span":{"length":7,"offset":74}}],"file":"demo.mds"}],"truncated":false,"version":1}
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
re-verify gate as a real `--fix`, so the preview is honest. Running plain `--fix`
removes the duplicate import line (which also clears the unused-import warning) and
leaves the two remaining warnings for you to resolve by hand. When several fixes are
planned and one is declined by the gate, `--fix` applies the rest and reports
`N of M fixes applied`, writing the best state it could reach.

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
with exit `2`; an unknown *rule name* prints a `warning: unknown lint rule …` and is
ignored (forward-compatible).

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
