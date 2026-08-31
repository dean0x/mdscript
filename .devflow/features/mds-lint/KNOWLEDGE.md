---
feature: mds-lint
name: mds lint — Static Analysis Engine and Tiered --fix
description: "Use when adding or modifying lint rules, extending the --fix pipeline, changing the JSON wire format, wiring lint into a binding layer, debugging unexpected exit codes and reverify gate refusals, or working on the ESC/bidi/newline injection defences. Keywords: mds lint, LintDiagnostic, fix_removals, fix_edits, TextEdit, FixLineSpan, diag_to_edits, LintResult, LintConfig, to_canonical_json, fix tier, reverify gate, FixOutcome, PartiallyFixed, apply_fixes_incremental, preview_fixes, PreviewOutcome, set_diag_display_path, AnalysisContext, ElseifBranch, end_offset, DefineFact, assertKnownKeys, CheckOptions, unreachable-branch, unused-variable, duplicate-import, empty-block, legacy-interpolation, is_output_neutral, all_output_neutral, Tier A Tier B Tier C, structural-standalone, compile-clean, is_standalone, sanitize_control_chars, sanitize_control_chars_wire, named_source_for_render, neutralize_source_for_render, SanitizedReport, SanitizedNode, MAX_AUX_DEPTH, EscapeMode, HUMAN WIRE, eprint_warning, safe_path, safe_inline, safe_file_display, preview_text_for, print_discipline, reverify_failure_reason, LintDirCtx, config_cache, dedup_contained_or_identical, EXIT 0 1 2 3, render_error_sanitized, eprint_error, display_sanitized, MdsError::display_sanitized, ESC-injection, CWE-150, CWE-117, bidi, Trojan-Source, CVE-2021-42574, U+061C, U+202E, U+FEFF, U+2028, U+2029, PF-014, PF-005, construction-time sanitization, per-field rule, Cow, #176, ADR-008, ResultSink, from_rules_checked, relative_display, write_bytes, PF-020, #309, emit-ordering."
category: domain-knowledge
directories:
  - crates/mds-core/src/lint
  - crates/mds-cli/src
  - crates/mds-wasm/src
  - crates/mds-napi/src
  - crates/mds-python/src
  - packages/mds/src
created: 2026-07-11
updated: 2026-08-31
---

# mds lint — Static Analysis Engine and Tiered --fix

## Overview

`mds lint` is a phase-3.5 static analysis pass: it runs AFTER `mds check` (resolve+validate) confirms the template compiles, then applies 10 rules over the raw AST and token stream. The engine lives entirely in `crates/mds-core/src/lint/` and is exposed unchanged via all five surfaces: CLI, WASM, napi, Python, and the `packages/mds` universal wrapper.

The feature has two key design invariants that touch every layer: (1) `LintResult::to_canonical_json()` is the ONE serializer for all surfaces — byte-parity across surfaces is enforced by goldens; (2) `tier.rs` is the single source of truth for which rules are auto-fixable — both `diagnostic.rs` and `fix.rs` import from it to break what would otherwise be a circular dependency.

Fix edits take two forms: **line-removal** (`fix_removals: Option<Vec<FixLineSpan>>`) for rules that delete whole lines (e.g. `duplicate-import`), and **in-place replacement** (`fix_edits: Option<Vec<TextEdit>>`) for rules that need to replace text without changing line structure (e.g. `legacy-interpolation` rewrites `{x}` → `{{x}}`). Both paths converge in `diag_to_edits` and produce `ByteEdit`s that `apply_plan_unchecked` applies right-to-left via `replace_range`.

## Business Context

Issue #61 closes the v0.4.0 gate. Users are prompt-library authors (dead `@define` functions accumulate silently), CI gatekeepers (PR template already mandates "no new linter warnings"), and future LSP integrations (stable JSON schema). `mds check` answers "will it compile?"; `mds lint` answers "should it compile this way?".

## Core Business Rules

### Rule Catalog

Ten rules across three severities. Defaults are built-in and overridable per-rule in `mds.json`.

| Rule | Default Severity | Fix Tier | Notes |
|------|-----------------|----------|-------|
| `duplicate-import` | Error | A | Lexical path normalization via `normalize_import_path` (interior segment-collapse) |
| `duplicate-export` | Error | A | Spans from D2 `offset` on `ExportDirective` variants |
| `unreachable-branch` | Error | A | Literal↔literal Eq/NotEq + duplicate structural @elseif only; variable compares never flagged |
| `empty-block` | Warn | A | Empty OR whitespace-only-Text bodies; NEVER @block (intentional placeholder); fires on @if/@elseif/@else/@for/@define/@message |
| `legacy-interpolation` | Warn | A | TOKEN-based (Token::Text + @message directives, structurally skips fences); detects old `{x}` single-brace syntax + `\{`/`\}` remnants + `@message {expr}:`; `${...}` skipped; fix via `fix_edits` (atomic TextEdit replacement, not FixLineSpan) |
| `unused-import` | Warn | B | Merge imports always "used"; selective flagged per-name; re-export (Named/ReExport) exempts; `fix_removals: None` (report-only in practice) |
| `unused-function` | Warn | B | Only fires when `has_explicit_exports`; self-recursion treated as used; `fix_removals = Some([whole-@define block])` |
| `unused-variable` | Warn | C | Frontmatter keys with no body reference; Tier C = report-only |
| `redundant-else` | Warn | C | @else body structurally identical to @if then-body via structural_eq |
| `shadow-variable` | Info | C | Default OFF — must be enabled in mds.json; never affects exit code |

### The Tier Model

Tier classification is the central safety contract for `--fix`. `tier.rs` is a dedicated leaf module that both `fix.rs` and `diagnostic.rs` import — it was extracted to break a potential circular dependency. It also hosts the `first_occurrence` helper (shared by `duplicate_import.rs` and `duplicate_export.rs`) as it is the one lint leaf module with no rule-specific imports.

- **Tier A**: Auto-fixable via `fix_removals` (line-removal) or `fix_edits` (text-replacement) + gated by the reverify pipeline. Currently: duplicate-import, duplicate-export, unreachable-branch, empty-block, legacy-interpolation.
- **Tier B**: Fixable only when the file is "structural-standalone" (no `@import` or `@extends`). Currently: `unused-function` (has `fix_removals = Some([whole-@define block])`, applied when standalone); `unused-import` has `fix_removals: None` — partial-name removal from an import list is structurally ambiguous and unsafe, so the rule is Tier B but never emits an edit (report-only in practice). A file that triggers `unused-import` is, by definition, not structural-standalone.
- **Tier C**: Never fixed — report-only. Currently: unused-variable, redundant-else, shadow-variable.

**Terminology (spec §7.5)**:
- **Structural-standalone**: a file with no `@import`, `@extends`, or use as a partial target. Gates Tier B `--fix`.
- **Compile-clean**: a file that compiles without any runtime `--vars`. Gates the output-equality reverify for Tier B fixes: removing an unused import or function must produce byte-identical compiled output.

```rust
// tier.rs — the single source of truth. DO NOT re-inline this table in fix.rs or diagnostic.rs.
pub fn rule_tier(rule: &str) -> FixTier {
    match rule {
        "duplicate-import" | "duplicate-export" | "unreachable-branch"
        | "empty-block" | "legacy-interpolation" => FixTier::A,
        "unused-import" | "unused-function" => FixTier::B,
        _ => FixTier::C, // all others, including unknown rules
    }
}

// is_output_neutral: true for all rules EXCEPT legacy-interpolation.
// All Tier A/B rules are output-neutral (fix preserves compiled output) except
// legacy-interpolation, which migrates {x} (plain text) to {{x}} (interpolation),
// intentionally changing the compiled output. The CLI uses this to skip the
// output byte-equality reverify sub-check when the plan contains that rule.
pub fn is_output_neutral(rule: &str) -> bool {
    rule != "legacy-interpolation"
}
```

The `fixable` flag in the canonical JSON output is computed as `(fix_removals.is_some() || fix_edits.is_some()) && tier::is_fixable(rule, is_standalone)` inside `to_canonical_json()` — it is NOT stored on `LintDiagnostic` directly.

### Rule Semantics — Key Edge Cases

