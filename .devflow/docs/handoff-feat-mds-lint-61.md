# S2 Handoff: feat/mds-lint-61

Phase S2 of 4 — 9-rule lint engine + tiered --fix planner.
(Supersedes S1 handoff — all S1 content still valid; this file extends it.)

## Branch

`feat/mds-lint-61` (based on `main` @ 3ce9f1d)

## Commits (S1 + S2)

| SHA | Message |
|-----|---------|
| `76616ea` | `feat(core): add offset to ExportDirective variants (D2, #61)` |
| `05bee39` | `feat(core): lint engine scaffolding — types, config, canonical JSON, limits (#61)` |
| `5cf36b4` | `feat(wasm): lint stub export + size baseline (#61)` |
| `4304d15` | `feat(core): 9-rule lint engine — 5 local-AST + 4 semantic rules (#61)` |
| `462b03d` | `feat(core): tiered --fix planner with overlap rejection and reverify gate (#61)` |
| `9a65849` | `chore(ci): raise wasm size budget for lint engine (#61)` |

## Files Created (S2)

| File | Purpose |
|------|---------|
| `crates/mds-core/src/lint/rules/mod.rs` | Declares all 9 rule modules + structural_eq |
| `crates/mds-core/src/lint/rules/structural_eq.rs` | Manual AST structural equality (no PartialEq due to f64) |
| `crates/mds-core/src/lint/rules/empty_block.rs` | empty-block rule (Warn, Tier A) |
| `crates/mds-core/src/lint/rules/redundant_else.rs` | redundant-else rule (Warn, Tier C) |
| `crates/mds-core/src/lint/rules/unreachable_branch.rs` | unreachable-branch rule (Error, Tier A) |
| `crates/mds-core/src/lint/rules/duplicate_import.rs` | duplicate-import rule (Error, Tier A) + normalize_import_path |
| `crates/mds-core/src/lint/rules/duplicate_export.rs` | duplicate-export rule (Error, Tier A) |
| `crates/mds-core/src/lint/rules/unused_variable.rs` | unused-variable rule (Warn, Tier C) |
| `crates/mds-core/src/lint/rules/unused_import.rs` | unused-import rule (Warn, Tier B) |
| `crates/mds-core/src/lint/rules/unused_function.rs` | unused-function rule (Warn, Tier B) |
| `crates/mds-core/src/lint/rules/shadow_variable.rs` | shadow-variable rule (Info, Tier C, DEFAULT-OFF) |
| `crates/mds-core/src/lint/fix.rs` | Tiered --fix planner (pure, no I/O) |

## Files Modified (S2)

| File | Change |
|------|--------|
| `crates/mds-core/src/lint/facts.rs` | Full rewrite: complete AnalysisContext with all fact types, shadow walk, frontmatter YAML parsing |
| `crates/mds-core/src/lint/mod.rs` | run_rules() wired to all 9 rules; `pub mod fix` added |
| `crates/mds-core/src/lint/diagnostic.rs` | LintResultBuilder visibility `pub(super)` → `pub(crate)`; removed `#[allow(dead_code)]` |
| `crates/mds-core/src/lib.rs` | `pub use lint::{fix, ...}` — fix module re-exported |
| `crates/mds-wasm/src/lib.rs` | rustfmt reformatting only (no logic change) |

## Public API Added (S3 integration points)

### Fix planner (mds::fix)

```rust
// Tier classification
pub enum FixTier { A, B, C }
pub fn rule_tier(rule: &str) -> FixTier
pub fn is_fixable(rule: &str, is_standalone: bool) -> bool

// Planning
pub struct ByteEdit { pub start: usize, pub end: usize, pub rule: String }
pub struct FixPlan { pub edits: Vec<ByteEdit>, pub overlap_rejected: bool, pub truncated: bool }
pub fn plan_fixes(lint_result: &LintResult, source: &str) -> FixPlan
pub fn plan_fixes_with_options(lint_result: &LintResult, source: &str, include_tier_b: bool) -> FixPlan

// Application
pub enum FixOutcome { Fixed { source: String, residual: LintResult }, Rejected { source: String, reason: String }, NothingToFix }
pub fn apply_plan(source: &str, plan: &FixPlan) -> String
pub fn apply_fixes<F: FnOnce(&str) -> Result<LintResult, MdsError>>(source: &str, plan: FixPlan, reverify: F) -> FixOutcome

// Utilities
pub fn extend_to_line_end(source: &str, pos: usize) -> usize   // CRLF-safe
pub fn fixable_diagnostics(result: &LintResult, is_standalone: bool) -> Vec<&LintDiagnostic>
```

