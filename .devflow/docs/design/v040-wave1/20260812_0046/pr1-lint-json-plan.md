# fix(lint)!: unify stdin label, sort diagnostics by offset, anchor unused-import spans at the unused name

Issues: #211, #202, #203

## Implementation Plan

# PR1 — Lint JSON wire contract (#211, #202, #203)

> **Challenge-review amendments are marked `[CR]`.** Every line number below was re-verified against `113f472`; corrections are marked `[CR-fix]`.

## 0a. USER RULING ON #211 (2026-08-12) — DECIDED, NOT OPEN

**The stdin label is uniformly `<stdin>`.** All four CLI diagnostic contexts converge on the
literal string `<stdin>`:

| Context | Today | After | Citation status |
|---|---|---|---|
| build / check — **site TBD, see the retraction below** | `<source>` | `<stdin>` | **RETRACTED** — the cited path does not exist |
| lint — `crates/mds-cli/src/lint.rs:682, :718, :727` **including the JSON `"file"` key, a published wire surface** | `input.mds` | `<stdin>` | **CONFIRMED at `113f472`** |
| fmt — `crates/mds-cli/src/fmt.rs:133, :136` | `<stdin>` | `<stdin>` (no change) | **CONFIRMED at `113f472`** |

### CITATION RETRACTION — `crates/mds-cli/src/formatter.rs:120` does not exist

**The ruling's build/check citation is withdrawn.** It is recorded here rather than silently
dropped so no reader re-derives from it. Verified at `113f472`: `crates/mds-cli/src/` contains
exactly `build.rs`, `fmt.rs`, `lint.rs`, `main.rs`, `output.rs`, `watch.rs` — **there is no
`formatter.rs` in the CLI crate.** The only `formatter.rs` in the workspace is
`crates/mds-core/src/formatter.rs`, which is the *source formatter* and is unrelated to
diagnostic labels. **No line 120 of any file was the build/check `<source>` emission site.**

**The ruling itself is unaffected** — it fixes the user-visible label, and the label for
build/check is still `<stdin>`. Only the pointer to where that change lands was wrong. Finding
the real site is an explicit implementation task; see §5 step 1a. **Do not substitute a guessed
line number for the retracted one.**

What *is* verified about the `<source>` value, as a starting point and nothing more:
`crates/mds-core/src/resolver.rs:199` defines `const SOURCE_LABEL: &str = "<source>"`
(crate-private), assigned as `ctx.file_str` at **three** sites — `:508`, `:617`, `:642` — and
locked by a display-label test at `crates/mds-core/src/resolver_tests.rs:2917-2920`. That is
the origin of the value, **not** the CLI boundary where it reaches the user.

**Rationale, recorded verbatim as the decision rationale:**

> `input.mds` is ambiguous because a real file can legitimately be named `input.mds`, so a
> JSON consumer reading `"file": "input.mds"` cannot tell stdin from a real file of that
> name. Angle brackets mark a pseudo-source unambiguously. `<stdin>` also matches what fmt
> already does. The project has effectively zero users and v0.4.0 is already a breaking
> release, so "least breakage" carries little weight while "right contract forever" carries
> a lot.

### THE MECHANISM IS ALREADY IN THE TREE — extend the boundary relabel, do not touch the constant

**This is the load-bearing architectural finding, verified at `113f472`. It changes PR1's
implementation approach; it does not change the ruling.**

**1. `input.mds` is not a CLI literal — it is a shared public constant.**
`crates/mds-core/src/sourcemap.rs:79` defines `pub const STRING_SOURCE_MAP_LABEL: &str =
"input.mds"`, re-exported at `crates/mds-core/src/lib.rs:73`. Verified consumers:

| Consumer | Role |
|---|---|
| `crates/mds-cli/src/lint.rs:682, :718, :727` | `named_source` name for the miette frame — **the lint sites the ruling changes** |
| `crates/mds-cli/src/build.rs:12` (import), `:992` (compare) | the existing stdin relabel |
| `crates/mds-wasm/src/lib.rs:69` | `DEFAULT_FILENAME` — **the WASM virtual-FS default filename** |
| `crates/mds-core/src/lib.rs:1219` | the string-lint entry point passes it to `lint::lint_source`, so it becomes `diag.file` |
| `crates/mds-core/tests/api_surface.rs:1420-1431` | pins the value; its own message says *"changing it requires updating every surface that uses it"* |

**Changing the constant is the wrong lever.** It would move the WASM default filename and the
cross-surface `sources[0]` parity that `crates/mds-core/tests/source_map_vfs.rs:1126-1135`
exists to enforce, in service of a CLI display change. That is a regression, not an
implementation.