**legacy-interpolation**: Token-based scan over `Token::Text` (skips fences/frontmatter/directives automatically) and `Token::Directive` (for `@message {expr}:` patterns). Detects: `\{`/`\}` remnants (fix = delete backslash), `${...}` (skip, never flagged), `{expr}` (fix = atomic TextEdit replacing entire `{expr}` with `{{expr}}`), `@message {expr}:` dynamic role (same fix). Edits are always single atomic TextEdits — never split open/close. Suppressible via `mds.json`.

**Suppression configs**: `examples/edge-cases/mds.json`, `examples/stress-test/edge/mds.json`, and `crates/mds-cli/tests/fixtures/mds.json` all set `"legacy-interpolation": "off"` — these directories intentionally contain literal-brace teaching content.

**unreachable-branch**: The rule only fires on literal↔literal comparisons (`@if "x" == "x":`). Always-true conditions only flag IF there are LATER branches to be unreachable. Each duplicate `@elseif` gets at most one finding — "duplicate" OR "always-true/false", never both. Diagnostic spans use `branch.offset`. All messages end with trailing periods. Per-case `fix_removals`: A/B/C/E/G have `Some(spans)`, D/F have `None` (when @elseif branches make safe removal ambiguous).

**empty-block**: "Empty" = `body.is_empty()` OR all nodes are whitespace-only `Text`. The `@block` directive is deliberately excluded (intentional "inherit parent default" pattern). All messages end with trailing periods. Per-case `fix_removals`: whole-block removal (`to_inclusive=true`) for ①@for ②@define ③bare-@if; `None` for ④@if-with-branches (unsafe partial removal); exclusive removal (`to_inclusive=false`) for ⑤@else ⑥terminal-@elseif; `None` for ⑦/⑧ non-terminal-@elseif or @else-follows; `@message` always `None`.

**unused-import (merge)**: Merge imports (`@import "path"`) are always treated as used — they inject all exports plus `prompt` into scope. Conservative; false negatives are acceptable here.

**unused-variable**: Uses are tracked across all recursive bodies: `Expr::Var`, `Arg::Var`, `Arg::MemberAccess`, condition operands, `@for` iterables, call arguments. Code-fence content is `Text` (interpolation suppressed) — correct by construction. Reserved skip-set: `{imports, type, extends, prompt}`.

**unused-* suppression**: When `ctx.is_partial_or_extends == true` (file starts with `_` OR has `@extends`), the unused-variable, unused-import, and unused-function rules are entirely suppressed. shadow-variable is NOT suppressed — it is already default-off.

**wildcard re-export exemption**: The `@export * from "path"` directive does NOT exempt an `@import` from being flagged as unused. Only `Named`/`ReExport` exports suppress their corresponding import.

### Configuration

`mds.json` `lint.rules` section:
```json
{ "lint": { "rules": { "unused-variable": "off", "shadow-variable": "warn" } } }
```