Accessed as `mds::fix::plan_fixes(...)` etc. (re-exported from crate root).

### AnalysisContext facts (for S3 diagnostic display / future rules)

```rust
pub struct AnalysisContext {
    pub has_explicit_exports: bool,
    pub is_partial_or_extends: bool,
    pub imports: Vec<ImportFact>,         // alias/selective/merge
    pub exports: Vec<ExportFact>,         // named/reexport/wildcard, with offsets
    pub defines: Vec<DefineFact>,         // @define blocks, with offset
    pub frontmatter_vars: Vec<FmVarFact>, // FM keys (excl. reserved), with approx offset
    pub used_vars: HashSet<String>,       // all Expr::Var / Arg::Var references
    pub used_calls: HashSet<String>,      // all Expr::Call / Arg::Call references
    pub used_namespaces: HashSet<String>, // all QualifiedCall namespaces
    pub used_include_aliases: HashSet<String>, // all @include alias references
    pub shadow_pairs: Vec<ShadowPair>,    // inner-over-outer shadows
}
```

## S3 Task: CLI Integration

S3 must implement `mds lint` and `mds lint --fix` subcommands in `crates/mds-cli/`.

### Key integration points

1. **`mds lint <file>`** — calls `mds::lint(Path, vars, &LintConfig)`, then renders
   diagnostics via `miette::Report::from(diag)` to stderr.

2. **`mds lint --fix <file>`** (pure-core fix):
   ```rust
   let source = fs::read_to_string(&path)?;
   let lint_result = mds::lint(&path, vars, config)?;
   let plan = mds::fix::plan_fixes_with_options(&lint_result, &source, is_standalone);
   let outcome = mds::fix::apply_fixes(&source, plan, |fixed| {
       mds::lint_str_with(fixed, base_dir, vars.clone(), config)
   });
   match outcome {
       FixOutcome::Fixed { source, .. } => atomic_write(&path, &source)?,
       FixOutcome::Rejected { reason, .. } => eprintln!("Fix rejected: {reason}"),
       FixOutcome::NothingToFix => {},
   }
   ```

3. **`--rules` / `--config` flag**: parse `mds.json` or inline `--rules key=value`
   into `LintConfig { rules: HashMap<String, Severity> }`.

4. **Exit codes** (per spec):
   - 0 = no diagnostics
   - 1 = warnings only
   - 2 = at least one error

5. **JSON output** (`--format json`): `result.to_canonical_json()` produces the
   canonical shape; set `fixable` field per `mds::fix::is_fixable(rule, standalone)`.

6. **`sanitize_control_chars`**: apply to human-rendered message/help strings at
   the CLI render boundary (NOT in JSON output).

7. **is_standalone detection**: a file can be linted standalone if it has no
   `@import` or `@extends` directives (check via `mds::check_str`). For standalone
   files, Tier B fixes are applied.

### WASM `lint()` rules config (S3 target)

The current WASM lint() passes `LintConfig::default()`. S3 should extend:

```js
// Target API:
lint(source, { rules: { 'unused-variable': 'error', 'shadow-variable': 'off' } })
```

Implemented by extending `parse_options` in `crates/mds-wasm/src/lib.rs` to extract
`options.rules` into `HashMap<String, Severity>` and pass as `LintConfig { rules }`.

### Python bindings (mds-python)

For parity with NAPI/WASM, expose `lint_str()` and `lint_str_with()` in
`crates/mds-python/src/lib.rs`. The canonical JSON output is the binding contract
(not the Rust types). Return the JSON string from Python; deserialization is the
caller's job. Tier: medium priority (defer --fix from Python if needed for time).

## Quality Gates Status

```
cargo test --workspace          → 813 mds-core tests PASS (0 failed)
cargo fmt --all --check         → CLEAN
cargo clippy --workspace --all-targets -- -D warnings  → CLEAN (0 warnings, 0 errors)
```