**2. The relabel-at-the-output-boundary pattern already exists.**
`crates/mds-cli/src/build.rs` `apply_source_map_file_label` maps `STRING_SOURCE_MAP_LABEL` →
`"<stdin>"` for stdin builds — rustdoc at `:973-974` (*"The `<stdin>` relabel: maps
`STRING_SOURCE_MAP_LABEL` (`"input.mds"`) → `"<stdin>"` for stdin builds"*), applied at
`:992-993`. Its own doc calls it *"a pure label swap — no path logic."* **PR1 extends this
established pattern to the lint JSON and diagnostic paths. It does not invent one.**

**3. Tests already enforce the sentinel on the source-map surface — a positive control that is
already in the tree.** `crates/mds-cli/tests/cli_source_map.rs:1048-1059` asserts stdin
`sources[]` **must** contain `<stdin>` and **must never** contain `"input.mds"` or `"<source>"`
(AC-FUNC-12), with companions at `:635-648` and `:1193-1196`. These need no edit; they are
existing proof that the sentinel is the repo's convention and that an assertion of this exact
shape can fail when the value is wrong.

**Approach, therefore: extend the existing boundary relabel to the lint JSON and diagnostic
paths. Do not change `STRING_SOURCE_MAP_LABEL`.**

**This reconciles the ruling with this PR's own challenge recommendation.** The disagreement
was only ever about the **user-visible label** — now settled as `<stdin>` by the user. It was
never about the **mechanism**: relabelling at the CLI output boundary while leaving the core
constant intact is what the tree already does for source maps, and it is what makes the ruling
implementable without collateral damage. The challenge agent's recommendation is superseded as
the *decision of record* on the label; its *mechanism* is the shipped one, and it was right.

**Consequences for this plan:**

- **AD-211-5 is DECIDED, not open.** The analysis-failure leg emits `<stdin>` like every
  other stdin diagnostic context. Open decision 3 is closed. AC-P1-07 is concrete. Its own
  emission site is verified — `emit_analysis_failure_json_or_stderr`
  (`crates/mds-cli/src/lint.rs:1421-1436`) — unlike the retracted build/check citation.
- **Scope:** the ruling names the four CLI diagnostic contexts. It names no `crates/mds-core`
  site and no binding surface, so **AC-P1-05, AC-P1-06 and AC-P1-24 stand unchanged** —
  `STRING_SOURCE_MAP_LABEL` stays `"input.mds"` and stays public, the WASM virtual-FS entry
  key at `mds-wasm/src/lib.rs:69` is untouched, and napi/WASM/Python keep reporting
  `input.mds` for string-source input. **AC-P1-28 makes the WASM half explicit**, because it
  is the precise regression the wrong lever would cause.
- Open decisions **2** (alias-import span anchoring, AC-P1-17) and **4** (`LintResult::new`
  sort placement, AD-202-1b) are unrelated to #211 and **remain open**.

**Recommendations superseded by this ruling — reasoning preserved, labelled not-taken:**

- The `preference-auto-resolve` agent recommended **Option B, `input.mds` everywhere**
  ("most descriptive, least surprising, aligns with lint JSON semantics"). **NOT TAKEN** —
  the user's ambiguity rationale is a direct rebuttal of it.
- This PR's challenge agent recommended **"core keeps `input.mds`; the CLI remaps to
  `<stdin>` at every render boundary"** (its own option C). **NOT TAKEN as the
  recommendation of record — superseded.** Its evidence is retained below in AD-211-1 as
  supporting evidence for the shipped `<stdin>` sentinel, not as an option under
  consideration.

## 0. Corrections to the issue text (verified at 113f472)

1. **`crates/mds-cli/src/formatter.rs` does not exist.** Verified — the CLI crate contains only `build.rs`, `fmt.rs`, `lint.rs`, `main.rs`, `output.rs`, `watch.rs`. Render helpers live in `crates/mds-cli/src/output.rs` **(2,156 lines `[CR-fix]`, not 2,271)**. `crates/mds-core/src/formatter.rs` is the *source formatter*, unrelated to diagnostics. **This same bad citation was carried into the #211 ruling text and is formally retracted in §0a — the build/check emission site is unknown and must be located, not guessed (§5 step 1a).**
2. **#203's premise is inaccurate.** Verified: `unused_import.rs` `make_diag` sets `length: "@import".len()` — 7 bytes on the keyword, not the whole line. The AC still holds; the diff is smaller than the issue implies.
3. **`[CR]` There are FIVE stdin conventions, not three or four.** `input.mds` (core), `<stdin>` (fmt/check/build), `<source>` (resolver), bare `stdin` (`lint.rs:662,667,706`), and — the one everyone missed — **`<source>` reaching the user through the lint ANALYSIS-FAILURE path**. All five user-visible CLI legs collapse to `<stdin>` under the §0a ruling; see §3 AD-211-5 (**DECIDED**).

## 1. Approach overview

All three issues mutate the same JSON object produced by `LintResult::to_canonical_json()` (`crates/mds-core/src/lint/diagnostic.rs:717`). Land in dependency order inside one PR: **#211 first** (it decides the `file` key that #202/#203 are asserted through), **#202 second**, **#203 third**.

Unifying insight: every surface reads a `LintResult`. Fix ordering **in core**, fix the label **at the CLI output boundary**, and all surfaces stay in parity by construction.

**The #211 half is an EXTENSION of an existing mechanism, not new machinery (§0a).** `crates/mds-cli/src/build.rs` `apply_source_map_file_label` (rustdoc `:973-974`, applied `:992-993`) already maps `STRING_SOURCE_MAP_LABEL` → `"<stdin>"` at the output boundary for stdin builds, and `crates/mds-cli/tests/cli_source_map.rs:1048-1059` already forbids `input.mds` and `<source>` on that surface. PR1 extends the same pure-label-swap pattern to (a) the lint JSON `files[].file` key, (b) the lint human miette frames (`lint.rs:682/:718/:727`), (c) the bare-`stdin` fix-path messages (`lint.rs:662/:667/:706`), and (d) the analysis-failure envelope (`lint.rs:1421-1436`). **`STRING_SOURCE_MAP_LABEL` is not modified** — it is a shared public constant whose consumers include the WASM `DEFAULT_FILENAME` (`crates/mds-wasm/src/lib.rs:69`), and changing it is the wrong lever (AC-P1-05, AC-P1-28).

## 2. Verified call graph (re-confirmed @ 113f472)

| Location | What it does | Status |
|---|---|---|
| `mds-core/src/lint/diagnostic.rs:717` | `to_canonical_json()` — sole wire producer | ✓ |
| `…:721-725` | groups by `diag.file` into a `BTreeMap`; insertion order preserved within a group — this is #202 | ✓ |
| `…:725` | `<unknown>` fallback key | ✓ |
| `…:756-764` | emits `rule, severity, message, help, fixable, span, fix_edits` | ✓ **`[CR]` the rustdoc schema at :683-704 omits `fix_edits` — pre-existing drift, fix here** |
| `…:774-782` | `file` key, WIRE-sanitized (#176 / CWE-150) | ✓ |
| `…:820` | `LintResultBuilder::push` — `MAX_DIAGNOSTICS` (=1,000, `limits.rs:94`) truncation | ✓ |
| `…:829` | `LintResultBuilder::build` — choke point for #202 | **`[CR-fix]` :829, plan said :828** |
| `…:659` | `LintResult::new` — public ADR-010 constructor, doctest at :649-656 | ✓ |

### Surfaces
`mds-cli/src/lint.rs:1398` (`emit_result`) → `to_canonical_json`; dir envelope `lint.rs:1029-1037`; `accumulate_result_json` `lint.rs:1439`. Human: `lint.rs:293/310-312` iterates `result.diagnostics` directly. napi `mds-napi/src/lib.rs`. WASM `mds-wasm/src/lib.rs`. Python `mds-python/src/lib.rs` + hand-written mirror at `:559-600` (alphabetical key order, documented byte-identical to `to_canonical_json`).

### Stdin label chain — `[CR]` amended with the missing fifth leg
| Location | Value today |
|---|---|
| `mds-core/src/sourcemap.rs:79` | `STRING_SOURCE_MAP_LABEL = "input.mds"` |
| `mds-core/src/lib.rs:1212` | `lint_str_with` → `lint_source(source, STRING_SOURCE_MAP_LABEL, …)` — sets `diag.file` for stdin |
| `mds-wasm/src/lib.rs:69` | `DEFAULT_FILENAME = mds::STRING_SOURCE_MAP_LABEL` — **virtual-FS entry key** |
| `mds-cli/src/lint.rs:682,718,727` | human `named_source` name = `input.mds` |
| `mds-cli/src/lint.rs:662,667,706` | bare `stdin` |
| `mds-cli/src/fmt.rs:133,136,144` | `<stdin>` |
| `mds-cli/src/main.rs:281` | `OK: <stdin>` |
| `mds-cli/src/build.rs:964-993` | already remaps `STRING_SOURCE_MAP_LABEL` → `<stdin>` |
| **`mds-core/src/resolver.rs:199` `[CR]`** | **`SOURCE_LABEL = "<source>"`, set as `ctx.file_str` at :617/:642 in `resolve_source_intrinsic` — which `lint_str_with` calls as its CHECK GATE. Surfaces to the user via `emit_analysis_failure_json_or_stderr` (`lint.rs:1421-1436`). THE MISSING LEG — remapped to `<stdin>` at that CLI boundary per the §0a ruling (AD-211-5).** |

## 3. Key design decisions

Every decision ships as an `AD-###` rustdoc block **at the call site** (hard AC).

### AD-211-1: USER RULING (§0a) — every CLI diagnostic context emits the single sentinel `<stdin>`.
**Decided by the user 2026-08-12.** The rationale of record is the ambiguity argument quoted verbatim in §0a: a real file can legitimately be named `input.mds`, so `"file": "input.mds"` cannot tell a JSON consumer stdin from a real file of that name; angle brackets mark a pseudo-source unambiguously; and `<stdin>` is already what fmt does.

Supporting evidence independently re-verified at `113f472`, retained because it tells the implementer *where* the sentinel is applied: `input.mds` is a functional VFS key (`mds-wasm/src/lib.rs:69`) and a pinned public constant (`api_surface.rs::string_source_map_label_is_in_public_api`), so it is not rewritten in core; `cli_source_map.rs:1048-1061` already forbids `input.mds` and `<source>` in stdin `sources[]` and requires `<stdin>`, making the sentinel the established repo convention; `to_canonical_json` already emits the `<unknown>` sentinel in the same key (`diagnostic.rs:725`), so a `<…>` value is not a novel shape for machine consumers; `build.rs:964-993` already implements exactly this remap for source maps.

**Rule:** every user-visible CLI emission of stdin's source identity — human diagnostics, JSON `files[].file`, fix-preview status lines, diff headers, source-map `sources[]`, and the analysis-failure envelope — is the single sentinel `<stdin>`. The remap is applied at the CLI render boundary; `crates/mds-core` continues to carry `input.mds` as an entry key, which the ruling does not name and does not change. **Core never emits `<stdin>`.**

> **Superseded framings, retained not-taken:** the `preference-auto-resolve` agent's Option B (`input.mds` everywhere) and this PR's challenge agent's option C (framed as "core keeps `input.mds`; the CLI remaps"). Neither is the decision of record; the user ruling above is.

### AD-211-2: fix the bare `stdin` fourth convention here. `lint.rs:662,667,706` → `<stdin>`.

### AD-211-3: centralize as `pub(crate) const STDIN_DISPLAY_LABEL: &str = "<stdin>";` in `crates/mds-cli/src/output.rs`. Replaces 5 existing literals + 5 new. A constant, not a new abstraction.
**Includes the one already in `build.rs`:** the existing relabel hardcodes the string literal `"<stdin>"` at `build.rs:993`. Point it at the new constant so the CLI has exactly one definition of the sentinel. This is the only edit `apply_source_map_file_label` needs — its behaviour is already correct and `cli_source_map.rs:1048-1059` already guards it.

### AD-211-4: reuse `set_diag_display_path` (`lint.rs:231`), do not add a remap path.
One call in `run_lint_stdin` after the `lint_str_with` at `lint.rs:646`. **`[CR]` Safety proof strengthened:** `crates/mds-core/src/lint/fix.rs` contains **zero** reads of `diag.file`, so relabelling upstream of `preview_fixes`/`plan_and_apply_fixes` cannot perturb fix planning. **`[CR]` Scope correction:** the plan claimed this "fixes the JSON `file` key for all three stdin branches" — but `--fix` + `--format json` + stdin is a **hard usage error, exit 2** (`lint.rs:136-146`, AC-F-22b). Only the report-only branch ever emits stdin JSON. The single call is still correctly placed; the claim was overstated, and the Tester must know the fix-preview assertions are human-mode only.

### `[CR]` AD-211-5: the analysis-failure label — **DECIDED, `<stdin>`.**
`lint_str_with` runs `resolve_source_intrinsic` as a check gate before linting; that path sets `ctx.file_str = SOURCE_LABEL = "<source>"`. A stdin lint of a source with a compile error therefore renders `<source>:3:1` today, and `<source>` may appear inside `error.message` in the JSON envelope.

**Ruling (§0a):** this leg emits `<stdin>` like every other stdin diagnostic context — it is precisely the `<source>` → `<stdin>` change the user's first bullet names. Open decision 3 is closed.

**Write it as a rule about the envelope, not about stdin specifically.** `emit_analysis_failure_json_or_stderr` (`lint.rs:1421-1436`) is reached by every `MdsError::Io` config/analysis failure (existing sites `lint.rs:637, :757, :924`), so the `AD-211-5` rustdoc block at the emitting site must state the general rule — *this envelope labels a stdin source as `<stdin>` and never as `<source>` or `input.mds`* — so any later error travelling the same path inherits it instead of inventing a second convention.

### AD-202-1: sort in core at the builder choke point.
One private helper called from `LintResultBuilder::build` (`:829`) **and** (pending open decision 4) `LintResult::new` (`:659`). Sorting only inside `to_canonical_json` would leave the CLI human path (`lint.rs:293`) and the Python mirror unsorted — **avoids PF-007**, whose lesson is that per-surface goldens cannot prove parity.

**Sort key:** `(diag.file, diag.span.map(|s| s.offset))`, `span: None` **last**. `None` verified reachable: `unused_variable.rs:79` builds the span from `fv.approx_offset.map(...)`. Use `sort_by` on **borrowed** fields — no `String` allocation in the comparator (AC-P1-22).

**Stability is load-bearing:** the **stable** `sort_by` keeps ties in rule-execution order, deterministic because `run_rules` (`lint/mod.rs:118-131`) is a fixed 10-call sequence (verified).

### `[CR]` AD-202-1b (NEW): `LintResult::new` is a PUBLIC constructor.
Sorting inside it silently reorders an external caller's deliberately-ordered vec. That is a semantic change to a published ADR-010 construction path and belongs in the CHANGELOG BREAKING entry — or the sort lives only in `build()`. **Open decision 4.** Note the doctest at `:649-656` uses an empty vec and passes either way, so `cargo test --doc` will not catch the difference.

### AD-202-2: sorting happens AFTER truncation, deliberately.
`push` enforces the 1,000 cap in rule-execution order; `build()` reorders the retained set but never changes *which* are retained. A reader will assume "sorted by offset" implies "the first N by offset" — it does not, and cannot without buffering past the cap. Document at the call site; pin with AC-P1-12.

### AD-202-3: the fix pipeline is order-independent — verified.
`plan_fixes_with_options` iterates `lint_result.diagnostics` at `fix.rs:338` but re-sorts its own edits at `fix.rs:361` by `(start ASC, end DESC)` before `dedup_contained_or_identical` (`:367`) and `has_overlapping_edits` (`:372`). **`[CR]` Plus: `fix.rs` never reads `diag.file` at all.** Residual: among edits with identical `(start,end)` the stable sort retains input order and dedup keeps the first — reachable only if two rules emit identical ranges with different `new_text`, and only `legacy_interpolation` emits non-empty `new_text`. Pin with T-202-3 rather than asserting it.

### AD-203-1: per-name offsets come from the parser, not a source re-scan.
Rejected: re-scanning from `imp.offset` breaks on prefix names, on a name occurring in the path, and manufactures the PF-012 failure class (in-bounds, wrong token, plausible caret). Chosen: compute in `parse_import_directive` (`parser_helpers.rs:807-840`), which already receives `(directive, offset)`.

**Soundness (verified end to end):** `scan_directive` (`lexer.rs:205-219`) has precondition `is_line_start() && chars[pos]=='@'`, emits `Token::Directive(line, byte_pos(pos))` with only a trailing `\r` stripped. `parse_directive` (`parser.rs:346`) does `dir.trim()`; because the token always begins with `@`, the leading half is a no-op. Therefore `trimmed` byte 0 **is** source byte `offset`.

**`[CR-fix]` The delta formula in the original plan was wrong.** `parser_helpers.rs:808` is `directive.trim_start_matches("@import").trim()` — a **both-ends** trim. `directive.len() - after_kw.len()` over-counts by any TRAILING whitespace, shifting every name offset right. Use **`trim_start` only**: `directive.len() - directive.trim_start_matches("@import").trim_start().len()`. Reproducer: `@import { a } from "./l.mds"   `.

**`[CR]` The stated MOTIVATION was also wrong** (keep the computed delta, fix the reason): `is_directive_token` (`parser_helpers.rs:1459-1463`) admits `@import` only when followed by ` `, `\t`, `{`, or EOL — so `@import@import` never reaches the parser and the repeat-strip path is unreachable. Record the real reason (trailing-whitespace correctness) or a future reader will "simplify" it back to `7`.

**`[CR]` GUARANTEED INDEX DESYNC — the largest correctness gap.** `parser_helpers.rs:816-821` builds `names` as `split(',').map(trim).filter(non-empty).collect()`. The filter runs AFTER the split, so a naive per-segment offset vector desyncs for `@import { a, , b }` (3 segments, 2 names) and `@import { a, b, }` (3 segments, 2 names) — both parse fine today. Under desync, indices SHIFT and `b` silently anchors at `a`'s offset: in-bounds, plausible, wrong — exactly PF-012, and AD-203-3's fallback does not save it. **The offset vector MUST be produced by the same filter+trim pipeline in a single pass** (push name and offset together, or build `Vec<(String, usize)>` internally and unzip). Pinned by AC-P1-15.

**`[CR]` Bound on the UTF-8 risk:** `is_valid_identifier` (`parser_helpers.rs:1470-1474`) is ASCII-only and `parse_import_directive:824-828` rejects anything else, so names are always ASCII and `name.len()` is a safe span length. Multi-byte content can only shift the BASE offset — T-203-5 tests the base-offset chain, not the name span.

### AD-203-2: the AST change is not a public API change.
`crates/mds-core/src/lib.rs:43` declares `pub(crate) mod ast;`, so `ImportDirective` is crate-private and **ADR-010 does not govern it**. **`[CR]` Same for `ImportFact`:** although declared `pub struct`, `crates/mds-core/src/lint/mod.rs:30` declares `pub(crate) mod facts;`, so it is crate-private too — the plan asserted ADR-010 non-applicability without checking this.

**`[CR-fix]` Consumer list was incomplete.** The plan listed six sites and claimed completeness. It missed **`crates/mds-core/src/lint/rules/structural_eq.rs:175` and `:180`**. Full verified list: `parser_helpers.rs:836` (sole construction site), `resolver.rs:1923`, `resolver/inheritance.rs:57`, `lib.rs:1373`, `parser_tests.rs:89`, `facts.rs:430`, **`structural_eq.rs:175,180`**. All but the construction site use `..`, so the addition is contained — but structural equality returns `n1 == n2 && p1 == p2` and deliberately IGNORES offsets (mirroring the `end_offset` doc at `ast.rs:338-341`). `name_offsets` MUST stay excluded or `duplicate-import` changes behavior. Pinned by AC-P1-18.

Carry `name_offsets: Vec<usize>` index-aligned with `names`, on `ImportDirective::Selective` and `ImportFact`, threaded through `collect_import_fact` (`facts.rs:430-441`). Rejected `Vec<ImportName>` / `Vec<(String,usize)>` as the public shape: cleaner in principle, but it touches every `names` consumer. Document the index-alignment invariant on both fields.

### AD-203-3: degrade gracefully on desync, never mis-anchor.
`imp.name_offsets.get(i).copied().unwrap_or(imp.offset)` + `debug_assert_eq!` on lengths. Per **PF-005** the `debug_assert` is dev feedback only — the unconditional fallback is the real guard. **`[CR]` This guard is necessary but NOT sufficient for the empty-segment case above, where indices shift rather than run short.**

### AD-203-4: only the Selective branch changes (pending open decision 2 on aliases).
`make_diag` gains a `length` parameter. **`[CR]`** Its rustdoc currently reads "The span always covers the `@import` keyword (length = 7), so `offset` is the only caller-supplied span parameter" — #203 falsifies both halves. Rewrite to the end-state; no "used to be 7" tombstone (project rule: leave the end-state, not the transition).

### `[CR]` AD-203-5 (NEW, risk retired with evidence): no `@extends` coordinate-space hazard.
I checked whether inheritance could feed the linter a MERGED module, putting parent-file offsets in a child-file diagnostic — a textbook PF-012 mis-anchor that narrowing the span would worsen. **It cannot.** `lint_source` (`lint/mod.rs:69-81`) re-parses the entry source independently (`lexer::tokenize` → `parse_with_ctx`) and never consults the resolver's merged module. Every offset the linter sees is in the entry source's own coordinate space. Recorded so nobody re-litigates it.

## 4. Affected files

**Core (WASM-reachable):** `lint/diagnostic.rs` (sort helper; `build` :829, `new` :659; AD-202-1/1b/2 rustdoc; **`[CR]` fix the `fix_edits`-missing schema rustdoc at :683-704**) · `lint/rules/unused_import.rs` (Selective span, `make_diag` signature + rustdoc; AD-203-4) · `lint/facts.rs` (`ImportFact.name_offsets`; `collect_import_fact` :430-441) · `ast.rs` (`Selective.name_offsets`; AD-203-2) · `parser_helpers.rs` (offset arithmetic :807-840; AD-203-1 soundness rustdoc)

**CLI:** `output.rs` (`STDIN_DISPLAY_LABEL`; AD-211-1/3) · `lint.rs` (`set_diag_display_path` call; labels :662,:667,:682,:706,:718,:727; AD-211-4 rustdoc on :231; **`[CR]` AD-211-5 at :1421**) · `fmt.rs` :133,:136,:144 · `main.rs` :281 · `build.rs` :993

**Tests / docs:** `cli_lint.rs` `stdin_lint_diagnostic_includes_code_frame` :1266-1280 (flip `input.mds` → `<stdin>`) · `unused_import.rs` unit tests · **`[CR]` canaries that must stay green UNEDITED: `api_surface.rs::string_source_map_label_is_in_public_api`, `cli_source_map.rs:1048-1061`, `mds-python/tests/test_lint.py:161`, `test_parity.py:150,258`** (the last two deliberately assert `input.mds` on binding surfaces — Option C preserves them) · `packages/mds/__test__/lint.spec.mjs` (ordering parity) · `CHANGELOG.md` BREAKING · `README.md` if it documents the lint JSON `file` key · **`[CR]` `structural_eq.rs` — verify-only, no edit expected**

## 5. Implementation sequence

0. **Baseline WASM raw bytes** — `npm run build -w @mdscript/mds-wasm`. Budget 850,000 (`ci.yml`); wave/v0.4.0-wave1 CI baseline 821,662 (~3.3% headroom; local build measured 820,305 at the same commit — CI uses Binaryen v129, local uses an older toolchain). **`[CR]` Run in the PRIMARY CHECKOUT — `pkg/` is generated and gitignored, so an isolated worktree has nothing to measure and the check passes vacuously (PF-016).**
1. **#211**: `STDIN_DISPLAY_LABEL`; swap 5 literals **including the hardcoded `"<stdin>"` at `build.rs:993`**; `set_diag_display_path` in `run_lint_stdin`; fix :662/:667/:706; repoint :682/:718/:727; **apply the AD-211-5 ruling** (`<source>` → `<stdin>` at `emit_analysis_failure_json_or_stderr`, written as an envelope-wide rule). Update `cli_lint.rs:1266-1280`. **Do NOT edit `crates/mds-core/src/sourcemap.rs:79`.** Gate.

   **1a. LOCATE the live build/check `<source>` emission site — an explicit task, not an assumption.** The ruling's `crates/mds-cli/src/formatter.rs:120` citation is **retracted** (§0a); that file does not exist and no replacement line number has been established. **Do not guess one.** Before writing any code for this leg:
   - Reproduce it: run `mds check -` and `mds build -` on a stdin source that fails resolution, and capture the exact rendered output that contains `<source>`.
   - Trace it from the reproduction back to the boundary where the CLI renders it. Verified starting point, and *only* a starting point: `crates/mds-core/src/resolver.rs:199` `const SOURCE_LABEL: &str = "<source>"` (crate-private), assigned as `ctx.file_str` at `:508`, `:617`, `:642`, and locked by `crates/mds-core/src/resolver_tests.rs:2917-2920`.
   - Record the located site, with its verified line number, in the PR body and in the `AD-211-5` rustdoc block. If the reproduction shows build/check never surfaces `<source>` for stdin, say so explicitly and scope the leg out in writing — an unreproducible leg must not be "fixed" speculatively.
   - Apply the relabel at that boundary using the same pure-label-swap shape as `apply_source_map_file_label`. **`SOURCE_LABEL` itself must not change** — `resolver_tests.rs:2917-2920` locks it, and it is `ctx.file_str` for non-stdin paths too.
2. **#202**: sort helper + call sites. Gate — this is where a hidden order-dependence surfaces across 590+ tests.
3. **#203**: parser offsets → `ast` → `facts` → rule. Gate. **`[CR]` Build the #202 fixture from rules #203 does NOT touch (`duplicate-export`, `unused-variable`, `legacy-interpolation`) so step 3 does not invalidate step 2's expected offsets.**
4. **Cross-surface differential** (PF-007) — **`[CR]` compare `files[].diagnostics[]` with the `file` key EXCLUDED.** Full byte-identity including `file` is FALSE by construction under Option C (CLI `<stdin>` vs bindings `input.mds`), as `test_parity.py:150` already documents. Assert the `file` key separately per surface.
5. **Re-measure WASM**, then the full §8 pipeline.

## 6. Test plan

See the structured `testPlan` (AC-P1-01 … AC-P1-28). Local IDs use a non-`#` prefix (**PF-010**). New coverage added by this review: the empty-segment/trailing-comma desync (AC-P1-15), trailing directive whitespace (AC-P1-16d), the escaping positive control (AC-P1-20, PF-013/ADR-009), `files[]` array ordering (AC-P1-10), truncation-vs-sort semantics (AC-P1-12), `structural_eq` neutrality (AC-P1-18), the corrected parity scope (AC-P1-24), and an explicit performance bound (AC-P1-22). Added by the 2026-08-12 #211 ruling: the concrete analysis-failure label (AC-P1-07, formerly blocked) and the ruling-pinned wire-key pair (AC-P1-26 positive / AC-P1-27 negative). Added by the citation/architecture correction: **AC-P1-28**, pinning that `STRING_SOURCE_MAP_LABEL` and the WASM `DEFAULT_FILENAME` are untouched — the regression that changing the constant instead of extending the output-boundary relabel would cause.

## 7. Risks and mitigations

| # | Risk | Mitigation |
|---|---|---|
| R1 | **WASM budget** — 3.5% headroom, PR touches WASM-reachable core | #202 reuses stable-sort machinery already linked via `fix.rs:361` (no new monomorphization family). #203 adds a `Vec<usize>` per selective import. Measure at step 0 and 5, both numbers in the PR body. **Do not raise the guard** — if it trips, shrink the change. |
| R2 | **Breaking wire change** to `files[].file` for stdin | Intended and free: v0.4.0 merged on main but **not tagged**. CHANGELOG `[Unreleased]` BREAKING with a before/after snippet. |
| R3 | Sorting masks a rule-ordering regression | AC-P1-11 (repeat-determinism) + AC-P1-13 (`--fix` byte-identical). |
| R4 | AST field ripples into the resolver | 8 consumers verified (`[CR]` +`structural_eq.rs:175,180`); only `parser_helpers.rs:836` constructs. `clippy --all-targets -D warnings` catches any missed site at compile time. |
| R5 | Python mirror drifts from `to_canonical_json` | Sorting at the builder means the mirror inherits order from the already-sorted `LintResult` — no mirror edit. AC-P1-24 proves it. |
| R6 | Offset arithmetic wrong but in-bounds (**PF-012**) | AC-P1-14 is a positive control: slice the source at the span and compare to the expected name. **`[CR]` AC-P1-15 extends it to the desync case the fallback cannot catch.** |
| **R7 `[CR]`** | **Fifth stdin convention (`<source>`) contradicts the shipped rule** | **RETIRED — ruled 2026-08-12 (§0a).** The leg emits `<stdin>`; AD-211-5 ships the rule as an envelope-wide rustdoc block and AC-P1-07 pins it. |
| **R8 `[CR]`** | **T-PAR-1 as originally written cannot pass** | Corrected in step 4 / AC-P1-24. |
| **R9 `[CR]`** | **`LintResult::new` sort is an unflagged public-API semantic change** | Open decision 4; if kept, name it in the CHANGELOG BREAKING entry. |
| R10 | `mds lint` exits 2 on `examples/` by design | Not a regression. Do not "fix". |
| R11 | `cargo test --workspace` stalls ~20 min locally | `cargo nextest` + repo-local `.cargo/config.toml` (`rustc-wrapper=""`, `jobs=2`). **Never commit it.** nextest skips doctests — `cargo test --doc` is mandatory. |

**ADR-008 check:** `files[].file` is identifier-shaped and stays WIRE-escaped on every surface. `<stdin>` contains no character in the hostile class, so the sentinel passes through unchanged. **`[CR]` That is exactly why the sentinel cannot serve as the escaping test — AC-P1-20 supplies a real positive control via a control-character PATH in directory mode. Per PF-018, construct that byte programmatically at runtime; never type a literal `\u` escape into tracked test source.**

## 8. Verification (all must pass before merge)

```bash
cargo nextest run --workspace && cargo test --doc   # --doc is mandatory, nextest skips it
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings   # zero warnings is a hard stop
npm ci && npm run build -w @mdscript/mds-wasm && npm run build --workspaces --if-present
npm test --workspaces --if-present
node scripts/verify-versions.mjs
. .venv/bin/activate && maturin develop -m crates/mds-python/Cargo.toml && pytest crates/mds-python/tests -q
```
Plus: WASM raw byte count before and after, pasted into the PR body, measured in the primary checkout.

## 9. Plan self-review

- **Gaps closed in the original plan:** `formatter.rs` does not exist; #203's "entire line" premise is wrong; `span: None` is reachable; the AST *and* `facts` are `pub(crate)` so ADR-010 does not apply; the fix planner re-sorts and never reads `diag.file`.
- **`[CR]` Gaps closed in THIS review:** a fifth stdin convention (`<source>` via the check gate) that the proposed rule contradicts; T-PAR-1 being unpassable as specified; a guaranteed index desync from post-split filtering; a both-ends-trim delta bug; a wrong justification for computing that delta; two missing `Selective` consumers in a claimed-complete list; `LintResult::new` sorting as an unflagged public-API change; the `fix_edits`-missing wire schema rustdoc; stale `make_diag` rustdoc; missing `files[]`-ordering, truncation-semantics, performance, and escaping-positive-control criteria; PF-016 exposure in the WASM measurement step.
- **`[CR]` Risks retired with evidence:** `@extends` cannot inject foreign-coordinate offsets (`lint_source` re-parses independently); import names are ASCII-only so `name.len()` is always a safe span length; the `trim_start_matches` repeat path is unreachable behind `is_directive_token`.
- **Deliberately in scope, not deferred:** the bare-`stdin` literals (AD-211-2) and the scattered `<stdin>` literals (AD-211-3).
- **Surfaced as decisions rather than silently dropped:** alias-import span anchoring (**still open**, open decision 2 / AC-P1-17); the `<source>` analysis-failure leg (**RULED 2026-08-12 → `<stdin>`**, §0a / AD-211-5); `LintResult::new` sort placement (**still open**, open decision 4 / AD-202-1b).
- **No new modules.** One constant, one private sort helper, one widened existing helper.

## Improvements and Gaps Identified

- VERIFIED CORRECT (no action): I re-checked the plan's load-bearing citations at 113f472 and they hold — `crates/mds-cli/src/formatter.rs` genuinely does not exist (CLI has build/fmt/lint/main/output/watch only); `unused_import.rs` `make_diag` really does set `length: "@import".len()`, so #203's "entire line" premise is indeed wrong; bare `stdin` really is emitted at `crates/mds-cli/src/lint.rs:662,667,706`; `to_canonical_json` is at `crates/mds-core/src/lint/diagnostic.rs:717` with the `<unknown>` fallback at :725; `LintResult::new` is at :659; `LintResultBuilder::push` at :820; `set_diag_display_path` at `crates/mds-cli/src/lint.rs:231`; `STRING_SOURCE_MAP_LABEL` at `crates/mds-core/src/sourcemap.rs:79`; `DEFAULT_FILENAME` at `crates/mds-wasm/src/lib.rs:69`; the `build.rs` remap at :964-993; `unused_variable.rs:79` `span: fv.approx_offset.map(...)` proving `span: None` is reachable; `run_rules` at `crates/mds-core/src/lint/mod.rs:118-131` as a fixed 10-call sequence; and all three cited tests (`cli_lint.rs:1266-1280`, `api_surface.rs:~1426`, `cli_source_map.rs:1048-1061`).
- CITATION DRIFT (minor, fix in plan text): `crates/mds-cli/src/output.rs` is 2,156 lines, not 2,271. `LintResultBuilder::build` is at `crates/mds-core/src/lint/diagnostic.rs:829`, not :828. Neither changes the approach, but a plan that cites a number a reviewer cannot confirm burns trust on the numbers that do matter.
- BLOCKER — T-PAR-1 AS WRITTEN CANNOT PASS. The plan's cross-surface differential asserts napi/WASM/Python/CLI JSON produce "byte-identical `files[]`". Under Option C that is false BY CONSTRUCTION: the CLI stdin path emits `file: "<stdin>"` while every binding emits `file: "input.mds"`. This is already documented in the repo — `crates/mds-python/tests/test_parity.py:150` says surfaces "differ in their file key (\"input.mds\" vs basename) when findings are present", and `test_lint.py:161` repeats it. The differential must compare `files[].diagnostics[]` (ordering, spans, rule, severity, fixable) with the `file` key EXCLUDED, and separately assert the `file` key equals the per-surface expected sentinel. PF-007 is satisfied by comparing surfaces to each other on the fields that are supposed to match — not by pretending a deliberately divergent field matches.
- ~~BLOCKER~~ **RESOLVED 2026-08-12 (§0a): the fifth convention collapses to `<stdin>`.** The finding below stands as verified analysis and is what forced the ruling; it is no longer blocking, and AD-211-5 / AC-P1-07 now carry a concrete answer instead of a placeholder. Original text: A FIFTH STDIN CONVENTION EXISTS AND THE PLAN'S RULE CONTRADICTS IT. `crates/mds-core/src/resolver.rs:199` defines `const SOURCE_LABEL: &str = "<source>"`, used as `ctx.file_str` in `resolve_source_intrinsic` (:615-624, :640-649). `lint_str_with` (`crates/mds-core/src/lib.rs:1212`) calls `resolve_source_intrinsic` as its check gate BEFORE linting. So `mds lint -` on a source with a compile error takes the `emit_analysis_failure_json_or_stderr` path (`crates/mds-cli/src/lint.rs:1421-1436`) and renders `<source>:3:1` on stderr via miette — and `<source>` can appear inside `error.message` in the JSON envelope for errors that name the file (e.g. circular import). The plan's proposed rule — "every user-visible emission of stdin's source identity uses the single sentinel `<stdin>`" — is violated by this path on day one. Issue #211 explicitly names `<source>` and cites `resolver.rs:199`; the plan's stdin-label chain table omits it entirely. This must be handled or explicitly scoped out with a written reason (see openDecisions).
- BLOCKER — GUARANTEED INDEX DESYNC IN THE #203 OFFSET VECTOR. `parse_import_directive` (`crates/mds-core/src/parser_helpers.rs:816-821`) builds `names` as `names_str.split(',').map(trim).filter(|n| !n.is_empty()).collect()`. The `filter` runs AFTER the split. A naive per-segment offset vector therefore desyncs from `names` for `@import { a, , b } from "./l.mds"` (3 segments, 2 names) and for the trailing-comma form `@import { a, b, } from "./l.mds"` (3 segments, 2 names). Both parse successfully today. Under desync, AD-203-3's `unwrap_or(imp.offset)` fallback does NOT save you — indices SHIFT, so name `b` silently anchors at name `a`'s offset. That is in-bounds, plausible, and wrong: precisely PF-012. The offset vector must be produced by the SAME filter+trim pipeline in a single pass (push name and offset together, or build `Vec<(String, usize)>` internally and unzip). Needs a dedicated AC and two tests.
- CORRECTNESS — THE PLAN'S DELTA FORMULA IS SUBTLY WRONG. The plan says compute `directive.len() - after_kw.len()`. But `crates/mds-core/src/parser_helpers.rs:808` is `let rest = directive.trim_start_matches("@import").trim();` — a BOTH-ENDS trim. Subtracting the both-ends-trimmed length over-counts the start delta by the length of any TRAILING whitespace on the directive line, which silently shifts every name offset right. The start delta must be computed with `trim_start` only: `directive.len() - directive.trim_start_matches("@import").trim_start().len()`. A source line with trailing spaces (`@import { a } from "./l.mds"   `) is the trivial reproducer, and it would pass any assertion that only checks in-bounds-ness.
- The plan's stated MOTIVATION for computing the delta is wrong even though the conclusion is right. It claims `trim_start_matches("@import")` strips repeated occurrences, so `7` is unsafe. But `is_directive_token` (`crates/mds-core/src/parser_helpers.rs:1459-1463`) only admits `@import` followed by ` `, `\t`, `{`, or end-of-string — `@import@import ...` never reaches `parse_import_directive`. The repeat-strip path is unreachable. Keep the computed delta (it is the right defensive choice and it is what makes the trailing-whitespace bug above visible), but fix the justification so a future reader does not "simplify" it back to `7` on discovering the stated reason is bogus.
- INCOMPLETE CONSUMER ENUMERATION. The plan claims it "verified every consumer" of `ImportDirective::Selective` and lists six sites. It missed `crates/mds-core/src/lint/rules/structural_eq.rs:175` and :180, which destructure `Selective { names, path, .. }` in both halves of a comparison. They use `..`, so the addition compiles — but the substantive point is that structural equality returns `n1 == n2 && p1 == p2` and deliberately IGNORES offsets (mirroring the `end_offset` doc at `crates/mds-core/src/ast.rs:338-341`, "intentionally excluded from structural equality"). `name_offsets` MUST stay excluded or `duplicate-import` detection changes behavior. This needs an explicit regression pin, not just a compile check.
- RISK RETIRED WITH EVIDENCE (strengthens the plan, add it to §7): I checked whether `@extends` inheritance could feed the linter a MERGED module, which would put parent-file offsets into a child-file diagnostic — a textbook PF-012 mis-anchor that narrowing the span from 7 bytes to a name would make worse. It cannot. `lint_source` (`crates/mds-core/src/lint/mod.rs:69-81`) re-parses the entry source independently (`lexer::tokenize(source, filename)` then `parse_with_ctx`) and never consults the resolver's merged module. Every offset the linter sees is in the entry source's own coordinate space. State this in the plan so nobody re-litigates it.
- RISK RETIRED WITH EVIDENCE: the plan asserts the fix pipeline is order-independent but only argues it from the re-sort at `fix.rs:361`. Stronger evidence: `crates/mds-core/src/lint/fix.rs` contains ZERO reads of `diag.file` anywhere in the file. So `set_diag_display_path` mutating `diag.file` to `<stdin>` BEFORE `preview_fixes`/`plan_and_apply_fixes` cannot perturb fix planning at all. This matters because AD-211-4 places the relabel early in `run_lint_stdin`, upstream of both fix paths.
- SCOPE FACT THE PLAN MISSTATES: `--fix` + `--format json` + stdin is a HARD USAGE ERROR that exits 2 (`crates/mds-cli/src/lint.rs:136-146`, AC-F-22b, flagged there as a deliberate exception). AD-211-4 says the single `set_diag_display_path` call "fixes the JSON `file` key for all three stdin branches" — but two of those three branches can never emit JSON. The call is still correctly placed; the claim is just overstated. More importantly the Tester must know this: T-211-3 as written (`mds lint --fix --check -`) is HUMAN-mode only, and anyone who adds `--format json` to it gets exit 2 and a plain stderr message, not a status line.
- STALE-DOC RESIDUE (project rule: "leave the end-state, not the transition"). `crates/mds-core/src/lint/rules/unused_import.rs` documents `make_diag` as: "The span always covers the `@import` keyword (length = 7), so `offset` is the only caller-supplied span parameter." #203 falsifies both halves. `make_diag` needs a `length` parameter and the rustdoc needs rewriting to the new end-state — not a "used to be 7" tombstone.
- PRE-EXISTING DOC DRIFT IN THE EXACT BLOCK THIS PR EDITS. The `to_canonical_json` schema rustdoc (`crates/mds-core/src/lint/diagnostic.rs:683-704`) documents keys `rule, severity, message, help, fixable, span` — but the code at :756-764 also emits `fix_edits`. This PR is the one that declares the lint JSON a pinned wire contract; shipping a contract whose canonical rustdoc omits an emitted key is a defect. Fix it here.
- SORTING IN `LintResult::new` IS A PUBLIC API BEHAVIOR CHANGE THE PLAN DOES NOT FLAG AS BREAKING. `LintResult::new` (`crates/mds-core/src/lint/diagnostic.rs:659`) is the ADR-010 supported construction path for external crates, with a doctest at :649-656. Sorting inside it means a caller who passes a deliberately-ordered `Vec<LintDiagnostic>` silently gets it reordered. That is a semantic change to a published constructor and belongs in the CHANGELOG BREAKING entry alongside the wire change — or the sort should live only in `build()`. See openDecisions.
- WIRE-CONTRACT COVERAGE GAP: the plan's AC pins ordering WITHIN a file but never pins the `files[]` array ordering. For a published contract both must be stated. The good news is the behavior is already deterministic and should simply be frozen: directory mode explicitly path-sorts at `crates/mds-cli/src/lint.rs:994-995` ("F1: path-sort explicitly — collect_mds_files does NOT guarantee order"), and single-file/stdin emit one entry. Pin it.
- NO PERFORMANCE THRESHOLD IS STATED ANYWHERE IN THE PLAN. State one and make it trivially satisfiable so it is a real gate rather than theater: `MAX_DIAGNOSTICS = 1_000` (`crates/mds-core/src/limits.rs:94`), so the added sort is bounded at n ≤ 1000 per `LintResult`, O(n log n), executed once per lint. Also pin that the sort key borrows (`&Option<String>` / `&str`) and performs zero `String` allocations, since the plan already commits to that but never makes it checkable.
- PF-013 / ADR-009 POSITIVE CONTROL IS MISSING. This PR rewrites `diag.file` (`set_diag_display_path`) and touches the WIRE sanitization boundary for that exact key (`crates/mds-core/src/lint/diagnostic.rs:774-782`, issue #176 / CWE-150). A test that merely asserts `<stdin>` appears proves nothing about whether escaping still works. Add a positive control: in DIRECTORY mode, lint a file whose path contains a control character and assert the emitted `files[].file` carries the sanitized `\u00XX` literal and NOT the raw byte — with a companion assertion that the same test detects the raw byte when sanitization is bypassed. Note also that `<stdin>` itself contains no character in the hostile class, so the sentinel is a no-op for the escape contract — that is the plan's ADR-008 claim and it is correct, but it is exactly why it cannot serve as the control.
- PF-016 APPLIES TO STEP 0/5 OF THE SEQUENCE. The WASM size baseline and re-measure depend on generated, gitignored build output (`pkg/`). If any agent runs that measurement from an isolated worktree the artifact is absent and the check passes on nothing. The measurement must run in the primary checkout, and the AC must require the two raw byte counts be pasted into the PR body, not merely asserted as "passed".
- SEQUENCING IMPROVEMENT: the plan lands #211 → #202 → #203, which is right, but #203 changes span offsets that #202's ordering fixture reads, so the T-202-1 fixture will need its expected offsets updated in step 3. Either (a) build the #202 fixture from rules that #203 does not touch (`duplicate-export`, `unused-variable`, `legacy-interpolation`), or (b) assert non-decreasing order rather than literal offsets. Option (a) is preferable — it keeps T-202-1 a pure ordering test with no coupling to #203.
- ADD A MISSING-FILE CANARY: `crates/mds-cli/tests/cli_source_map.rs:1048-1061` already forbids `input.mds` AND `<source>` in stdin sidecar `sources[]` and requires `<stdin>`. It needs no edit, but it is the strongest existing proof that Option C's sentinel is the established repo convention, and it should be listed as a must-stay-green canary alongside `api_surface.rs`. Conversely `crates/mds-python/tests/test_lint.py:161` and `test_parity.py:150,258` document `input.mds` as the binding-side key — under Option C those assertions must stay UNCHANGED, and the plan listing those files as "touched for ordering parity" should say explicitly that their `input.mds` expectations are deliberately preserved.
- ASCII-ONLY IDENTIFIERS BOUND THE UTF-8 RISK (tightens T-203-5). `is_valid_identifier` (`crates/mds-core/src/parser_helpers.rs:1470-1474`) requires ASCII letter/underscore start and ASCII alphanumeric/underscore body, and `parse_import_directive:824-828` rejects anything else. So an import name is always ASCII and `name.len()` is always a safe span length. Multi-byte content can therefore only shift the BASE offset, never split a name. T-203-5 is still required, but it is testing the base-offset chain, not the name span — say so, or the test gets written against the wrong hypothesis.

## Acceptance Criteria

1. AC-P1-01 (stdin label, CLI JSON): The system MUST emit `files[0].file == "<stdin>"` when `mds lint - --format json` is run on a source that produces at least one diagnostic, and the string `input.mds` MUST NOT appear anywhere in stdout.
2. AC-P1-02 (stdin label, CLI human): The system MUST render `<stdin>` as the file reference in the miette code frame for `mds lint -` in human mode, and MUST NOT render `input.mds`.
3. AC-P1-03 (stdin label, fix preview): The system MUST print `Would fix: <stdin>` for `mds lint --fix --check -` and MUST emit `<stdin>` (not bare `stdin`) in the unified-diff header for `mds lint --fix --diff -`. The system MUST print `Partially fixed: <stdin> (N of M fixes applied)` on the partial-fix path. No bare, unbracketed `stdin` token may remain as a source identity in any of these three messages.
4. AC-P1-04 (cross-subcommand consistency): For stdin input, `mds lint`, `mds fmt`, `mds check`, and `mds build --source-map` MUST all emit the identical sentinel `<stdin>` as the source identity in their user-visible output (diagnostics, status lines, diff headers, and source-map `sources[]`).
5. AC-P1-05 (library contract preserved — NEGATIVE): The system MUST NOT change `mds::STRING_SOURCE_MAP_LABEL`; it MUST remain exactly `"input.mds"` and remain publicly reachable. `crates/mds-core/tests/api_surface.rs::string_source_map_label_is_in_public_api` MUST pass unmodified. The strings `<stdin>` and `<source>` MUST NOT be emitted as a `diag.file` value by any function in `crates/mds-core` — the remap is a CLI-boundary concern only.
6. AC-P1-06 (binding surfaces unchanged — NEGATIVE): The napi, WASM, and Python direct lint APIs MUST continue to report `input.mds` as the `file` key for string-source input. `crates/mds-python/tests/test_lint.py` and `test_parity.py` MUST pass with their existing `input.mds` expectations unedited.
7. AC-P1-07 (analysis-failure label — CONCRETE, per the 2026-08-12 ruling): For `mds lint -` on a source that fails the check gate, the source identity rendered on stderr MUST be exactly `<stdin>`, and any source identity embedded in `error.message` under `--format json` MUST likewise be exactly `<stdin>`. The strings `<source>` and `input.mds` MUST NOT appear as the stdin source identity on either channel. This rule MUST be written as an `AD-211-5` rustdoc block at `emit_analysis_failure_json_or_stderr` (`crates/mds-cli/src/lint.rs:1421-1436`), phrased as a rule about that envelope generally rather than about stdin specifically, so every `MdsError::Io` failure reaching it (existing sites `lint.rs:637, :757, :924`) inherits the same label.
8. AC-P1-08 (ordering, within file): For any single file, `files[].diagnostics[]` MUST be ordered by non-decreasing `span.offset`. Diagnostics with `span == null` MUST sort last. Two diagnostics with an identical `span.offset` MUST retain the fixed rule-execution order defined by `run_rules` (`crates/mds-core/src/lint/mod.rs:118-131`).
9. AC-P1-09 (ordering, cross-surface): The diagnostic ordering in AC-P1-08 MUST hold identically on the CLI human path, CLI JSON path, napi, WASM, and Python surfaces, because it is established on `LintResult.diagnostics` itself and not per-renderer.
10. AC-P1-10 (ordering, files array): `files[]` MUST be ordered by ascending path for directory-mode runs and MUST contain exactly one entry for single-file and stdin runs.
11. AC-P1-11 (determinism): Linting identical input N times MUST produce byte-identical `--format json` stdout across all N runs (N ≥ 5).
12. AC-P1-12 (truncation semantics — NEGATIVE): Sorting MUST NOT change WHICH diagnostics are retained when `MAX_DIAGNOSTICS` (1,000) is reached. The retained set MUST remain the first 1,000 in rule-execution order; sorting reorders only that retained set. `truncated` MUST remain `true`. The system MUST NOT be documented or tested as returning "the first 1,000 by offset".
13. AC-P1-13 (fix pipeline unaffected — NEGATIVE): Diagnostic ordering MUST NOT alter `--fix` output. For every fixture in the existing fix test corpus, the fixed source bytes and the residual diagnostic set MUST be identical before and after the ordering change.
14. AC-P1-14 (span anchoring, positive control): For a selective import with unused names, each `unused-import` diagnostic's span MUST satisfy `source[span.offset .. span.offset + span.length] == <the unused name>` exactly. Asserting only that the offset changed, or that it is in bounds, does not satisfy this criterion.
15. AC-P1-15 (span anchoring, index integrity): AC-P1-14 MUST hold for inputs where the comma-split segment count differs from the parsed name count — specifically `@import { a, , b } from "./l.mds"` and the trailing-comma form `@import { a, b, } from "./l.mds"`. A name MUST NOT anchor at another name's offset under any input.
16. AC-P1-16 (span anchoring, robustness): AC-P1-14 MUST hold for irregular interior whitespace, a name that is a strict prefix of another name in the same import, a name that also occurs as a substring of the import path, a directive line with trailing whitespace, CRLF line endings, and multi-byte UTF-8 content preceding the import.
17. AC-P1-17 (span anchoring, scope — NEGATIVE): Alias-import and merge-import diagnostic spans MUST NOT change unless openDecisions #2 rules otherwise. Whatever is ruled, the shipped behavior MUST match the CHANGELOG BREAKING entry.
18. AC-P1-18 (structural equality unaffected — NEGATIVE): Adding per-name offsets MUST NOT participate in structural equality. `duplicate-import` detection results MUST be unchanged for every existing fixture, including imports that differ only in interior whitespace.
19. AC-P1-19 (graceful degradation — NEGATIVE): If the per-name offset vector and the name vector ever differ in length, the diagnostic MUST fall back to the `@import` keyword offset. The system MUST NOT panic, MUST NOT index out of bounds, and MUST NOT emit a span that slices to a value other than the reported name.
20. AC-P1-20 (escaping contract preserved, with positive control): The WIRE sanitization of `files[].file` MUST remain in force after the display-path rewrite. A directory-mode lint of a file whose path contains a control character MUST emit the sanitized `\u00XX` literal and MUST NOT emit the raw byte — and the test MUST demonstrate it detects the raw byte when sanitization is absent (PF-013 / ADR-009).
21. AC-P1-21 (wire schema documented): The `to_canonical_json` rustdoc schema block MUST list every key the function actually emits, including `fix_edits`. The CHANGELOG `[Unreleased]` BREAKING entry MUST show a before/after JSON snippet covering the `file` key change, the diagnostic ordering change, the `unused-import` span change, and the analysis-failure envelope's `<source>` → `<stdin>` change (AD-211-5), and MUST state that a consumer keying off `files[].file == "input.mds"` for CLI stdin output, or matching `<source>` in `error.message`, or relying on rule-execution array order, or relying on `unused-import` spans having length 7, will break. This CHANGELOG BREAKING block is the wave's **single wire-change ledger** — PR2 and PR4 append to it rather than opening a parallel one.
22. AC-P1-22 (performance, explicit threshold): The added sort MUST be O(n log n) over n ≤ `MAX_DIAGNOSTICS` (1,000) and MUST execute at most once per `LintResult` construction. The sort key MUST borrow (`&str` / `&Option<String>`) and MUST NOT allocate a `String` per comparison. No wall-clock regression threshold is imposed — at n ≤ 1,000 the sort is not a measurable cost — but a lint of the largest fixture in the repo MUST NOT regress by more than 10% wall-clock, measured as the median of 5 runs.
23. AC-P1-23 (WASM budget): The optimized WASM binary raw byte count MUST remain strictly under 850,000. Both the pre-change and post-change raw byte counts MUST be recorded verbatim in the PR body. The guard in `.github/workflows/ci.yml` MUST NOT be raised to accommodate this PR.
24. AC-P1-24 (cross-surface parity, correctly scoped): For an identical fixture linted through napi, WASM, Python, and the CLI, the `files[].diagnostics[]` arrays MUST be byte-identical across all four surfaces when the `file` key is excluded from comparison. The `file` key itself MUST equal `input.mds` on the three binding surfaces and the CLI's own sentinel on the CLI surface. Surfaces MUST be compared to each other, not each to its own golden (PF-007).
25. AC-P1-25 (gates): `cargo nextest run --workspace`, `cargo test --doc`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, the full npm build/test cycle, and `pytest crates/mds-python/tests` MUST all pass. Zero warnings is a hard stop. The `--doc` run is mandatory because nextest skips doctests and both `diagnostic.rs` and `unused_import.rs` carry doc examples.
26. AC-P1-26 (lint JSON `"file"` key for stdin — RULING-PINNED, POSITIVE): Every `--format json` document the CLI emits for stdin input that **carries at least one diagnostic** MUST have `files[0].file` equal exactly `"<stdin>"`. For the analysis-failure envelope emitted when the check gate fails, the source identity carried in `error` MUST likewise be exactly `<stdin>`. The key is a **published wire surface** and this value is the contract of record per the 2026-08-12 ruling. **Zero-diagnostic carve-out (2026-08-14):** when stdin lint completes cleanly, the implementation emits `{"files":[],...}` — no file entry — consistent with how non-stdin files and all three binding surfaces (napi, WASM, Python) handle zero-diagnostic results. The `<stdin>` sentinel is therefore present in `files[0].file` only when at least one diagnostic is emitted for stdin. This carve-out is documented in CHANGELOG under the lint JSON wire contract entry.

**Analysis-failure carve-out (2026-08-14):** when the check gate fails, the CLI emits `{"version":1,"error":{"code":…,"message":…,"help":…,"span":…}}` — no `file` key at the top level and no source identity inside `error`. This is by design: `MdsError::serialize()` emits `code`/`message`/`help`/`span` only, and no `MdsError` Display template interpolates `ctx.file_str`, so the source identity structurally cannot reach `error.message` (AD-211-5 rustdoc at `lint.rs:emit_analysis_failure_json_or_stderr`). A JSON consumer MAY NOT rely on a `file` key in the error envelope; the envelope shape differs from the success envelope (`{"version":1,"files":[…],"truncated":…}`). This carve-out supersedes the "(c)" clause of this AC requiring `error` to carry exactly `<stdin>`. The human channel is unaffected — stderr still renders `[<stdin>:L:C]`. The contract is locked in `cli_lint.rs::stdin_analysis_failure_labels_source_as_stdin` with ADR-009 positive controls.
27. AC-P1-27 (lint JSON `"file"` key for stdin — RULING-PINNED, NEGATIVE): The CLI MUST NEVER emit `"file": "input.mds"` for stdin input in any `--format json` output, on any code path, in any mode. The literal `input.mds` MUST NOT appear anywhere in CLI stdout for a stdin lint, and the literal `<source>` MUST NOT appear as a stdin source identity on stdout or stderr. Verification MUST include a positive control demonstrating the assertion detects `input.mds` when it is present (e.g. the same extraction run against a `113f472` build), so an absence assertion cannot pass vacuously (PF-013 / ADR-009). This criterion does NOT constrain the napi, WASM or Python surfaces, which keep `input.mds` per AC-P1-06.
28. AC-P1-28 (WASM `DEFAULT_FILENAME` unchanged — NEGATIVE, the wrong-lever regression guard): `crates/mds-wasm/src/lib.rs:69` binds `const DEFAULT_FILENAME: &str = mds::STRING_SOURCE_MAP_LABEL`, making the shared constant the WASM **virtual-FS default filename**, not a display label. This PR MUST NOT change `crates/mds-core/src/sourcemap.rs:79`, and the WASM default filename MUST remain `input.mds`. Concretely: a WASM string-source compile MUST still emit `sources[0] == "input.mds"`; a WASM string-source lint MUST still emit `files[].file == "input.mds"`; and a relative `@import` in a string source MUST still resolve against the `input.mds` virtual-FS entry key exactly as at `113f472`. `crates/mds-core/tests/api_surface.rs:1420-1431` and `crates/mds-core/tests/source_map_vfs.rs:1126-1135` MUST pass **unmodified**. Any diff touching `sourcemap.rs:79` fails this criterion outright — the relabel belongs at the CLI output boundary, and moving the constant to satisfy a CLI display requirement is the specific regression this criterion exists to prevent.

## Test Plan

### 1. AC-P1-01 — stdin lint in JSON mode reports the <stdin> sentinel and never leaks the library label.

- **Scenario:** stdin lint in JSON mode reports the <stdin> sentinel and never leaks the library label.
- **Setup:** In crates/mds-cli/tests/cli_lint.rs, pipe a source that fires at least one rule (e.g. "@define greet(name):\n  Hello {{name}}!\n@end\n\n@export greet\n@export greet\n") to `mds lint - --format json`.
- **Expected outcome:** stdout parses as JSON; `files[0].file` is exactly "<stdin>"; `files` has length 1; the literal substring "input.mds" does not occur anywhere in stdout.
- **Verification method:** integration

### 2. AC-P1-02 — stdin lint in human mode renders <stdin> in the code frame.

- **Scenario:** stdin lint in human mode renders <stdin> in the code frame.
- **Setup:** Modify the existing test crates/mds-cli/tests/cli_lint.rs::stdin_lint_diagnostic_includes_code_frame (currently asserts stderr contains "input.mds"). Same source as AC-P1-01, no --format flag.
- **Expected outcome:** stderr contains "duplicate-export", contains "<stdin>", contains "@export" (the code frame is still rendered), and does NOT contain "input.mds".
- **Verification method:** integration

### 3. AC-P1-03 — All three fix-path status/diff messages use the bracketed sentinel; the bare `stdin` convention is gone.

- **Scenario:** All three fix-path status/diff messages use the bracketed sentinel; the bare `stdin` convention is gone.
- **Setup:** Three cases in cli_lint.rs against a source with an auto-fixable finding (a Tier A rule on a standalone file, no @import/@extends): (a) `mds lint --fix --check -`; (b) `mds lint --fix --diff -`; (c) a fixture producing a partial fix via `mds lint --fix -`. All in HUMAN mode — do NOT add --format json, which is rejected with exit 2 at crates/mds-cli/src/lint.rs:136-146.
- **Expected outcome:** (a) stderr contains "Would fix: <stdin>" and the process exits 1. (b) stdout diff header references "<stdin>". (c) stderr matches "Partially fixed: <stdin> (\\d+ of \\d+ fixes applied)". In all three, the regex /(^|[^<])\\bstdin\\b([^>]|$)/ does not match any emitted source-identity line.
- **Verification method:** integration

### 4. AC-P1-04 — Cross-subcommand sentinel consistency for stdin.

- **Scenario:** Cross-subcommand sentinel consistency for stdin.
- **Setup:** Run all four against stdin with a valid source: `mds lint -` (human), `mds fmt --check -`, `mds check -`, `mds build - --source-map` (sidecar or inline as the existing cli_source_map.rs tests do).
- **Expected outcome:** lint code frame shows "<stdin>"; fmt prints "Would reformat: <stdin>" when it would change; check prints "OK: <stdin>"; build source-map `sources[]` contains "<stdin>". No surface emits "input.mds" or "<source>".
- **Verification method:** integration

### 5. AC-P1-05 — The pinned public constant and the core-emits-no-sentinel rule both hold.

- **Scenario:** The pinned public constant and the core-emits-no-sentinel rule both hold.
- **Setup:** Run crates/mds-core/tests/api_surface.rs::string_source_map_label_is_in_public_api unmodified. Separately, grep crates/mds-core/src for the literals "<stdin>" and "<source>" assigned to a diagnostic `file` field.
- **Expected outcome:** The api_surface test compiles and passes with `label == "input.mds"`. No core code path assigns "<stdin>" to `LintDiagnostic.file`. (`<source>` legitimately remains as resolver.rs:199 SOURCE_LABEL — that is the ctx.file_str for errors, not a diag.file, and is governed by AC-P1-07.)
- **Verification method:** unit

### 6. AC-P1-06 — Binding surfaces are untouched by the CLI remap.

- **Scenario:** Binding surfaces are untouched by the CLI remap.
- **Setup:** Run `pytest crates/mds-python/tests -q` and `npm test --workspaces --if-present` with NO edits to the `input.mds` assertions in crates/mds-python/tests/test_lint.py:161 and test_parity.py:150,258.
- **Expected outcome:** All pass unmodified. A Python `lint(source)` call returns diagnostics whose file key is "input.mds".
- **Verification method:** integration

### 7. AC-P1-07 — The analysis-failure path's source identity is `<stdin>`, and is documented.

- **Scenario:** The analysis-failure path's source identity is `<stdin>`, and is documented.
- **Setup:** Pipe a source with a hard syntax error to `mds lint -` (human) and to `mds lint - --format json`. Then grep the emitting site (crates/mds-cli/src/lint.rs:1421 emit_analysis_failure_json_or_stderr) for an `AD-211-5` rustdoc block.
- **Expected outcome:** Human stderr renders `<stdin>:L:C`, not `<source>:L:C`. The JSON `error.message` carries `<stdin>` wherever it names the source, and contains neither `<source>` nor `input.mds`. An AD-211-5 block exists at the emitting site stating the envelope-wide rule and its reason (the 2026-08-12 ruling). Include a positive control: the same extraction applied to a build at 113f472 DOES find `<source>`, proving the assertion detects the old value when present.
- **Verification method:** integration

### 8. AC-P1-08 — Diagnostics within a file are offset-ordered, with span:None last and stable ties.

- **Scenario:** Diagnostics within a file are offset-ordered, with span:None last and stable ties.
- **Setup:** Build a fixture that fires at least three rules NOT touched by #203 — use duplicate-export, unused-variable, and legacy-interpolation — at known, deliberately out-of-execution-order offsets (i.e. the rule that runs first in run_rules fires at the LARGEST offset). Run `mds lint <file> --format json`. Add a mds-core unit test that also constructs the case via the lint entry point.
- **Expected outcome:** `files[0].diagnostics[].span.offset` is non-decreasing across the array. Any diagnostic with `span == null` appears after every diagnostic with a span. The array order differs from run_rules order, proving the sort ran.
- **Verification method:** integration

### 9. AC-P1-08 (span:None reachability) — A span-less diagnostic sorts last without panicking.

- **Scenario:** A span-less diagnostic sorts last without panicking.
- **Setup:** Construct a source with a frontmatter variable whose offset cannot be approximated, so crates/mds-core/src/lint/rules/unused_variable.rs:79 yields `fv.approx_offset == None`, combined with at least one span-bearing diagnostic. Lint via the public entry point.
- **Expected outcome:** No panic. The span-less diagnostic is the last element of `files[0].diagnostics[]` and serializes as `"span": null`.
- **Verification method:** unit

### 10. AC-P1-09 — Human output ordering matches JSON ordering.

- **Scenario:** Human output ordering matches JSON ordering.
- **Setup:** Run the AC-P1-08 fixture through `mds lint <file>` (human, stderr) and `mds lint <file> --format json` (stdout). Extract the rule-name sequence from each.
- **Expected outcome:** The sequence of rule names on stderr is identical to the sequence of `diagnostics[].rule` in the JSON. This proves the sort is on LintResult.diagnostics and not in the JSON renderer.
- **Verification method:** integration

### 11. AC-P1-10 — files[] array ordering is pinned for directory and single-file/stdin modes.

- **Scenario:** files[] array ordering is pinned for directory and single-file/stdin modes.
- **Setup:** Create a temp dir with files that sort non-trivially (e.g. b.mds, a.mds, sub/c.mds), each with at least one finding. Run `mds lint <dir> --format json`. Separately run single-file and stdin JSON lints.
- **Expected outcome:** Directory mode: `files[].file` is in ascending path order regardless of filesystem enumeration order. Single-file and stdin: `files` has exactly one element.
- **Verification method:** integration

### 12. AC-P1-11 — Repeated lints of identical input are byte-identical.

- **Scenario:** Repeated lints of identical input are byte-identical.
- **Setup:** Run `mds lint <AC-P1-08 fixture> --format json` five times, capturing stdout each time.
- **Expected outcome:** All five stdout buffers are byte-identical. This pins AD-202-3's stable-tie residual (identical (start,end) edits retaining input order).
- **Verification method:** integration

### 13. AC-P1-12 — Truncation still selects by rule-execution order, not by offset.

- **Scenario:** Truncation still selects by rule-execution order, not by offset.
- **Setup:** Extend the existing crates/mds-core/src/lint/diagnostic.rs::builder_truncates_at_max_diagnostics pattern: push MAX_DIAGNOSTICS+1 diagnostics whose offsets DESCEND (so the last-pushed, rejected one has the SMALLEST offset). Build the result. Construct diagnostics via LintDiagnostic::new / with_* builders, never struct literals (ADR-010).
- **Expected outcome:** `diagnostics.len() == 1000`; `truncated == true`; the rejected smallest-offset diagnostic is ABSENT from the result even though sorting would have placed it first. The retained 1000 are sorted ascending among themselves.
- **Verification method:** unit

### 14. AC-P1-13 — --fix output is byte-identical before and after the ordering change.

- **Scenario:** --fix output is byte-identical before and after the ordering change.
- **Setup:** Before landing #202, capture `mds lint --fix <f>` stdout bytes and the residual diagnostic set for every multi-rule fixture in the existing fix corpus (crates/mds-core/src/lint/fix.rs tests plus cli_lint.rs fix tests). Re-capture after.
- **Expected outcome:** Byte-identical fixed source and identical residual rule/severity multiset for every fixture. Supporting static evidence: crates/mds-core/src/lint/fix.rs contains no read of `diag.file`, and re-sorts its own edits at fix.rs:361 before dedup/overlap.
- **Verification method:** integration

### 15. AC-P1-14 — POSITIVE CONTROL — each unused-import span slices to exactly the unused name.

- **Scenario:** POSITIVE CONTROL — each unused-import span slices to exactly the unused name.
- **Setup:** Unit test in crates/mds-core/src/lint/rules/unused_import.rs. Source: `@import { used, unused_a, unused_b } from "./l.mds"\n@include used()\n` (adjust so `used` is genuinely referenced). Lint, then for every unused-import diagnostic compute `&source[span.offset .. span.offset + span.length]`.
- **Expected outcome:** Two diagnostics. The slice equals "unused_a" for one and "unused_b" for the other, matching the name quoted in each diagnostic's message. `span.length` equals the name length, not 7. An in-bounds-but-wrong offset fails this assertion (PF-012).
- **Verification method:** unit

### 16. AC-P1-15 — Empty and trailing comma segments do not desync name-to-offset alignment.

- **Scenario:** Empty and trailing comma segments do not desync name-to-offset alignment.
- **Setup:** Two unit cases: (a) `@import { a, , b } from "./l.mds"`; (b) `@import { a, b, } from "./l.mds"`. Neither `a` nor `b` referenced in the body. Apply the AC-P1-14 slice assertion to each diagnostic.
- **Expected outcome:** Case (a): exactly two diagnostics; slices are "a" and "b" respectively, matching each message. Case (b): identical. Critically, `b`'s span offset must be `b`'s real source position, NOT `a`'s. Add a companion assertion that the offset vector length equals `names.len()` at the construction site.
- **Verification method:** unit

### 17. AC-P1-16 — Span anchoring survives whitespace, prefix collisions, path collisions, trailing whitespace, CRLF, and multi-byte prefixes.

- **Scenario:** Span anchoring survives whitespace, prefix collisions, path collisions, trailing whitespace, CRLF, and multi-byte prefixes.
- **Setup:** Six unit cases, each applying the AC-P1-14 slice assertion: (a) `@import {  a ,   b  } from "./l.mds"`; (b) `@import { foo, foobar } from "./l.mds"` with only foo used; (c) `@import { lib } from "./lib.mds"` with lib unused; (d) `@import { a, b } from "./l.mds"   ` (three trailing spaces — this is the case that catches the both-ends-trim delta bug); (e) the same source with \r\n line endings; (f) `# héllo wörld\n@import { a, b } from "./l.mds"`.
- **Expected outcome:** Every diagnostic's slice equals its reported name in all six cases. In (b) the `foobar` diagnostic anchors at foobar's own offset, not at foo's. In (c) the span lands inside the braces, not inside the quoted path. In (d) offsets are unshifted by the trailing whitespace. In (f) `source.is_char_boundary(span.offset)` is true and the slice does not panic.
- **Verification method:** unit

### 18. AC-P1-17 — Alias and merge import spans behave per the ruling.

- **Scenario:** Alias and merge import spans behave per the ruling.
- **Setup:** Unit test with `@import "./l.mds" as unusedAlias` and a merge `@import "./l.mds"`. Assert the span against whichever behavior openDecisions #2 rules.
- **Expected outcome:** If the ruling is 'exclude': alias span offset == the @import keyword offset and length == 7, unchanged from HEAD; merge is never diagnosed (ImportKind::Merge is skipped at unused_import.rs). If the ruling is 'include': alias span slices to the alias identifier and the CHANGELOG BREAKING entry names it.
- **Verification method:** unit

### 19. AC-P1-18 — duplicate-import detection is unchanged by the new field.

- **Scenario:** duplicate-import detection is unchanged by the new field.
- **Setup:** Lint sources containing two structurally identical selective imports written with DIFFERENT interior whitespace, e.g. `@import { a, b } from "./l.mds"` and `@import {a,b} from "./l.mds"` — the offsets differ, the structure does not. Also run the full existing duplicate_import rule test suite.
- **Expected outcome:** duplicate-import fires exactly as it does at HEAD. structural_eq (crates/mds-core/src/lint/rules/structural_eq.rs:175-185) continues to compare only `names` and `path`; name_offsets must not affect the result. All pre-existing duplicate_import tests pass unedited.
- **Verification method:** unit

### 20. AC-P1-19 — Length desync degrades to the keyword anchor instead of mis-anchoring or panicking.

- **Scenario:** Length desync degrades to the keyword anchor instead of mis-anchoring or panicking.
- **Setup:** A unit test that constructs an ImportFact whose name_offsets vector is deliberately shorter than names (crate-internal test, legal because lint::facts is pub(crate)), then invokes the rule.
- **Expected outcome:** No panic in release semantics. Each name lacking an offset produces a span at the @import keyword offset. Verify the debug_assert_eq! on lengths exists AND that the unconditional `unwrap_or(imp.offset)` fallback exists — per PF-005 the debug_assert alone is not the guard.
- **Verification method:** unit

### 21. AC-P1-20 — POSITIVE CONTROL — WIRE sanitization of files[].file still works after the display-path rewrite.

- **Scenario:** POSITIVE CONTROL — WIRE sanitization of files[].file still works after the display-path rewrite.
- **Setup:** Directory-mode integration test. Create a file whose PATH contains a control character (construct the byte programmatically from a numeric escape at test runtime — do NOT type a literal \u sequence into the test source; per PF-018 the editing tool decodes it into a live control byte in tracked source). Skip on platforms that reject the filename. Run `mds lint <dir> --format json`. Then, as the control arm, assert that the same extraction logic applied to the RAW unsanitized path string DOES find the raw byte.
- **Expected outcome:** The emitted `files[].file` contains the escaped literal form and contains no raw control byte. The control arm proves the assertion is capable of detecting a raw byte when one is present — satisfying PF-013 / ADR-009. Absence alone is not accepted.
- **Verification method:** integration

### 22. AC-P1-21 — The wire schema rustdoc and the CHANGELOG match what ships.

- **Scenario:** The wire schema rustdoc and the CHANGELOG match what ships.
- **Setup:** Read the to_canonical_json rustdoc schema block in crates/mds-core/src/lint/diagnostic.rs against the keys the function actually inserts. Read the CHANGELOG [Unreleased] BREAKING entry.
- **Expected outcome:** The rustdoc schema lists rule, severity, message, help, fixable, span, AND fix_edits. The CHANGELOG entry contains a before/after JSON snippet and names all three breaks explicitly: CLI stdin `files[].file` changes from "input.mds" to "<stdin>"; `diagnostics[]` array order changes from rule-execution to offset order; `unused-import` span changes from length 7 at the @import keyword to the name length at the name.
- **Verification method:** manual

### 23. AC-P1-22 — Sort cost is bounded and allocation-free.

- **Scenario:** Sort cost is bounded and allocation-free.
- **Setup:** Inspect the sort helper for `sort_by` with a borrowed key (no `.clone()`, no `to_string()` in the comparator). Then time `mds lint` on the largest fixture in the repo, 5 runs, taking the median, before and after the change.
- **Expected outcome:** Comparator borrows `&Option<String>`/`&str` and allocates nothing. n is bounded by MAX_DIAGNOSTICS = 1_000 (crates/mds-core/src/limits.rs:94). Median wall-clock regression is under 10%. Sort is invoked at most once per LintResult construction.
- **Verification method:** load

### 24. AC-P1-23 — WASM binary stays under the 850,000-byte guard.

- **Scenario:** WASM binary stays under the 850,000-byte guard.
- **Setup:** IN THE PRIMARY CHECKOUT, NOT AN ISOLATED WORKTREE (PF-016 — pkg/ is generated and gitignored, so an isolated worktree has nothing to measure and the check passes vacuously). Requires Binaryen v129+. Run `npm run build -w @mdscript/mds-wasm` and record the raw .wasm byte count before any source change, then again after all three issues land.
- **Expected outcome:** Both raw byte counts are pasted verbatim into the PR body. The post-change count is strictly less than 850,000. The threshold in .github/workflows/ci.yml is unchanged — if the budget trips, the change shrinks rather than the guard growing.
- **Verification method:** manual

### 25. AC-P1-24 — Cross-surface differential on the fields that are supposed to match.

- **Scenario:** Cross-surface differential on the fields that are supposed to match.
- **Setup:** One fixture file. Lint it via CLI `--format json`, napi, WASM, and Python, each through its file-based lint entry point where available. Normalize by deleting the `file` key from each `files[]` entry, then compare the four normalized structures pairwise. Separately assert each surface's `file` key equals its expected value.
- **Expected outcome:** The four normalized `files[].diagnostics[]` arrays are byte-identical — same order, same spans, same rule/severity/fixable. The `file` keys are "input.mds" on napi/WASM/Python string-source paths and the basename (file mode) or "<stdin>" (stdin mode) on the CLI. Do NOT assert full byte-identity including the file key — that is false by construction under Option C and is already documented at crates/mds-python/tests/test_parity.py:150.
- **Verification method:** integration

### 26. AC-P1-25 — Full gate suite.

- **Scenario:** Full gate suite.
- **Setup:** Run, in order: `cargo nextest run --workspace`; `cargo test --doc`; `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `npm ci && npm run build -w @mdscript/mds-wasm && npm run build --workspaces --if-present`; `npm test --workspaces --if-present`; `node scripts/verify-versions.mjs`; `. .venv/bin/activate && maturin develop -m crates/mds-python/Cargo.toml && pytest crates/mds-python/tests -q`. Use the repo-local .cargo/config.toml (rustc-wrapper="", jobs=2) and NEVER commit it.
- **Expected outcome:** All green, zero clippy warnings. `cargo test --doc` is mandatory and separately reported — nextest skips doctests, and both LintResult::new (diagnostic.rs:649-656) and the to_canonical_json schema block carry doc examples this PR edits. `mds lint` exiting 2 on examples/ is by design and is not a failure.
- **Verification method:** integration

### 27. AC-P1-26 — Every stdin JSON envelope carries `<stdin>` as the source identity, with a zero-diagnostic carve-out.

- **Scenario:** Every stdin JSON envelope carries `<stdin>` as the source identity when diagnostics are present; emits an empty `files[]` array when none are.
- **Setup:** In crates/mds-cli/tests/cli_lint.rs, three stdin cases run with `--format json`: (a) a source producing several diagnostics; (b) a lint-clean source producing zero diagnostics; (c) a source with a hard syntax error that fails the check gate and takes the emit_analysis_failure_json_or_stderr path.
- **Expected outcome:** (a) stdout parses as JSON, `files` has length 1, and `files[0].file` is exactly `"<stdin>"`. (b) stdout parses as JSON and `files` is an empty array — consistent with the zero-diagnostic carve-out in AC-P1-26 (2026-08-14): no file entry is emitted and `"<stdin>"` does NOT appear in `files[]`, matching the behaviour for non-stdin files and all three binding surfaces when zero diagnostics are produced. The test MUST assert `files.len() == 0`, not access `files[0]`. (c) the error envelope's source identity is exactly `"<stdin>"`. Cases (a) and (c) assert exact string equality on the key, never `contains`.
- **Verification method:** integration

### 28. AC-P1-27 — NEGATIVE with positive control — the CLI never emits `input.mds` or `<source>` for stdin.

- **Scenario:** NEGATIVE with positive control — the CLI never emits `input.mds` or `<source>` for stdin.
- **Setup:** Re-run every stdin case from AC-P1-26 plus the human-mode and fix-preview stdin cases. Scan full stdout and stderr for the literals `input.mds` and `<source>`. Control arm: run the identical extraction against a binary built at 113f472.
- **Expected outcome:** Post-change: zero occurrences of `input.mds` in CLI stdout for any stdin lint, and zero occurrences of `<source>` as a stdin source identity on either channel. Control arm at 113f472: the SAME extraction finds `input.mds` (lint JSON and human frame) and `<source>` (analysis-failure path) — proving the assertions are capable of failing, per PF-013 / ADR-009. Binding-surface assertions (crates/mds-python/tests/test_lint.py:161, test_parity.py:150,258) stay unedited and keep expecting `input.mds`; this criterion is CLI-scoped.
- **Verification method:** integration

### 29. AC-P1-28 — NEGATIVE — the shared constant and the WASM virtual-FS default are untouched.

- **Scenario:** NEGATIVE — the shared constant and the WASM virtual-FS default are untouched.
- **Setup:** (a) `git diff 113f472 -- crates/mds-core/src/sourcemap.rs` and confirm line 79 is unchanged. (b) Run crates/mds-core/tests/api_surface.rs and crates/mds-core/tests/source_map_vfs.rs with zero edits. (c) Through the WASM surface: compile a string source with `source_map: true`; lint a string source; and compile a string source containing a relative `@import` that must resolve against the virtual-FS default entry key.
- **Expected outcome:** (a) `STRING_SOURCE_MAP_LABEL` is still `"input.mds"` and the diff is empty for that line. (b) Both test files pass unmodified, including `string_source_map_label_is_in_public_api` (api_surface.rs:1420-1431) and the D1 cross-surface parity block (source_map_vfs.rs:1126-1135). (c) WASM `sources[0] == "input.mds"`, WASM lint `files[].file == "input.mds"`, and the relative `@import` resolves exactly as at 113f472. Any of these changing means the relabel was applied at the constant instead of at the CLI output boundary.
- **Verification method:** integration

## Merge Position

**Position 2 of 6** in the recommended merge order: PR1 — Lint JSON wire contract (#211, #202, #203)

Reason: Defines the lint JSON envelope, the `files[]` shape and ordering, and the CHANGELOG wire-change ledger that PR2 and PR4 must append to rather than fork; it also settles the source-label rule that PR2's config-error path inherits.

### Plan Amendments from the Cross-PR Conflict Audit

**PR1 — Lint JSON wire contract**

> **Post-ruling status (2026-08-12):** amendment **1 is VOID** — under the #224 warn-but-continue ruling PR2 emits no `files[].error` entries, so AC-P1-10 needs no widening and the `files[]` array keeps a single entry shape. Amendment **3 is SATISFIED** — AD-211-5 is now ruled (`<stdin>`) and is written as an envelope-wide rule, which is exactly what that amendment asked for. Amendments 2, 4, 5 and 6 stand unchanged.

1) AC-P1-10 must be widened: in directory mode `files[]` may contain BOTH diagnostic entries and PR2's error-only entries (`{"file":…, "error":…}` with no `diagnostics` key, emitted at crates/mds-cli/src/lint.rs:1086-1096); both are path-sorted. As written, AC-P1-10 is silently violated by PR2. 2) The CHANGELOG BREAKING block PR1 creates is the wave's single wire-change ledger — say so explicitly, so PR2 and PR4 append rather than fork. 3) AD-211-5's ruling on how the error envelope labels its source must be written as a rule about `emit_analysis_failure_json_or_stderr` generally, not about stdin specifically, because PR2's config rejection travels the same path (existing MdsError::Io sites at lint.rs:637, :757, :924). 4) Add a line to §8 verification: run `node scripts/verify-no-control-bytes.mjs` before pushing, and construct the AC-P1-20 control byte at runtime — PR6's gate will be live by the time PR1 merges. 5) Record the verified fact that mds-cli calls only `apply_fixes_incremental` (lint.rs:451, :571) and never `apply_fixes`, so PR5's deprecation cannot touch PR1. 6) Note that fix.rs's `make_result` (fix.rs:1031) builds LintResult by struct literal, bypassing both `new()` and `build()` — so PR1's sort does NOT reach the existing fix.rs unit corpus, which strengthens AC-P1-13 and narrows what actually needs re-baselining.
