# Linting demo

`demo.mds` compiles cleanly (`mds build examples/linting/demo.mds`) but deliberately
trips four lint rules: **duplicate-import** (error, auto-fixable), **unused-import**,
**unused-variable**, and **redundant-else** (warnings). `_shared.mds` is the sibling
module it imports twice.

All commands below are run from the repository root.

## Human output

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
 17 │ Thanks for reading, {audience}.
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

```console
$ mds lint --format json examples/linting/demo.mds
```

```json
{"files":[{"diagnostics":[{"fixable":false,"help":"Remove the @else branch or make its content different from the @if body.","message":"The @else body is identical to the @if body — the conditional produces the same output regardless of the condition.","rule":"redundant-else","severity":"warn","span":{"length":3,"offset":463}},{"fixable":true,"help":"Remove the duplicate import. If different forms are needed (alias vs merge), consolidate into one import directive.","message":"Duplicate import: './_shared.mds' is imported more than once.","rule":"duplicate-import","severity":"error","span":{"length":7,"offset":74}},{"fixable":false,"help":"Remove the frontmatter key or reference it in the template body.","message":"Variable 'retries' is defined in frontmatter but never referenced in the body.","rule":"unused-variable","severity":"warn","span":{"length":7,"offset":25}},{"fixable":false,"help":"Remove the @import or use the alias with @include or as a qualified call (`alias.func(...)`).","message":"Import alias 'extra' from './_shared.mds' is never used.","rule":"unused-import","severity":"warn","span":{"length":7,"offset":74}}],"file":"demo.mds"}],"truncated":false,"version":1}
```

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

`--fix --diff` (and `--fix --check`) never write. Running plain `--fix` removes the
duplicate import line (which also clears the unused-import warning) and leaves the
two remaining warnings for you to resolve by hand.

## Exit codes

`0` clean · `1` warnings only · `2` any error-severity finding or analysis failure
(this demo exits `2`) · `3` resource limit exceeded.