## Test Counts (S1 + S2)

- Workspace before S1: ~593 tests
- After S1: ~713 lib + 57 integration + 33 doctests
- After S2: 813 mds-core lib tests (net +100 new tests from 9 rules + fix planner)

## WASM Size Baseline (S2, measured 2026-07-11)

Measured locally using wasm-pack 0.15.0 with its bundled wasm-opt (`-Oz`):

| Target | Raw bytes | Gzipped |
|--------|-----------|---------|
| Node (`dist/node/mds_wasm_bg.wasm`) | **712,419** | 287,224 |
| Web (`dist/web/mds_wasm_bg.wasm`) | **712,419** | 287,224 |

**Decision: RAISE** — 712,419 bytes exceeds old 700,000 guard. Guard bumped to
750,000 in `9a65849` (`chore(ci): raise wasm size budget for lint engine (#61)`).
The +50K increment follows the prior budget-history pattern and provides buffer
for CI toolchain variation.

The S1 baseline (before S2 lint rules) was NOT separately measured (no `git stash`
approach practical mid-stream; the pre-S2 files in `pkg/` dated Jun 26, 592,494 bytes,
predating all lint work). The S1 WASM stub commit (`5cf36b4`) added only a thin
`lint()` shim — the ~120KB delta above the Jun 26 measurement accounts for both S1
and S2 additions (AnalysisContext, 9-rule dispatch, fix planner).

## Key Invariants / Gotchas

1. **`fix.rs` is pure (no I/O)**: File read and atomic write are S3's responsibility.
   `apply_fixes()` returns a `FixOutcome` with the fixed source bytes in memory.

2. **Tier B gate**: `plan_fixes()` does NOT include Tier B edits by default.
   Call `plan_fixes_with_options(result, source, true)` for standalone files.

3. **Reverify is mandatory**: Never call `apply_plan()` directly in production code
   (only in tests). Always use `apply_fixes()` which includes the reverify gate.

4. **CRLF (AC-F-24)**: `extend_to_line_end()` always consumes `\r\n` as a unit.
   The planner correctly handles Windows CRLF, macOS CR, and Unix LF line endings.

5. **shadow-variable default-off**: `resolve_severity()` returns `Severity::Off` as
   built-in default. The rule only fires when explicitly enabled in `mds.json`.

6. **Partial/@extends suppression**: unused-variable, unused-import, unused-function
   are all suppressed when `ctx.is_partial_or_extends` is true. shadow-variable is
   NOT suppressed (it's already default-off, and shadowing in partials is valid).

7. **normalize_import_path** (D1): textual interior segment-collapse used for
   duplicate-import path comparison. Located in `rules/duplicate_import.rs`.

8. **D2 offsets**: `ExportFact.offset` uses the `offset: usize` field added to all
   ExportDirective variants in S1. Used by duplicate-export for span placement.

9. **LintResultBuilder cap**: `MAX_DIAGNOSTICS = 1000`. When hit, `truncated = true`
   and `plan.truncated = true` (AC-F-25 idempotence caveat applies).

10. **`fixable` in canonical JSON**: Currently hardcoded `false` in
    `to_canonical_json()`. S3 should update this to use `mds::fix::is_fixable(rule,
    standalone)` to populate the `fixable` field correctly.

## Deviations from Plan

1. **Steps 4+5 combined into one commit**: The 9 rules all depend on
   `AnalysisContext` being fully populated (steps 4 and 5 share `facts.rs`), and
   `lint/mod.rs` calls all 9 rules together. Splitting at commit boundary would have
   required temporary scaffold. Combined into one workspace-green commit.

2. **`DefineFact.params` removed**: The `params` field was collected but never read
   by any rule. Shadow detection already handled via `shadow_pairs` collected during
   the facts walk. Removed to keep zero-dead-code policy.

3. **WASM size guard raised**: S2 lint engine pushed optimized binary to 712,419
   bytes (712K); guard raised 700K→750K in commit `9a65849`. S3 additions (CLI
   code is native, not WASM) should not affect WASM size. WASM binding changes
   for `options.rules` parsing should be minimal.