**Unknown rule NAMEs** → warned about on every surface, then ignored; the config still
loads and lint continues, and the exit code does not move (forward compat: a config naming
a rule from a newer release must not break an older binary). The CLI writes the warning to
stderr (suppressed by `--quiet`); napi/WASM/Python return it in `lint_warnings`.
The registry is `mds::KNOWN_LINT_RULES`, derived from each rule module's own `RULE` const;
detection is `mds::find_unknown_rule_names`. (#224)  
**Unknown severity VALUES** → hard parse error → exit 2 (closed enum, no sensible fallback).

`LintConfig` lives in `mds-core` (not mds-cli). The CLI `LintCliConfig` from `build.rs` converts to it via `into_core_config()`.

`LintConfig::from_rules` was **deleted** in PR #308 — it shipped deprecated-since-birth and was never published. All callers use `from_rules_checked`. This also removed the `#[expect(deprecated)]` scaffolding from `crates/mds-core/tests/api_surface.rs`.

## Technical Implementation Patterns

### Engine Pipeline (per-file)

```rust
// lint_source() in crates/mds-core/src/lint/mod.rs — the engine entry point.
// Called by the public mds::lint(), mds::lint_str_with(), mds::lint_virtual() API.
pub(crate) fn lint_source(source, filename, config) -> Result<LintResult, MdsError> {
    // Step 1: re-parse entry independently (mirrors scan_imports pattern)
    let tokens = lexer::tokenize(source, filename)?;
    let module = parser::parse_with_ctx(&tokens, filename, source)?;

    // Step 2: facts walk — one traversal building AnalysisContext.
    let ctx = collect_facts(&module, is_partial || is_extends, source)?;

    // Step 3: standalone detection for Tier B eligibility
    let is_standalone = !ctx.is_partial_or_extends && ctx.imports.is_empty();

    // Step 4: non-generic rule dispatch (10 plain fn calls).
    // Token-based rules (Step 6) receive &tokens + source; AST rules receive &module + &ctx.
    run_rules(&module, &ctx, &tokens, source, filename, config, &mut builder);

    Ok(builder.build(is_standalone))
}
```

Key invariants:
- The check gate (resolve+validate) runs ONCE in the public `mds::lint()` wrapper before calling `lint_source`. The engine never calls the resolver again.
- Per-file fresh resolve is intentional (v1). There is NO cross-file `ModuleCache` — it would be unsafe under per-file runtime vars.
- `run_rules` threads both `&[Token]` (the raw token stream) and `source: &str` through so token-based rules (currently `legacy-interpolation`) can operate without re-tokenizing.

### AST: ElseifBranch, IfBlock, ForBlock, DefineBlock

`ElseifBranch` carries a per-branch offset. `IfBlock`, `ForBlock`, and `DefineBlock` carry `end_offset` for fix span computation:

```rust
pub struct ElseifBranch {
    pub condition: Condition,
    pub body: Vec<Node>,
    pub offset: usize,  // byte offset of @elseif token — used for diagnostic spans
}

pub struct IfBlock {
    pub condition: Condition,
    pub then_body: Vec<Node>,
    pub elseif_branches: Vec<ElseifBranch>,
    pub else_body: Option<Vec<Node>>,
    pub offset: usize,
    pub else_offset: Option<usize>,  // byte offset of @else token, when present
    pub end_offset: usize,           // byte offset of @end token (for fix span computation)
}
```

`structural_eq.rs` compares `ElseifBranch` by `condition` and `body` only — `offset` is excluded. `end_offset` on all block types is excluded from structural equality. `DefineFact` in `facts.rs` mirrors this: `DefineFact { name, offset, end_offset }` — the facts walker copies `b.end_offset` from the AST so fix rules have it without re-walking.

### The `--fix` Pipeline

The fix pipeline is split into a pure core (`fix.rs`) and an I/O layer (`lint.rs`).

**fix.rs** (pure, no I/O):

Fix edits are driven by two complementary fields on `LintDiagnostic`:
- `fix_removals: Option<Vec<FixLineSpan>>` — line-range removals. `FixLineSpan` encodes `from`/`to`/`to_inclusive`. Used by all Tier A/B rules except `legacy-interpolation`.
- `fix_edits: Option<Vec<TextEdit>>` — in-place replacements. `TextEdit { start: usize, end: usize, new_text: String }` where `start` is inclusive, `end` is exclusive, and empty `new_text` is a pure deletion. Used by `legacy-interpolation`.

Both paths go through `diag_to_edits(diag, source) -> Vec<ByteEdit>`. The `fix_removals` path produces `ByteEdit { replacement: String::new() }` (pure deletion); the `fix_edits` path produces `ByteEdit { replacement: edit.new_text.clone() }` with a char-boundary guard (fail-closed, ADR-001). `apply_plan_unchecked` applies edits right-to-left via `replace_range`, handling both deletions and replacements uniformly.

Planning steps in `plan_fixes_with_options`:
1. Collect `ByteEdit`s from fixable diagnostics via `diag_to_edits`.
2. Sort edits by `(start ASC, end DESC)` — widest edit first among same-start edits.
3. **Containment coalescing** (`dedup_contained_or_identical`): drop any edit whose byte range is fully contained within (or identical to) an earlier retained edit.
4. **Overlap detection**: after containment deduplication, any remaining partial overlap causes the whole batch to be cleared (`overlap_rejected = true`). Fail-closed.

`FixOutcome` (returned by `apply_fixes` and `apply_fixes_incremental`) has four variants: `Fixed { source, residual }`, `PartiallyFixed { source, residual, rejected }`, `Rejected { source, reason }`, `NothingToFix`.

**Reverify gate (AC-F-20)**: After applying edits, a reverify callback checks three conditions: (1) recompile-success; (2) no-new-untargeted-diagnostics; (3) output byte-equality for standalone files **when all edits in the plan are output-neutral**.

The output-equality sub-check (3) is **skipped** when the plan contains any edit from `legacy-interpolation` (the only non-output-neutral rule). Both `plan_and_apply_fixes` and `preview_fixes` in `lint.rs` compute `all_output_neutral = plan.edits.iter().all(|e| mds::fix::is_output_neutral(&e.rule))` and gate the equality check on it. This bypass applies to the whole batch — a mixed plan containing `legacy-interpolation` alongside output-neutral rules skips the equality check for co-batched neutral edits too (assessed P2 risk).

**`reverify_failure_reason(err)` in `fix.rs`** — the ONLY construction site for `FixOutcome::Rejected.reason`. It WIRE-escapes `err.to_string()` via `sanitize_control_chars_wire` so the CLI can print `fix rejected: {reason}` as a bare status line without further escaping. Construction-time sanitization, not print-time.

**CLI's `plan_and_apply_fixes`** (in lint.rs): The short-circuit for `NothingToFix` guards on `plan.edits.is_empty() && !plan.overlap_rejected`. When overlap was detected, `plan.edits` is cleared but `plan.overlap_rejected = true` — so the function falls through to `apply_fixes_incremental`, which immediately returns `Rejected`.

**Emit-before-exit ordering rule** (`lint.rs` output-contract, PF-004 recurring defect class — 7 realized defects across ~33 emitter sites): The invariant is **emit the envelope before any early exit; write first, then print/accumulate**. Two defects fixed in PR #308: (1) `lint <file> --fix --check --format json` called `process::exit(1)` before `emit_result`, emitting zero stdout bytes (AC-F-14 held for `<dir>` but not `<file>`); (2) JSON write-failure arms pushed the post-fix result before the write, so a failed write emitted `{"files":[],…}` (reads as a clean tree) with exit 2. The `Fixed:` arms were already correct; `PartiallyFixed` arms were not. The `ResultSink` redesign that makes `--quiet` structurally unbypassable is deferred to issue **#309**.

**Atomic write** (`atomic_write_file` in `output.rs`, imported by both `lint.rs` and `fmt.rs`): TOCTOU guard + permissions restore + `sync_all()` + `persist()` (intra-filesystem rename). Temp prefix `.mds-tmp-`. `Fixed:` is printed only AFTER a successful write.

### CLI Preview Pipeline (`preview_fixes`)

`preview_fixes` in lint.rs routes through the same gated pipeline as the write path — it calls `apply_fixes_incremental` with the full reverify closure and does NOT write to disk.

`PreviewOutcome` has three variants:
- `WouldFix { fixed: String, residual: mds::LintResult }` — at least one edit would apply; `fixed` is the would-be source text (used for `--diff` rendering); `residual` is the post-fix lint result (used for the R1 preview exit code)
- `Rejected(String)` — every edit refused by the reverify gate
- `NothingToFix` — no edits to apply

The `WouldFix` variant carries a named `residual` because the preview exit code is derived from it, not from a simple "any file would change" boolean:

```
preview_exit_code(residual) = result_exit_code(residual).max(1)
```

The `.max(1)` floors the exit at 1 (signaling "fixes are pending" for the `--fix --check` CI contract). Residual Error-severity findings push the exit above 1: a tree where `--fix` would leave Error findings behind exits 2 even though every file "would fix". This formula governs both `--fix --check` and `--fix --diff`.

`display_label` (the caller-supplied relative, forward-slash-normalised path) is passed to both `plan_and_apply_fixes` and `preview_fixes` and applied to every residual diagnostic's `file` field via `set_diag_display_path`. Without it the residual would leak the basename or `STRING_SOURCE_MAP_LABEL` instead of the navigable relative path.

**Directory mode**: In the `WouldFix` arm, the per-file tally comes from the RESIDUAL (`tally_from_result(residual)`), not the pre-fix result. Summary-bucket counters (`warn_file_count`, `error_file_count`, etc.) are residual-based under preview. `any_would_fix = true` then floors the aggregate at 1 via `exit = max_tally.exit_code().max(if any_would_fix { 1 } else { 0 })`. A former early-exit `if flags.check && any_would_fix { exit(1) }` shadowed `max_tally` — residual errors would have exited 1 instead of 2. That early-exit is removed.

The three-way gate in both single-file and directory modes: `fix && !check && !diff` → write path; `fix && (check || diff)` → preview path; report-only (no `fix`) → `mds::lint` + render.

### Directory Mode: Per-File Config Discovery

In directory mode, `mds lint` no longer loads a single root `mds.json` — each file independently walks up to its nearest `mds.json`. This is managed by `LintDirCtx`:

```rust
struct LintDirCtx<'a> {
    flags: LintFlags,
    runtime_vars: &'a Option<HashMap<String, mds::Value>>,
    config_cache: RefCell<HashMap<PathBuf, Rc<mds::LintConfig>>>,
}
```

A config-load failure for a nested `mds.json` is a per-file error: it emits the error and continues linting other files, but the overall exit code reflects the failure (exit 2).

### Display Path Remapping (`set_diag_display_path`)

`mds::lint(path, …)` sets each diagnostic's `file` field to the file's **basename**. In directory mode, `set_diag_display_path` replaces this with the relative path (relative to the lint root) immediately after every `mds::lint` call.

### Cross-Surface Options Validation (`packages/mds`)

`packages/mds/src/util/options.ts` exports `assertKnownKeys(options, method)`, which enforces strict unknown-option rejection at the wrapper layer before dispatching to any backend.

- Error code: `'mds::invalid_options'` (satisfies `isMdsError`).
- Message format is byte-matched to the backend's `format_unknown_keys_error`.
- `CheckOptions { vars? }` is a separate interface from `CompileOptions` (which also carries `sourceMap`, `sourcesContent`).

### Python Surface

`crates/mds-python/src/lib.rs` exposes fully typed frozen result classes:

- `LintDiagnostic`: `#[pyclass(frozen)]` with `.rule`, `.severity`, `.message`, `.help`, `.fixable`, `.span`, `.fix_edits` attributes. Supports pickling via `__reduce__`.
- `fix_edits` is a custom `#[getter]` (not `#[pyo3(get)]`) because `Vec<serde_json::Value>` does not implement `IntoPy`. Returns `list[dict] | None`. Pickle round-trips `fix_edits` as a JSON string (`fix_edits_json: Option<String>`) via `__reduce__`/`__new__`.
- `LintDiagnostic.as_json()` inserts keys in alphabetical order explicitly (serde_json::Map with insertion order): `fix_edits`, `fixable`, `help`, `message`, `rule`, `severity`, `span` — emitting `fix_edits`, `help`, and `span` unconditionally as `null` when `None` (PF-007 parity).
- `LintFileReport`: `#[pyclass(frozen)]` per-file findings group with `.file` and `.diagnostics`.
- **`serde_json::Map` is `BTreeMap`** when `preserve_order` is not enabled (it is not enabled in this codebase). Keys serialize alphabetically by construction regardless of insertion order.
- **Parity fixtures must use `write_bytes`, not `write_text`** (PF-020): `write_text` applies newline translation on Windows, breaking byte-equality comparisons with the canonical JSON emitted by `to_canonical_json()`.

### WASM Surface

`parse_check_options` in `crates/mds-wasm/src/lib.rs` uses a strict allow-list: `reject_unknown_wasm_keys(&obj, &["filename", "modules", "vars"])?;` — the `check()` export rejects any option key not in `[filename, modules, vars]`.

### Canonical JSON Wire Format

`LintResult::to_canonical_json()` is THE single serializer. All five surfaces call it:

```json
{
  "version": 1,
  "files": [
    {
      "file": "path/to/file.mds",
      "diagnostics": [
        {
          "rule": "legacy-interpolation",
          "severity": "warn",
          "message": "...",
          "help": "...",
          "fixable": true,
          "span": { "offset": 6, "length": 6 },
          "fix_edits": [{ "start": 6, "end": 12, "new_text": "{{name}}" }]
        }
      ]
    }
  ],
  "truncated": false
}
```

**SPAN-1**: `span.line` and `span.column` are NOT part of the stable wire format. All 10 lint rules pass `None` for `line`/`column`.

**`fix_edits` field**: emitted unconditionally on every diagnostic — `null` when the rule uses `fix_removals` or has no fix, `[{start,end,new_text}]` when populated. Because `to_canonical_json()` builds its map via `serde_json::json!` and serde_json's `Map` defaults to `BTreeMap`, keys are sorted alphabetically: `fix_edits` appears before `fixable`. File ordering in `files` is deterministic (BTreeMap sorted by filename). `fixable` is NOT stored on the diagnostic struct.

**Error-only entries and `fix_edits[].new_text` sanitization (ADR-008 resolved — Option B)**: Analysis-failure entries (no diagnostics, only an `"error"` key) bypass `to_canonical_json` entirely; their `file` key is sanitized at the push site via `file_key = sanitize_control_chars_wire(&display_path)`. Both normal and error-only entries therefore carry identically-sanitized `file` values. **`fix_edits[].new_text` IS NOW WIRE-sanitized** in `to_canonical_json` (`sanitize_control_chars_wire(&e.new_text)`) — ADR-008 closed as Option B: sanitize the display wire, leave the functional `--fix` path raw. The stored `LintDiagnostic.fix_edits` field is NOT sanitized so the `--fix` path (`diag_to_edits` → `ByteEdit.replacement`) reads original bytes unchanged; `to_canonical_json` applies the WIRE sanitizer before building the `serde_json::json!` value, which is upstream of all four surfaces (CLI, napi, WASM, Python).

**Per-file cap**: `MAX_DIAGNOSTICS = 1000` (`crates/mds-core/src/limits.rs`). **File-less key**: `"<unknown>"`. **Analysis failure envelope** (JSON mode only): `{ "version": 1, "error": { "code": "...", "message": "...", "help": "...", "span": {...} } }`. **`--fix --format json --stdin` is a usage error** — exits 2 with a plain stderr message (AC-F-22b).

## Sanitization Discipline

This is the most important section for any agent working on security or error-output paths. Read it before touching anything in `output.rs`, `diagnostic.rs`, `error.rs`, or any file that calls `eprintln!`.

### The Governing Principle: Per-Field, Not Per-Surface

The design is **input-sanitizing, not output-sanitizing** (avoids PF-014). The single governing rule, normative in spec §7.5:

> **On the diagnostic surfaces — the `"version": 1` JSON wire, CLI status and warning lines, `[file:line:col]` frame headers — untrusted identifiers, filenames, and error causes are WIRE-escaped, human terminal output included. Prose — a diagnostic message body or help body — stays HUMAN on terminal surfaces so multi-line frames keep rendering.**

**The rule governs diagnostic output only.** Two categories are named carve-outs and are not escaped at all:

1. **The command's product** — compiled template output (`mds build -o -`, `mds lint --fix -`). Escaping it would corrupt every redirect.
2. **Functional path references** — source-map `file` / `sources` / `sourcesContent` (both the `mds build --source-map` sidecar and the `sourceMap` embedded in `CompileResult::to_canonical_json()`), and `CompileResult.dependencies`. These paths are emitted **verbatim**, control bytes and all: devtools, bundlers and IDEs resolve them against the filesystem, so an escaped path would not exist. Consumers MUST treat them as untrusted and escape them for their own destination — JSON string encoding is not escaping, since a decoded `"\n"` is a real newline again. The CLI does not depend on this: `Compiled to …` / `Source map written to …` print through `safe_path`. See spec §7.5 "Carve-out: functional path references".

A reviewer reproduced (2026-07-26) that a file named with a real `\n` and ESC yields a sidecar whose decoded `file` / `sources` carry those bytes while the CLI status line for the same run is escaped. That is the carve-out working as designed, not a gap — but the per-field sentence used to claim otherwise, which is why it is now scoped to diagnostic surfaces.

The discriminator is whether the value is ever legitimately multi-line. A filename, an `mds.json` rule name, a `--format` argument, and an `io::Error` cause are each rendered on exactly one line (a status line or a `[file:line:col]` frame header) — a raw `\n` in one forges a standalone line byte-identical to genuine output (CWE-117). A diagnostic body genuinely is multi-line, so escaping its newlines breaks the frame.

This rule supersedes the old "wire mode at exactly these four boundaries" enumeration. Enumerating boundaries went stale twice under review; the per-field rule makes each new site decidable without re-deriving the list.

### The Escape Class and the Two Modes

**The escape class** — identical for both modes: C0 (U+0000–U+001F), DEL (U+007F), C1 (U+0080–U+009F), all 12 Unicode `Bidi_Control=Yes` members (U+061C, U+200E/U+200F, U+202A–U+202E, U+2066–U+2069 — Trojan Source / CVE-2021-42574), the JS line/paragraph separators U+2028/U+2029, and U+FEFF (BOM). Each hostile character is replaced by an uppercase `\uXXXX` 6-char literal. `\t` (U+0009) is exempted from both modes. `\n` (U+000A) is IN the class — but whether it is escaped depends on mode, not on the class definition.

Two modes, one shared implementation via `sanitize_with(s, EscapeMode)`:

- **HUMAN** (`sanitize_control_chars(s)`) — preserves `\n`. For terminal/miette render output where multi-line frames must stay readable.
- **WIRE** (`sanitize_control_chars_wire(s)`) — also escapes `\n` → `
`. For JSON wire, binding error objects, status lines, any line-oriented consumer of the string value.

Both return `Cow<'_, str>`: borrowed on clean input (zero allocation), owned only when a hostile character is actually present. Both are idempotent.

**Key byte-level detail**: The fast-path scan checks for bytes `< 0x20`, `0x7F`, `0xC2` (C1 prefix), **`0xD8`** (U+061C prefix), `0xE2` (U+200E/U+202E/U+2028 etc. prefix), `0xEF` (U+FEFF prefix). The `0xD8` byte must be in the fast-path scan or U+061C short-circuits to `Cow::Borrowed` without being inspected.

### The `neutralize_source_for_render` Byte-Width Branches

Source text passed to `NamedSource` uses a different function: `neutralize_source_for_render(s)` — byte-length-preserving substitution so span offsets and caret alignment stay exact:

- **C0/DEL (1-byte)** → `?` (1 byte)
- **C1 (U+0080–U+009F) AND U+061C** (both 2-byte UTF-8) → U+00A0 NBSP (2 bytes). U+061C is in the 2-byte branch.
- **The other 11 format hazards** (U+200E/U+200F, U+2028/U+2029, U+202A–U+202E, U+2066–U+2069, U+FEFF — all 3-byte) → U+FFFD REPLACEMENT CHARACTER (3 bytes).

The split is implemented via two private predicates: `is_two_byte_format_hazard(ch)` (only U+061C) and `is_three_byte_format_hazard(ch)` (the remaining 11). A `debug_assert_eq!` in `neutralize_source_for_render` catches byte-length violations immediately during development.

### `named_source_for_render` — The Single NamedSource Builder

`named_source_for_render(file: &str, source: &str) -> miette::NamedSource<String>` is now the ONLY `NamedSource` builder in the codebase. Three callers: `MdsError::at()` (error.rs), `check_equivalence` (formatter.rs), `render_diag_human` (mds-cli/src/lint.rs). Contract:

- **filename** → `sanitize_control_chars_wire` (WIRE — a filename is never legitimately multi-line).
- **source** → `neutralize_source_for_render` (byte-length-preserving — keeps every span offset and caret column exact).

### `SanitizedReport` — The CLI stderr Choke-Point

`eprint_error` wraps every `miette::Report` in a `SanitizedReport` before miette renders it. This covers **both** CLI error families: `MdsError` (compiler diagnostics) and CLI-authored `miette::miette!()` reports — wrapping at the `Report` level means any error type added later inherits the guarantee without touching a downcast ladder (avoids PF-004).

`SanitizedReport` overrides every prose surface — the `Display` message, the `help` text, each `LabeledSpan`'s label text — with HUMAN-mode sanitized copies, while delegating `code`, `severity`, `url`, `source_code`, and each label's **byte span** to the inner report untouched (so `named_source_for_render`'s byte-length neutralization keeps every span exact).

The auxiliary diagnostic graph (`source` cause chain, `related`, `diagnostic_source`) cannot be forwarded by reference; the wrapper materialises the whole graph into owned `SanitizedNode`s at construction, bounded by `MAX_AUX_DEPTH = 16` to prevent infinite loops from a cyclic `source()` (avoids PF-005). An earlier revision returned `None` for `source()`/`related()` behind a `debug_assert!` — the guarantee was real in tests and absent in the shipped binary. Now enforced by data transformation.

### The Complete Boundary Table

| Boundary | Mode | Fields |
|----------|------|--------|
| `eprint_error` (output.rs) via `SanitizedReport` | HUMAN for prose | message, help, label text, entire auxiliary graph — every report rendered to CLI stderr |
| `eprint_warning` (output.rs) | prose HUMAN; interpolated identifiers/paths WIRE | HUMAN for the warning body prose; `safe_path` / `safe_inline` for any untrusted value the caller interpolates into it |
| `safe_inline(value)` (output.rs) | WIRE | any single-line untrusted value interpolated into a status, warning, or error line: rule names, config paths, `--format` args, `io::Error` causes |
| `safe_path(p)` / `safe_file_display(name)` (output.rs) | WIRE | CLI status-line path display (`Clean:`, `Fixed:`, `Would fix:`, `Compiled to`, …) |
| `named_source_for_render(file, source)` (diagnostic.rs) | WIRE for filename; neutralize for source | the single `NamedSource` builder used by `MdsError::at()`, `check_equivalence`, `render_diag_human` |
| `render_diag_human` (lint.rs) | HUMAN | message/help (filename and source go through `named_source_for_render`) |
| `fix::FixOutcome::Rejected.reason` (fix.rs) | WIRE | construction-time via `reverify_failure_reason(err)` — the CLI prints `fix rejected: {reason}` as a bare status line |
| `MdsError::serialize()` (error.rs) | WIRE | message, help — covers all three bindings' error path |
| `LintResult::to_canonical_json()` (diagnostic.rs) | WIRE | message, help, `files[].file` key; error-only entries bypass this — their `file` key is sanitized at the push site via `sanitize_control_chars_wire` |
| `CompileResult::to_canonical_json()` (lib.rs) | WIRE | warning strings (distinct method, not a duplicate) |
| `emit_warnings()` (lib.rs) | HUMAN for prose; WIRE at construction for identifiers interpolated in `resolver.rs`/`evaluator.rs` | warnings printed to stderr |
| Python `LintResult::new()` via `sanitize_lint_value()` | WIRE | message, help, file — construction-time, so typed getters read pre-sanitized data (closes PF-004 parallel-path) |
| `--diff` preview output (output.rs) | neutralized on TTY, byte-faithful when piped | `preview_text_for(writer_is_tty, text)` — redirected diffs stay `patch`-applicable |

**`--check` is NOT TTY-gated** — it emits only status lines, which are unconditionally sanitized via `safe_path`. Only `--diff` calls `preview_text_for`. This distinction matters.

**Deliberate residual (not closed):** `MdsError` message bodies and CLI `miette!()` message construction interpolate untrusted text as prose (paths, `io::Error` causes, identifiers). `eprint_error` applies HUMAN mode before miette renders them, so no raw control byte reaches stderr — but a `\n` in an interpolated path or identifier survives *inside the rendered frame*. Frame content is `│`-prefixed and indented, so it cannot masquerade as a bare status line. Not closed because there are 110+ `MdsError::*(format!(…))` construction sites; fixing a few would leave the claim false at ~100 others. Named in spec §7.5.

**Escaping is one-way** — the transformation is lossy and non-injective: a template literally containing `\u001B` and one containing an actual ESC byte produce identical output. Consumers MUST NOT un-escape `\uXXXX` sequences back into bytes. Round-tripping is an explicit non-goal.

### The Print-Discipline Guard

`crates/mds-cli/tests/print_discipline.rs` is a CI-enforced test that **lexes** `crates/mds-cli/src/**` and FAILS if any print macro interpolates a value that is not a call to one of the accepted sanitizing helpers.

Accepted helpers (SANITIZERS): `safe_path`, `safe_file_display`, `safe_inline`, `sanitize_control_chars_wire`, `render_error_sanitized`. **HUMAN `sanitize_control_chars` is deliberately NOT in SANITIZERS** — it preserves `\n`, so it cannot make an identifier safe. That is the M2 finding: routing an `mds.json` rule name through `eprint_warning` (HUMAN) still forged three standalone status lines.

The guard:
- Traces `let` bindings **one hop** in the same file — hoisting a `format!` out of the call is checked exactly as if written inline.
- **Fails closed on untraceable arguments** (function parameters, loop variables, unrecognised expression shapes) — false positives cost one allowlist entry with written justification; false negatives cost another review round.
- **Poisons non-`let` binder names** (`collect_non_let_binders`). `let`s are matched file-wide, so before this a `for` variable / parameter / closure param was resolved against unrelated `let`s of the same name and accepted if all of them were safe. Proven live: `for label in rules { eprint_warning(label) }` injected into the real `lint.rs` (which has three `let label = safe_path(…)`) passed the guard; it now fails with `lint.rs:1547: eprint_warning(untraced) interpolates unsanitized \`label\``.
- Covers `eprint_warning` arguments too — the whole `format!` nested inside must have every interpolation pass through an accepted helper.
- Allowlists are keyed by `(file, expression)` — an `every_allowlist_entry_is_live` test fails if an entry stops matching, preventing silent staleness.

**Documented limits** (five, in the file's own rustdoc): sanitizers are matched by the last path segment of the callee (an alias or a local function named `safe_path` would pass); binding traces are one hop within one file; allowlist exemptions are anti-rot but not anti-reuse (a new variable reusing an exempted name in the same file inherits the exemption silently); `write!` stream detection is by name; and the poison set models only `for` / parameter / closure binders, not `if let` / `while let` / `match`-arm ones. These limits close *accidental* reintroduction (which is what all four review rounds of #176 involved) — not intentional defeat.

**The one cross-crate precondition** — that `mds-core` WIRE-escapes the identifiers its warning producers interpolate, since `mds-cli` prints whole warning strings through HUMAN-mode `eprint_warning` — is pinned by `crates/mds-cli/tests/producer_discipline.rs`. `mds-core` has exactly three such producers: `resolver.rs`'s imported-module filename (testable, and tested — a module key is a filesystem path) and `evaluator.rs`'s two `@include` alias warnings (**not** testable: the parser restricts an alias to `[A-Za-z_][A-Za-z0-9_]*`, so a test would be vacuous per PF-013 — upheld by review, and stated as such).

This guard exists because three consecutive review rounds of #176 each found a NEW bare `eprintln!` after the previous one was fixed. It converts an unbounded reviewer search into a bounded enforced invariant.

### Cross-Surface ESC-Injection Test Anchors

The test anchor inventory covers five surfaces across both error and lint paths.

**T-1..T-3** `error_tests.rs` — serialize path: ESC/DEL/C1  
**T-4** `diagnostic.rs` — `to_canonical_json`: bidi override (U+202E) in message/help/file key; span offsets byte-accurate  
**T-5..T-9** `cli_lint.rs` — single-file, dir, stdin, DEL/C1, JSON (T-9 rewritten non-vacuous: duplicate-import + U+0085 NEL vector)  
**T-10a/b/c** `output.rs` — `neutralize_source_for_render` byte-length invariant, caret alignment, miette SGR survives hostile OSC  
**T-11** `error.spec.mjs` universal JS — differential  
**T-11a/b** `output.rs` — `safe_path` (ESC → escaped literal, clean passthrough)  
**T-12/T-13** `index.spec.mjs` napi  
**T-14** `test_errors.py` Python (DEL/NEL params)  
**T-15** `web.rs` WASM (F5/F5-DEL/F6/F6-C1)  
**T-16f** `diagnostic.rs` — U+2028 in wire message  
**T-16g** `diagnostic.rs` — wire-mode newline escaping / HUMAN mode preserves `\n`  
**T-16h** `diagnostic.rs` — WIRE and HUMAN modes differ only on `\n`  
**T-16i** `diagnostic.rs` — WIRE mode: borrowed-on-clean, idempotent  
**T-NS-1/2/3** `diagnostic.rs` — `named_source_for_render`: hostile filename WIRE, hostile filename bidi class, source neutralized without changing byte length  
**T-AUX-1/2/3** `output.rs` — `SanitizedReport`: cause chain escaped+preserved, related diagnostics escaped+preserved, cyclic cause chain bounded at `MAX_AUX_DEPTH`  
**T-ESC-5/6/7** `output.rs` — label text escaped/span preserved, PF-014 colour path, inert on clean input  
**T-WARN-1/2/3** `output.rs` — `eprint_warning`: C0, clean passthrough, bidi  
**T-REASON-1/2** `fix.rs` — `reverify_failure_reason` WIRE on both construction paths  
**T-ESC-MSG-1/2** `security.rs` — `MdsError` and CLI-authored message escaping  
**T-ESC-RULE-1** `security.rs` — unknown `mds.json` rule name with embedded control bytes  
**T-ESC-FNAME-1/2** `security.rs` — `\n` in filename cannot forge standalone status line (build and lint)  
**T-ESC-WALK-1** `security.rs` — walker depth-limit warning hostile directory name  
**Print-discipline self-tests** `print_discipline.rs` — `the_guard_flags_a_bare_interpolating_print`, `the_guard_follows_a_hoisted_format_binding`, `the_guard_reports_an_untraceable_helper_argument`, `cli_print_sites_sanitize_every_interpolated_value`, `every_allowlist_entry_is_live`

### CLI Exit Codes

Lint uses **direct `std::process::exit`**, never the shared `exit_code()` function.

| Code | Condition |
|------|-----------|
| 0 | Clean — no Warn or Error findings |
| 1 | Warn-severity findings only, no errors; preview mode (`--fix --check` / `--fix --diff`) when any file would change and residual has no Error findings |
| 2 | Any Error-severity finding OR analysis failure (parse, syntax, nesting-overflow) OR usage error; preview mode: residual has Error findings |
| 3 | ResourceLimit — `MAX_BLOCKS_PER_MODULE=256` exceeded in the resolver's `collect_block()` |

`Info` severity never contributes to exit code. With `--fix`, residual post-fix findings determine the code (see `preview_exit_code` above).

**R4 carve-out — `mds::var_conflict`**: A `--set`/`--set-string` key collision exits **1** on ALL subcommands including lint, not 2. This is consistent with how `build`/`check`/`watch` handle it via `exit_code()`. Implementation: a one-variant-wide downcast in `do_lint` matches `MdsError::VarConflict { .. }`, routes through `emit_analysis_failure_json_or_stderr` (so `--format json` consumers get the structured envelope), then calls `process::exit(1)`. Every other setup error keeps the blanket exit 2 in `run_lint`'s catch.

**R6 — `mds::module_not_found`**: When `lint_virtual` (or the public `mds::lint_virtual` in mds-core) is called with an `entry` key absent from the `modules` map, the resolver returns `MdsError::module_not_found(entry)` rather than the file-surface `FileNotFound`. This preserves error-code accuracy on virtual surfaces — `VirtualFs` has no filesystem, so `FileNotFound` would be misleading.

**WARN-A / WARN-B — `@include` empty-output warnings (R2)**: Two variants of the empty-`@include` warning exist in the resolver. WARN-A fires when the included module has no prompt body at all. WARN-B fires when the module has prompt body text but its `@export` list explicitly excludes `"prompt"` — the `@include` caller still sees empty output because the body is suppressed by export gating (`prompt_suppressed_by_exports = true` in `NamespaceScope::to_namespace_scope`). Both warnings surface in the `warnings` field of compile/lint results.

## State Transitions

### How a finding becomes a fix (apply_fixes_incremental path)

```
LintDiagnostic.fix_removals (FixLineSpan)  OR  .fix_edits (TextEdit)
  → diag_to_edits() → Vec<ByteEdit { start, end, rule, replacement }>
  → plan_fixes_with_options():
      sort (start ASC, end DESC)
      → dedup_contained_or_identical() — drop edits contained in wider edits
      → overlap detection — any partial overlap: FixPlan { overlap_rejected: true, edits: [] }
  → apply_fixes_incremental():
      all_output_neutral? → skips equality gate if any edit is legacy-interpolation
      batch attempt (1 reverify call) → passes? → FixOutcome::Fixed
      batch fails → per-edit right-to-left retry → accept | RejectedEdit
        all rejected → FixOutcome::Rejected { reason: reverify_failure_reason(&err) }
        ≥1 accepted, ≥1 rejected → FixOutcome::PartiallyFixed
  → CLI: Fixed/PartiallyFixed → atomic_write_file()
       | Rejected → "fix rejected: ..." + original diagnostics
       | NothingToFix → pass through
```

## Anti-Patterns

- **Re-inlining the tier table**: Adding tier logic to `fix.rs` or `diagnostic.rs` instead of importing from `tier.rs` recreates the circular dependency the leaf module was designed to prevent.

- **Splitting a TextEdit into separate open/close edits**: `legacy-interpolation` must emit one atomic `TextEdit` replacing the entire `{expr}` span. Split edits allow per-edit fallback to accept only the close half, producing `{expr}}` garbage that compounds on repeated `--fix` runs.

- **Forking a second escape map** (the anti-pattern `sanitize_control_chars_wire` was created to prevent): WIRE and HUMAN share one implementation via `sanitize_with(s, EscapeMode)`. A second table that duplicates the character class but changes one entry will diverge silently when the class is extended.

- **Using HUMAN mode for an identifier or filename**: `sanitize_control_chars` (HUMAN) preserves `\n`, so a hostile filename routed through it can still forge a standalone status line. Use `safe_path`, `safe_file_display`, or `safe_inline` (all WIRE) for identifiers and filenames.

- **Adding a bare interpolating `eprintln!`**: `crates/mds-cli/tests/print_discipline.rs` will fail CI immediately. The guard checks `crates/mds-cli/src/**` and fails on any interpolation not routed through a WIRE helper.

- **Post-processing a rendered miette frame with any sanitizer** (PF-014): Sanitizing the rendered output escapes miette's own ANSI SGR colour codes into `\u001B[33m` noise on TTYs. CI uses `NO_COLOR=1` and piped stderr so this regression would stay green indefinitely. Pre-sanitize inputs before constructing the `Report`.

- **Putting U+061C in the 3-byte neutralization branch**: U+061C is 2 bytes in UTF-8. Routing it through the 3-byte branch (`U+FFFD`) fires the byte-length `debug_assert_eq!` (13 vs 12 bytes). This was proven, not theorized, during the #176 development. U+061C belongs in `is_two_byte_format_hazard`.

- **Omitting `0xD8` from the fast-path byte scan in `sanitize_with`**: U+061C is encoded as `0xD8 0x9C`. Without `0xD8` in the fast-path, `sanitize_control_chars("a\u{061C}b")` returns `Borrowed` and skips the character entirely.

- **Resting a security invariant on `debug_assert!`** (PF-005): `debug_assert!` is compiled out of release. The old `SanitizedReport` returned `None` for `source()`/`related()` behind a `debug_assert!` that no CLI error populates the aux graph — real in tests, absent in the shipped binary. Enforce invariants with data transformation, not assertions.

- **Calling `apply_plan_unchecked()` on a production write path**: Production code that writes back to disk MUST use `apply_fixes_incremental()`. The `_unchecked` suffix makes the bypass explicit at every call site.

- **Adding a ModuleCache "optimization"**: Per-file fresh resolve is intentional. A shared cache would be unsafe because runtime vars are per-call.

- **Calling `sanitize_control_chars` in a `LintDiagnostic` constructor, or on source text passed to `NamedSource`**: Constructors must keep raw bytes so span offsets and fix-edits remain accurate. Source text for miette must use `neutralize_source_for_render` (byte-length-preserving) — `sanitize_control_chars` expands 1–2-byte control chars to 6 bytes, desynchronising every span offset that follows.

- **Using `format!()` to build the canonical JSON**: Always use `serde_json::json!()`.

- **Sorting the file list after processing** in directory mode: `collect_mds_files()` does NOT return a sorted list. Sort with `files.sort()` before the loop (F1 invariant).

- **Indexing source with `source[..offset]` in fix logic**: Panics on non-char-boundary offsets. Use `source.get(..offset)?` (fail-closed → None) as required by ADR-001 (REL-1).

- **Omitting `set_diag_display_path` in directory mode**: Without it, every file in a directory lint run maps its diagnostics under the same basename key in JSON.

- **Setting `fix_removals` on `unused-import`**: Partial-name removal from an import list is structurally ambiguous and unsafe. `unused-import` always leaves `fix_removals: None`.

- **Emitting the result envelope after an early exit** (`lint.rs` output-contract): In the CLI `--fix` path, any early `process::exit` or error return must come AFTER `emit_result` has written the JSON envelope. `PartiallyFixed` arms must write first, then accumulate status lines. Violating this emits zero stdout bytes on `--format json` or emits a stale clean envelope on write failure (both realized in PR #308). `ResultSink` redesign deferred to #309.

## Gotchas

**WASM budget raised three times**: 700K→750K (S2 lint rules), 750K→800K (S4 full surface), 800K→850K (v0.4.0 dogfood remediation). Current guard in `ci.yml`: **850,000 bytes**.

**Two-pass artifact for `\{x\}` remnants**: Fixing a `\{x\}` remnant removes the backslash, leaving bare `{x}`. On the NEXT lint pass, that `{x}` is flagged as a `legacy-interpolation` single-brace expression. Two `--fix` runs are needed to fully migrate.

**Mixed batch and output-equality bypass**: When a fix plan contains any `legacy-interpolation` edit, `all_output_neutral = false` and the output byte-equality sub-check is skipped for the **entire batch**. Assessed P2 risk.

**is_standalone requires BOTH conditions**: A file is standalone only when `!is_partial_or_extends && ctx.imports.is_empty()`. A file with `@extends` but no `@import` is NOT standalone.

**Tier B unused-import cannot fire on standalone files**: A standalone file has `imports.is_empty()` by definition — the rule never fires in the only context where Tier B fixes would be attempted.

**Containment coalescing resolves same-block multi-rule conflicts**: When two rules fire on the same block (e.g. `unreachable-branch` spans the full dead `@if`/`@end`, `empty-block` spans only the inner `@else` body), the containment step keeps only the wider edit.

**Partial-overlap from 10 real rules is structurally impossible via CLI**: All current rules emit spans that are disjoint or containment-related. The `overlap_rejected` path is only reachable with synthetic diagnostics. Regression anchors: `fix.rs::a4_partial_overlap_still_rejected_after_dedup` and `lint.rs::preview_fixes_surfaces_rejected_on_overlap`.

**span.line / span.column are never in lint diagnostic JSON**: All 10 rules pass `None` for `line`/`column`.

**examples/stress-test/errors/ fixtures contain intentional errors**: Running `mds lint examples/` will exit 2 by design.

**shadow-variable is info AND default-off**: Only fires when explicitly configured. `Info` findings never contribute to the exit code.

**Unknown rule NAMES vs unknown severity VALUES behave differently**: An unknown rule name is warned about and then ignored — on every surface, not just the CLI — and the run continues with an unchanged exit code and an unchanged JSON envelope. An unknown severity value fails loudly with a serde deserialization error (exits 2). The asymmetry is deliberate: severities are a closed set, rule names grow every release.

**`.devflow/features/*/KNOWLEDGE.md` is TRACKED, not gitignored**: `.gitignore` ignores `.devflow/*` but re-includes `!.devflow/features/*/KNOWLEDGE.md` (lines 64-70). A doc sweep that excludes `.devflow` wholesale will miss this file, and the source-hygiene gate does scan it.

**D2 mechanical ripple in resolver.rs**: The `..` in the three `ExportDirective` match arms in `resolver.rs` is intentional — it acknowledges the new `offset` field without reading it.

**Frontmatter key span is approximate**: `FmVarFact.approx_offset` is `Option<usize>` — it can be `None` when substring search fails.

**`assertKnownKeys` must be called before backend dispatch**: The validation runs synchronously in the wrapper, before `init()` is awaited or any backend is invoked.

**atomic_write_file temp prefix**: The temp file prefix is `.mds-tmp-`. Both lint and fmt share the same `atomic_write_file` from `output.rs`.

**Python `LintDiagnostic.fix_edits` getter vs `#[pyo3(get)]`**: `Vec<serde_json::Value>` does not implement `IntoPy`. Use the custom `#[getter]` which calls `value_to_py`. Stored internally as `Option<Vec<serde_json::Value>>`.

**Python `LintDiagnostic.to_dict()` always includes `fix_edits`, `help`, and `span` keys**: All three are emitted as Python `None` (JSON `null`) when not set — never absent. This matches `to_canonical_json()` exactly (PF-007 guard). `to_dict()` now conditionally emits `span.line` and `span.column` when present; `LintResult.files[]` parses them from canonical JSON. No built-in rule currently populates them — the only prior test was a negative one that passed identically under the old code. Verify with a positive control (PF-018).

**Reverify rejection message**: Exact stable text: `"could not verify fix — the edited source did not re-parse cleanly ({err}); leaving the file unchanged"`. Test `A5` in `cli_lint.rs` pins this.

**B1 attribution test pattern — use `MdsError::TypeMismatch { src, .. }`, not offsets**: `SerializedError` has no `file` field (PF-012). See `crates/mds-core/tests/virtual_fs.rs` B1 tests.

**serde_yaml_ng rejects raw ESC/DEL in YAML double-quoted keys, but U+0085 NEL passes**: ESC (U+001B) and DEL (U+007F) in YAML double-quoted string keys raise a parser error. U+0085 NEL (`0xC2 0x85`) IS valid YAML `c-printable` and passes through — a reachable ESC-injection vector for `unused-variable` and rules that embed import paths.

**`render_error_sanitized` is private and does no post-processing**: It is just `format!("{report:?}")` on the `SanitizedReport`. Do NOT expect it to sanitize content — sanitize inputs before constructing the `Report`. Use `eprint_error` for CLI output (which wraps in `SanitizedReport` before calling it).

**`MdsError::Display` is explicitly unsanitized**: `e.to_string()` / `eprintln!("{e}")` may emit raw C0/DEL/C1 bytes. Use `e.display_sanitized()` for terminal output or `e.serialize().message` for JSON/binding output. In the CLI, `eprint_error` handles this; downstreams of the published crate should use `display_sanitized()`.

**napi build script is `build:native`, NOT `build`**: Running `npm run build -w @mdscript/mds-napi` silently does nothing useful. Use `npm run build:native -w @mdscript/mds-napi`.

**`packages/mds` prefers the dev WASM artifact**: `packages/mds/src/backend/wasm.ts` resolves to `crates/mds-wasm/pkg/` (the `wasm-pack` dev output) rather than `packages/mds-wasm/dist/node/`. Rebuilding only the `packages/mds-wasm` npm package leaves a STALE backend active, and the cross-surface differential test fails with convincing-looking divergence that isn't a real bug. Always rebuild via `wasm-pack build crates/mds-wasm` when working on WASM output.

**Directory ordering is byte-wise over `/`-normalized paths**: `relative_display` normalizes path separators to `/` via `components().join("/")` before sorting. The fix was declared BREAKING with zero Windows CI executions; it is now covered by a platform-independent ordering test on Ubuntu and a directory case in `packages/mds/__test__/lint.spec.mjs` (runs on windows-latest). The ordering fixture is separator-sensitive by construction (`/` = 0x2F < `[` = 0x5B < `\` = 0x5C) — any separator regression breaks the fixture on Windows.

**`FixOutcome::PartiallyFixed` is silently discarded by `_ => {}`**: `PartiallyFixed` is returned only by `apply_fixes_incremental`. A `_ => {}` wildcard arm compiles clean and discards it without warning. `#[must_use]` does NOT catch this — it fires on a dropped value, not a wildcard arm. Always match `PartiallyFixed` explicitly.

**Clippy stale cache can report a pass on dirty code**: Deleting `LintConfig::from_rules` (PR #308) surfaced an unused `use super::helpers::*;` in `crates/mds-core/src/parser_tests.rs`. A `cargo clippy --workspace --all-targets` run immediately after reported a stale cached pass. Touch the file to force a real re-check before trusting a `clippy` clean result after a deletion.

## Related Follow-ups / Known Limitations

- **#173**: `run_lint_file` FixFileOutcome 3rd-copy duplication and dir-mode JSON per-file wrapper churn.
- **#179**: Entry file read 2–3× — a raw `std::fs::read` call in lint.rs bypasses `NativeFs`.
- **#180**: `LintOptions.basePath` vs `CompileOptions` asymmetry.
- **#202**: Diagnostic ordering — within a file, the order of diagnostics across rules is currently arbitrary.
- **#203**: `unused-import` span anchor — points to the full `@import` directive, not the specific unused name.

## Key Files

- `crates/mds-core/src/lint/mod.rs` — engine entry point; `lint_source()`, `run_rules()`, partial detection
- `crates/mds-core/src/lint/tier.rs` — fix tier table (leaf module); `is_output_neutral(rule)`; `first_occurrence` helper
- `crates/mds-core/src/lint/diagnostic.rs` — `LintDiagnostic`, `LintResult`, `to_canonical_json()` (WIRE: message/help/file key); `sanitize_control_chars` (HUMAN, `Cow`, `#[must_use]`, idempotent); `sanitize_control_chars_wire` (WIRE, new public API, shares one impl via `EscapeMode`); `neutralize_source_for_render` (byte-length-preserving: C0/DEL → `?`, C1+U+061C → NBSP, other 11 hazards → U+FFFD); `named_source_for_render` (new public API, the single `NamedSource` builder); `is_two_byte_format_hazard` / `is_three_byte_format_hazard`
- `crates/mds-core/src/error.rs` — `MdsError`: `serialize()` (WIRE message/help); `display_sanitized()` (HUMAN Display for TTY); raw `Display` documented as unsanitized; `at()` (uses `named_source_for_render` — inherited by all `*_at` constructors)
- `crates/mds-core/src/lib.rs` — `CompileResult::to_canonical_json()` (WIRE warnings, distinct from `LintResult::to_canonical_json`); `emit_warnings()` (HUMAN for prose; identifiers WIRE at construction)
- `crates/mds-core/src/lint/fix.rs` — `plan_fixes_with_options`, `diag_to_edits`, `ByteEdit`, `apply_plan_unchecked`, `dedup_contained_or_identical`, `apply_fixes_incremental`, `FixOutcome`; `reverify_failure_reason()` (sole construction site for `Rejected.reason`, WIRE)
- `crates/mds-core/src/lint/rules/legacy_interpolation.rs` — Tier A token-based rule; atomic single TextEdit per finding; two-pass artifact for backslash-escape fixing
- `crates/mds-core/src/lint/facts.rs` — `collect_facts()`, `AnalysisContext`, `DefineFact { name, offset, end_offset }`
- `crates/mds-core/src/lint/config.rs` — `LintConfig` (lives in mds-core; CLI converts to it)
- `crates/mds-core/src/ast.rs` — `ElseifBranch { offset }`, `IfBlock { else_offset, end_offset }`, `ForBlock/DefineBlock { end_offset }`
- `crates/mds-core/src/lint/rules/` — 10 rule modules + `structural_eq.rs`
- `crates/mds-cli/src/lint.rs` — CLI subcommand; `render_diag_human` (HUMAN for message/help; filename+source via `named_source_for_render`; all status lines via `safe_path`); `set_diag_display_path`, `LintDirCtx`; the rule-name list lives in `mds::KNOWN_LINT_RULES`, not in this crate (#224)
- `crates/mds-cli/src/output.rs` — `atomic_write_file`; `eprint_error` (single CLI stderr choke-point, wraps in `SanitizedReport`); `SanitizedReport` / `SanitizedNode` / `MAX_AUX_DEPTH`; `render_error_sanitized` (private, plain `format!("{report:?}")` on sanitized wrapper); `eprint_warning` (HUMAN, new); `safe_path` / `safe_file_display` / `safe_inline` (all WIRE, new); `preview_text_for` (TTY-gated source neutralization for `--diff`); `render_unified_diff` / `colorize_unified_diff`
- `crates/mds-cli/src/build.rs` — `LintCliConfig` struct, `into_core_config()`, `MdsConfig.lint` field
- `crates/mds-cli/src/watch.rs` — all 11 error prints route through `eprint_error`; lifecycle status lines route through `safe_path` / `safe_inline` / `eprint_warning`
- `crates/mds-cli/tests/print_discipline.rs` — CI-enforced lexical guard; SANITIZERS allowlist; `ALLOWED_UNSANITIZED` allowlist; `every_allowlist_entry_is_live` rot check
- `crates/mds-cli/tests/security.rs` — T-ESC-MSG-1/2, T-ESC-RULE-1, T-ESC-FNAME-1/2, T-ESC-WALK-1
- `crates/mds-wasm/src/lib.rs` — `lint()` and `lintVirtual()` exports; `parse_check_options` strict allow-list
- `crates/mds-napi/src/lib.rs` — `lint`, `lintFile`, `lintVirtual`; `extract_rules_direct`; `parse_lint_file_opts`
- `crates/mds-python/src/lib.rs` — `LintDiagnostic` frozen pyclass; `LintResult::new()` calls `sanitize_lint_value()` (WIRE, construction-time — closes PF-004 parallel-path gap)
- `packages/mds/src/types.ts` — `LintDiagnostic.fix_edits`; `CheckOptions { vars? }`
- `packages/mds/src/util/options.ts` — `assertKnownKeys` (strict unknown-option rejection)
- `crates/mds-cli/tests/cli_lint.rs` — A5 (reverify rejection message prefix), L-CLI-RESOURCE (exit-3), L-CLI-DIR2 (file-order determinism); T-5..T-9 ESC-injection anchors

## Related

- **ADR-007** (sanitizer escape-map contract): `sanitize_control_chars` / `sanitize_control_chars_wire` share one implementation via `EscapeMode`; forking a second escape table is the explicit anti-pattern this prevents.
- **PF-004** (parallel-path enforcement gaps): `SanitizedReport` wraps at the `Report` level to cover both `MdsError` and CLI `miette!()` error families unconditionally. Python `LintResult::new()` calls `sanitize_lint_value()` construction-time. `print_discipline.rs` enforces the per-field rule mechanically.
- **PF-005** (debug_assert-only invariants absent in release): `SanitizedReport` materialises the auxiliary graph at construction instead of returning `None` behind a `debug_assert!` — the earlier revision's guarantee held in tests and was absent in the shipped binary.
- **PF-007** (cross-surface goldens can't catch divergence): `fix_edits` is emitted unconditionally (null when None) across all surfaces; differential tests cover cross-surface parity.
- **PF-013** (vacuous negative security tests): Every ESC-injection test now pairs a NEGATIVE assertion (raw byte absent) with a POSITIVE one (escaped form present) and a non-vacuity guard (diagnostics non-empty, expected rule matched). T-9 was rewritten from a vacuous YAML-rejection vector to a reachable duplicate-import + U+0085 NEL vector.
- **PF-014** (sanitize inputs, not rendered artifacts): The `SanitizedReport` design — pre-sanitize message/help/labels before miette renders — is the PF-014-correct boundary. Post-processing the rendered frame corrupts miette's own ANSI SGR codes; CI uses `NO_COLOR=1` and pipes stderr so the failure would stay green. T-ESC-6 pins this on the colour path.
- **ADR-001** (span-guided rewrite + compile-equivalence gate): All `--fix` edits are span-guided byte rewrites. `TextEdit` ranges are validated fail-closed. `apply_plan_unchecked` is explicitly named to make ADR-004 reverify-gate bypass visible.
- **ADR-004** (three-tier --fix safety model, reverify gate): `apply_fixes_incremental`'s batch-first strategy with bounded per-edit fallback is the AC-F-20 implementation.
- **ADR-002** (v0.4.0 whitespace contract, interior-verbatim): The `empty-block` rule's "whitespace-only-Text body" definition is directly downstream of this contract.
- **ADR-003** (@extends FM emission): The `unused-variable` rule is suppressed on `@extends` children.
- **PF-012** (span source-identity): For test attribution (B1 tests), `SerializedError` has no `file` field — use `MdsError::TypeMismatch { src, .. }` pattern-match.
- `crates/mds-core/tests/api_surface.rs` — pins the public lint API signatures.
- `.devflow/features/mds-fmt/KNOWLEDGE.md` — `mds fmt` knowledge base; `atomic_write_file` is shared between both subcommands via `output.rs`.
- `.devflow/features/source-map-security/KNOWLEDGE.md` — source map path-containment choke-point.

## v0.5.0 Removal Tracker: apply_fixes

`mds::fix::apply_fixes` is deprecated as of v0.4.0. The six ADR-004 reverify-gate
tests that must be ported or retired before removal are enumerated below by name —
**test names are the durable key; line numbers drift as `fix.rs` evolves**. GitHub
issue #304 carries behavioral context (which ADR-004 behavior each test pins); its
line numbers predate the `#[expect(...)]` insertions made by PR #303 (issue #209) and
have since drifted.

Six tests to port or retire (use `grep -n 'fn <name>' crates/mds-core/src/lint/fix.rs`
to locate current lines — test names are the durable key):

- `a4_partial_overlap_still_rejected_after_dedup`
- `l_fix_rev1_a5_rejection_message_pins_stable_prefix_and_suffix`
- `reverify_preexisting_untargeted_survives_and_fix_applies`
- `reverify_new_untargeted_diagnostic_is_rejected`
- `tier_b_unused_function_standalone_apply_succeeds`
- `l_fix_rev1_output_delta_causes_rejection`
