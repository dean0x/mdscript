# chore(mds-core)!: deprecate `apply_fixes` in favor of `apply_fixes_incremental` [#209]

Issues: #209

## Implementation Plan

# PR5 — Deprecate `apply_fixes` (#209) — CHALLENGED AND AMENDED

**Size:** XS as scoped, S/M if OD-209-A or OD-209-E resolve toward doing the real work now.
**Wave position:** independent. Touches `crates/mds-core/src/lint/fix.rs`, `crates/mds-core/tests/api_surface.rs`, `CHANGELOG.md`, `.devflow/features/mds-lint/KNOWLEDGE.md`. Only the `CHANGELOG.md` `[Unreleased]` block is plausibly shared with the other five PRs.

> **STOP — do not implement until OD-209-A is answered.** The issue's stated blocker is falsified by the tree (see §0.1). Everything below assumes OD-209-A resolves to option C (deprecate). If it resolves to A, B, or D, §3-§5 change materially.

---

## 0. State verified against 113f472

Every anchor in the original plan was re-checked against the working tree. **All of them are accurate** — including the ten test fn-decl/call-site pairs, which matched exactly. The original plan's correction of the issue's stale `fileRefs` (407-465 → 663-762) is also correct.

| Claim | Verified | Evidence |
|---|---|---|
| `apply_fixes` defined at fix.rs:691 | YES | `pub fn apply_fixes<F>(source: &str, plan: FixPlan, original: &LintResult, reverify: F) -> FixOutcome`; rustdoc 663-689; `#[must_use = "a dropped FixOutcome silently discards the fix result"]` at 690 |
| Zero production callers | YES | Only call sites are inside fix.rs's own `#[cfg(test)] mod tests` (mod starts 1007). mds-cli/src/lint.rs uses `apply_fixes_incremental` at 451 and 571. Zero references in mds-napi, mds-wasm, mds-python, packages/*, README.md, or any crate README |
| Public via `mds::fix` | YES | lint/mod.rs:31 `pub mod fix;`; lib.rs:63-67 `pub use lint::{fix, ...}` |
| Issue `fileRefs` 407-465 is stale | YES | That range is `plan_fixes_with_options` territory. Real anchor 663-762 |
| Workspace version is 0.3.0, not 0.4.0 | YES | Cargo.toml:6. `bump-version.mjs` rewrites manifests + package.json + CHANGELOG only, never a `.rs` file |
| MSRV supports `#[expect]` | YES | Cargo.toml:8 `rust-version = "1.88"`; ci.yml MSRV job pinned to `dtolnay/rust-toolchain@1.88`; `#[expect]` stable since 1.81 |
| No workspace/crate lint table | YES | No `[workspace.lints]`, no `[lints]` in mds-core, no clippy.toml, no crate-level `#![deny]`/`#![warn]` |
| KNOWLEDGE.md is git-tracked | YES | `git ls-files` returns it; `.gitignore:64-70` is the un-ignore block. **Checked from the repo root working tree, not a worktree (PF-016).** |
| No `AD-` convention exists yet | YES | `grep -rn 'AD-[0-9]' --include='*.rs' crates/` returns nothing |
| No `#[expect]` or `#[deprecated]` exists yet | YES | Both greps return nothing across `crates/`. This PR introduces both |
| CHANGELOG anchors | YES | `[Unreleased]` 8, Security 89, Added 427, Changed 627, Fixed 764 |
| api_surface.rs F-API-1 | YES | 1374-1418; `use mds::fix::{...}` inside the fn body at 1382; next test at 1420 |
| WASM guard | YES | ci.yml:87-118 loops over `pkg/mds_wasm_bg.wasm` and `pkg-web/mds_wasm_bg.wasm`, emits `::notice::`, fails at `raw > 850000` |

### 0.1 NEW — the issue's premise does not hold

`git show v0.3.0:crates/mds-core/src/lint/fix.rs` → **does not exist in v0.3.0**. The file was added by 5a227dc (mds lint #61, PR #171). `git tag --list 'v*'` → newest tag is `v0.3.0`. The v0.4.0 tag is not cut.

**`mds::fix::apply_fixes` has never been published to crates.io.** The issue asserts it 'cannot be deleted without a semver-breaking change since it is a public mds-core export.' That is false at this commit: deletion or `pub(crate)` demotion costs nothing, because no downstream consumer exists or can exist. ADR-010 recorded the counter-argument verbatim for exactly this window: *'the pre-publish window is the last moment the break is free and every unmarked public type is a permanent semver trap.'* → **OD-209-A.**

### 0.2 NEW — the v0.5.0 coverage cliff

Six ID-tagged regression guards assert only through `apply_fixes` and have no `apply_fixes_incremental` counterpart:

| fix.rs line | Test | Behavior it is the sole pin for |
|---|---|---|
| 1392 | `a4_partial_overlap_still_rejected_after_dedup` | A4: partial overlap survives dedup and still rejects |
| 1681 | `l_fix_rev1_a5_rejection_message_pins_stable_prefix_and_suffix` | A5 rejection-message contract |
| 1869 | `reverify_preexisting_untargeted_survives_and_fix_applies` | **AC-F-23**: pre-existing untargeted diagnostic must not refuse the fix |
| 1890 | `reverify_new_untargeted_diagnostic_is_rejected` | A genuinely new untargeted diagnostic IS a regression |
| 1957 | `tier_b_unused_function_standalone_apply_succeeds` | **I-13**: end-to-end Tier B with a real reverify closure |
| 2024 | `l_fix_rev1_output_delta_causes_rejection` | **L-FIX-REV1**: output delta must reject |

The incremental suite is INC-1..INC-8 (2075, 2094, 2113, 2150, 2204, 2236, 2278, 2404), `pf005_unsorted_edits_rejected_in_incremental` (2336), `incremental_rejection_reason_escapes_embedded_error_display` (1767). It covers none of the six.

Two consequences. First, deleting `apply_fixes` at v0.5.0 silently deletes the only coverage of five ADR-004 reverify-gate behaviors. Second — and this is true today, before any deprecation — those safety behaviors are pinned against a function the shipped CLI never calls. → **OD-209-E.**

### 0.3 NEW — ADR mis-citation inside the block being edited

fix.rs:585 reads ``# `_unchecked` suffix — ADR-001`` and 586-588 say the function 'bypasses the ADR-001 reverify gate (compile-equivalence check)'. Per the ledger, ADR-001 is the **mds fmt** gate; ADR-004 states that gate is 'inapplicable BY CONSTRUCTION' to lint --fix. Rewriting 589-591 while leaving 585-588 ships one paragraph citing two mutually exclusive ADRs. Same defect at KNOWLEDGE.md:180. → **OD-209-D.**

---

## 1. Approach

Six moves, strictly ordered:

1. Attach `#[deprecated]` to `apply_fixes`; write the migration semantics into the rustdoc as an `AD-209-1` record.
2. Run the **positive control** (clippy must report exactly 10 deprecation errors) before touching a single suppression.
3. Add `#[expect(deprecated)]` per test function — never at module scope, never `#[allow]`.
4. Correct the misdirecting rustdoc at 238, 590, 599, **and the ADR-001 mis-citation at 585-588**.
5. CHANGELOG `### Deprecated`; KNOWLEDGE.md:462.
6. Add `F-API-3` to `tests/api_surface.rs`, then run the **reverse mutation control** (delete the attribute, confirm 11 `unfulfilled_lint_expectations`, restore).

No new module, no wrapper, no forwarder. The body (695-762) is byte-identical. Codegen delta is exactly zero.

---

## 2. Affected files and anchors

### `crates/mds-core/src/lint/fix.rs` (2488 lines)

| Anchor | Current | Change |
|---|---|---|
| 663-689 | rustdoc for `apply_fixes` | Prepend `# Deprecated (AD-209-1)` (see §3 D2). **No compiled doctest** — ```text or ```ignore fences only |
| 690 | `#[must_use = "..."]` | Unchanged; insert `#[deprecated(...)]` after it, directly above 691 |
| 691-762 | signature and body | **Untouched** |
| 236-240 | `FixPlan` docs | Rewrite 238 to name `apply_fixes_incremental` only. **Preserve 239-240 verbatim** (the `FixPlan::default()` / ADR-010 sentence) |
| 585-588 | ``# `_unchecked` suffix — ADR-001`` | **NEW** — correct to ADR-004 (§0.3) |
| 589-591 | 'must use [`apply_fixes`] instead' | Rewrite to `apply_fixes_incremental`. Load-bearing safety guidance |
| 598-600 | 'use [`apply_fixes`] which checks this' | Rewrite to `apply_fixes_incremental` |
| 620-626 | comment naming both functions | **Leave** — factually true of both |
| 800-802 | `Unlike [`apply_fixes`] which requires `F: FnOnce`` | **Leave** — this is the migration caveat readers need |
| 1006-1008 | `#[cfg(test)]` / `mod tests {` / `use super::*;` | **No module-level suppression.** Do not convert the glob to explicit imports (a glob of a deprecated item does not fire the lint; an explicit `use` would) |

**Ten `#[expect(deprecated)]` insertion points** (fn-decl line → call line, all verified):

| # | Function | fn | call |
|---|---|---|---|
| 1 | `a4_partial_overlap_still_rejected_after_dedup` | 1392 | 1448 |
| 2 | `l_fix_rev1_reverify_failure_rejects_fix` | 1654 | 1661 |
| 3 | `l_fix_rev1_a5_rejection_message_pins_stable_prefix_and_suffix` | 1681 | 1687 |
| 4 | `apply_fixes_rejection_reason_escapes_embedded_error_display` | 1739 | 1749 |
| 5 | `reverify_success_returns_fixed` | 1833 | 1850 |
| 6 | `reverify_preexisting_untargeted_survives_and_fix_applies` | 1869 | 1878 |
| 7 | `reverify_new_untargeted_diagnostic_is_rejected` | 1890 | 1898 |
| 8 | `tier_b_unused_function_standalone_apply_succeeds` | 1957 | 1995 |
| 9 | `l_fix_rev1_output_delta_causes_rejection` | 2024 | 2050 |
| 10 | `pf005_unsorted_edits_rejected_in_apply_fixes` | 2371 | 2392 |

All ten bind the result (`let outcome = apply_fixes(...)`), so `unused_must_use` will not fire and the 'exactly 10' control is not polluted.

### `crates/mds-core/tests/api_surface.rs`
Insert `F-API-3` after 1418, before the `STRING_SOURCE_MAP_LABEL` test at 1420. Mirror `fix_api_incremental_exists` (1381-1418): `use mds::fix::{...}` **inside** the fn body so the `#[expect(deprecated)]` covers both import and call; construct via `LintResult::new` / `plan_fixes` / `ByteEdit::deletion`, never struct literals (applies ADR-010).

### `CHANGELOG.md`
Insert `### Deprecated` immediately before the existing `### Fixed` at 764, matching the file's own Added(427) → Changed(627) → Fixed(764) run. **Do not justify this by Keep a Changelog ordering** — `[Unreleased]` already places `### Security` at 89, ahead of Added, so the file does not follow it.

### `.devflow/features/mds-lint/KNOWLEDGE.md` (tracked)
- **462** — the only line that changes: drop `apply_fixes()` from the 'MUST use' anti-pattern bullet.
- **Do not touch:** line 4 (frontmatter keyword blob), 180 (unless OD-209-D says so), 188 (factual FixOutcome statement), 543 (key-files list), 571 (ADR-004 linkage), or `.devflow/features/index.md:4`. None name `apply_fixes` in a way that misdirects.

---

## 3. Design decisions

**D1 — `since` handling is now an open question (OD-209-B), not settled.** Hardcoding `"0.4.0"` compiles clean today (rustc's `deprecated_semver` only checks parseability for third-party crates) but leaves a drift risk that `bump-version.mjs` provably cannot fix. Omitting `since` eliminates the risk at zero cost. Do not implement until answered.

**D2 — The `note` carries the blocking caveat; the rustdoc carries the full record. `note` MUST be ≤ 160 characters.** rustc renders `note` verbatim, inline, in every downstream warning; a 230-character note wraps badly in terminals and CI logs. Three verified migration deltas belong in the `AD-209-1` rustdoc section:
1. **Closure bound:** `apply_fixes` takes `F: FnOnce` (693); `apply_fixes_incremental` requires `F: Fn` (811). A move-once closure cannot migrate mechanically. Already documented at 800-802.
2. **New reachable outcome:** `PartiallyFixed` (docs 794-799, construction 966+) is returned only by the incremental path. `FixOutcome` is `#[non_exhaustive]` (265, applies ADR-010), so external matches already carry a wildcard — and a wildcard that swallows `PartiallyFixed` drops partial results on the floor.
3. **Cost:** 1 reverify call vs. up to N+1, capped by `FALLBACK_MAX_EDITS = 50` (774).

Shape (note trimmed, full record in the rustdoc above it):

```rust
#[must_use = "a dropped FixOutcome silently discards the fix result"]
#[deprecated(
    note = "use `apply_fixes_incremental`; not a drop-in swap, the reverify closure must be `Fn`, not `FnOnce`. See the item docs."
)]
pub fn apply_fixes<F>(...)
```

**No placeholder token may reach a commit.** OD-209-A/B/C must resolve first.

**D3 — `#[expect(deprecated)]` per test fn, never `#[allow]`, never at module scope.** `#[expect]` suppresses the diagnostic AND fires `unfulfilled_lint_expectations` if the diagnostic stops being produced, so the suppression doubles as a live assertion. **Amendment:** state plainly that this assertion has exactly ONE enforcement point — `cargo clippy --workspace --all-targets -- -D warnings` (ci.yml:38). `cargo test --workspace`, `cargo nextest run`, and the MSRV job (`cargo check` with no `--all-targets`, so tests are never compiled) all let it pass as a mere warning. Module scope is wrong regardless: a `#![allow]` at 1007 would blanket ~1480 lines and mask future accidental use of any other deprecated item.

**D4 — Do NOT also deprecate `apply_plan_unchecked`.** Verified: its only reference outside fix.rs is a *comment* at mds-cli/src/lint.rs:845; its three live callers are fix.rs:720, 846, 895. Deprecating it would force suppressions onto three production call sites inside `apply_fixes_incremental` itself, violating #209's own AC. It is a live internal primitive with a deliberately scary name and an unconditional PF-005 sortedness assert (627-630). Leave it public and undeprecated; only correct its rustdoc (585-600, per §0.3).

**D5 — AD-series traceability.** `AD-209-1` on `apply_fixes` (why deprecated, `applies ADR-004`, the three deltas, why the body was not deleted **given fix.rs is absent at tag v0.3.0**), `AD-209-2` on F-API-3 (why `#[expect]` over `trybuild`). No leading `#` (avoids PF-010). Whether to establish this repo-wide convention here is OD-209-F.

**D6 — ADR-004 linkage.** `apply_fixes` implements the ADR-004 gate as an all-or-nothing batch verify; `apply_fixes_incremental` implements the same safety contract with a batch attempt plus a bounded per-edit fallback, salvaging the safe subset rather than refusing wholesale. That is *why* the deprecation is correct rather than arbitrary, and it belongs in code.

**D7 — No new abstraction.** A `pub(crate) fn apply_fixes_impl` with a deprecated forwarder is rejected: the tests would stop exercising the deprecated public path, which is the only path an external user can reach.

**D8 (NEW) — No compiled doctest, and this PR must ADD no new `allow(deprecated)` under `crates/*/src/`.** `cargo clippy --all-targets` does not compile doctests, so a doctest calling `apply_fixes` would emit a permanently ungated warning in every downstream `cargo test`. And because the AC-209-04 audit is a lexical grep over `*.rs`, an `#[allow(deprecated)]` written inside a doc-comment code fence in `src/` would trip it. Migration examples use ```text or ```ignore. **This is an "add none" rule, not an absolute-absence rule (avoids PF-015):** `crates/mds-core/src/lint/config.rs` lines 287 and 289 already carry `/// #[allow(deprecated)]` inside the compiled doctest for the earlier `LintConfig::from_rules` deprecation, where they are load-bearing — removing them would make that doctest emit a deprecation warning. They are pre-existing and whitelisted by AC-209-04 group (c). Do not delete them in service of this design decision.

---

## 4. Implementation sequence

0. **Resolve OD-209-A.** If it lands on A, B, or D, discard §3 D1-D3 and re-plan; this sequence assumes C.
1. **Resolve OD-209-B and OD-209-C** so no placeholder is ever committed.
2. **Attach `#[deprecated]` + write the `AD-209-1` rustdoc.** Run `cargo clippy -p mds-core --all-targets -- -D warnings` and capture the failure list. **Positive control (applies ADR-009, avoids PF-013): expect exactly 10 errors at 1448, 1661, 1687, 1749, 1850, 1878, 1898, 1995, 2050, 2392.** Fewer than 10 is a failure signal, not success — stop and diagnose.
3. **Add `#[expect(deprecated)]` to each of the ten fn-decl lines**, each with a one-line rationale comment. Re-run → clean.
4. **Correct rustdoc at 585-588, 589-591, 598-600, and 238** (preserving 239-240 verbatim). Leave 620-626 and 800-802.
5. **CHANGELOG `### Deprecated`** after `### Added` and before the first `### **BREAKING**` section (deliberately positioned in the visible upper portion of `[Unreleased]`; see AC-209-13). Public-facing copy: no em/en dashes, no placeholders.
6. **`tests/api_surface.rs` F-API-3** after 1418, with the `AD-209-2` docstring.
7. **Reverse mutation control (NEW, mandatory):** delete only the `#[deprecated(...)]` attribute, run `cargo clippy --workspace --all-targets -- -D warnings`, then also run `cargo clippy -p mds-core --test api_surface -- -D warnings`; the UNION of the two runs must be **exactly 11** `unfulfilled_lint_expectations` — 10 in fix.rs and 1 in api_surface.rs (applies ADR-009) — then restore and confirm both runs clean. How the 11 split across the two commands is build-scheduling dependent, not a property of the code: removing the attribute leaves the lib rlib compiling clean, so the lib-test and `api_surface` targets become ready simultaneously. In the run verified for this PR the workspace command alone reported all 11; a serialized scheduler may instead abort after the lib-test target, in which case the targeted command supplies the 11th. Assert the union, never a per-command count. Without this, AC-209-05 is an absence-only claim.
8. **`.devflow/features/mds-lint/KNOWLEDGE.md:462`**, confirmed tracked from the **repo root working tree** (avoids PF-016).
9. **OD-209-E work**, if it resolved to 'migrate now'.
10. **Full gate (§6).**

---

## 5. Risks

| ID | Risk | Likelihood | Mitigation |
|---|---|---|---|
| R1 | `-D warnings` breaks on 10 in-crate uses the moment the attribute lands | Certain | By design; steps 2-3. Sites pre-enumerated with exact lines |
| R2 | `since = "0.4.0"` drifts; `bump-version.mjs` provably cannot fix it | Low but silent | Escalated to **OD-209-B**. 'Omit `since`' eliminates it; a runbook grep only defers it to a human |
| R3 | A future PR deletes an `apply_fixes` call but leaves the `#[expect]` → build error | Low | Intended feedback loop. Note it in the PR body so a later reviewer is not confused |
| R4 | CHANGELOG `[Unreleased]` conflicts with the other five wave PRs | Medium | PR5 is the only wave PR creating `### Deprecated`, and it inserts at a section boundary. Land early in the squash order |
| R5 | WASM size guard (820,305 / 850,000; 3.5% headroom) | Effectively zero — **but verify, do not assume** | Attributes and doc comments emit no codegen; the body is byte-identical. **Assert delta == 0 bytes on BOTH `pkg/mds_wasm_bg.wasm` and `pkg-web/mds_wasm_bg.wasm` against the wave base, not merely '≤ 850,000'** (ci.yml:87-118 checks both). A budget-only pass would absorb an unrelated regression |
| R6 | Someone converts `use super::*;` (1008) to explicit imports later; an explicit `use` fires the lint where a glob does not | Low | Called out in §2. Documented, not defended against |
| R7 | Deprecating the function that `apply_plan_unchecked`'s safety doc names as the required alternative | Certain if step 4 is skipped | Step 4 is not optional. Now also covers the ADR-001 mis-citation at 585-588 |
| **R8 (NEW)** | A rewritten intra-doc link silently degrades to literal text — **CI has no `cargo doc` step and no `RUSTDOCFLAGS`** | Medium | `RUSTDOCFLAGS="-D warnings" cargo doc -p mds-core --no-deps` is a blocking AC (AC-209-11), plus visual confirmation of three resolved anchors. Whether to add the CI job is **OD-209-G** |
| **R9 (NEW)** | A doctest in the new section emits a permanently ungated deprecation warning downstream (`clippy --all-targets` does not compile doctests; CI's `cargo test --workspace` runs them but does not fail on warnings) | Medium | D8: no compiled doctest; `cargo test --doc -p mds-core` must log zero `deprecated` warnings |
| **R10 (NEW)** | v0.5.0 deletion silently drops the only coverage of AC-F-23, I-13, L-FIX-REV1, A5, and A4-after-dedup | **High if unaddressed** | AC-209-15 forces either migration now or verbatim enumeration in the removal tracker. **OD-209-E** |

---

## 6. Verification

```bash
# Rust — nextest SKIPS doctests, so the --doc run is mandatory
cargo nextest run --workspace && cargo test --doc
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings   # HARD STOP on any warning
RUSTDOCFLAGS="-D warnings" cargo doc -p mds-core --no-deps   # NEW — CI has no rustdoc gate

# JS surfaces
npm ci && npm run build -w @mdscript/mds-wasm && npm run build --workspaces --if-present
npm test --workspaces --if-present
node scripts/verify-versions.mjs
```

Use the repo-local `.cargo/config.toml` workaround (`rustc-wrapper=""`, `jobs=2`) for local Rust runs; plain `cargo test --workspace` stalls ~20 min on fresh binaries. **Never commit that file.** `mds lint` exiting 2 on `examples/` is by design.

PR-specific checks:

1. **Forward positive control** — before any suppression, exactly 10 deprecation errors at the ten enumerated lines (§4 step 2).
2. **Reverse mutation control** — attribute deleted → exactly 11 `unfulfilled_lint_expectations`; restored → clean (§4 step 7).
3. **Suppression audit with a planted positive (shape-tolerant grep required)** — the audit grep MUST be shape-tolerant because rustfmt wraps every `#[expect(deprecated, reason = ...)]` across multiple lines; a single-line `expect(deprecated)` pattern is blind to all ten suppressions in the tree (applies ADR-009, avoids PF-013). Command: `grep -rn -A2 -e '#\[expect(' -e '#\[allow(' crates/ --include='*.rs' | grep deprecated`. Positive control: temporarily insert the multi-line attribute block — `#[expect(` on one line, `    deprecated,` on the next — in `crates/mds-core/src/lexer.rs` (e.g., inside `#[cfg(never)] fn _audit() { todo!() }`), confirm the grep reports a `deprecated,` context line in lexer.rs, remove the block, re-run. The planted form MUST be the multi-line attribute shape, NOT `// allow(deprecated)`; a comment only proves the old single-line pattern works. Clean run must hit only (a) fix.rs below line 1006, (b) `tests/api_surface.rs`, and (c) config.rs:287,289 (doc-comment prose). Read every hit individually; `-A2` produces multiple output lines per suppression, so a bare count is not evidence.
4. **Stale-guidance sweep** — `grep -n 'apply_fixes\b' crates/mds-core/src/lint/fix.rs`; every surviving hit must be the deprecated item's own docs, a factual comparison (624, 801), or a test identifier. Zero sentences may *instruct* a caller to use it.
5. **ADR sweep** — `grep -n 'ADR-001' crates/mds-core/src/lint/fix.rs` returns nothing in any lint-fix reverify-gate paragraph.
6. **Rustdoc** — zero broken intra-doc links; three resolved `apply_fixes_incremental` anchors on the FixPlan / apply_plan_unchecked / apply_fixes pages.
7. **Doctest hygiene** — `cargo test --doc -p mds-core` logs zero `deprecated` warnings.
8. **WASM byte identity** — build both artifacts at base and at head with the identical toolchain (Binaryen v129+ locally); raw sizes must be equal and ≤ 850,000. Compare the two `::notice::WASM ...` lines across CI runs as a cross-check.
9. **Diff shape** — `git diff -U0` on fix.rs shows only attribute, `///`, and `//` lines.
10. **Python surface** — no `pytest` run strictly required (zero `apply_fixes` references in `crates/mds-python`), but AC-209-17 asks for it as the cheap proof that nothing leaked.

---

## 7. Acceptance criteria

See the `acceptanceCriteria` array: AC-209-01 through AC-209-17. They cover API contract (01, 02, 17), functionality and lint-gate behavior (03, 04, 05, 06, 09), documentation correctness (07, 08, 11, 12, 14), release artifacts (13, 16), coverage protection (15), and performance (10 — explicit zero-byte codegen-delta threshold plus an explicit 'no runtime performance requirement'). Six are stated negatively (03, 04, 08, 09, 12, 17).

---

## 8. Plan self-review — what the original plan got right, and what it missed

**Got right (all re-verified):** every line anchor including all ten fn/call pairs; the stale-`fileRefs` correction; the zero-callers claim across all three binding crates and packages/*; that `since` is informational for third-party crates; that in-crate uses do warn; that a glob import does not fire the lint; that MSRV clears `#[expect]`; that D4's rejection of deprecating `apply_plan_unchecked` is technically correct (mds-cli:845 is a comment, not a call); PF-010-clean local IDs; the PF-016 caution on KNOWLEDGE.md.

**Missed:**
1. The issue's premise is falsified — `fix.rs` is absent at tag v0.3.0, so `apply_fixes` was never published and deletion is free right now. **OD-209-A.**
2. Six ADR-004 regression guards exist only on the deprecated path; the incremental suite covers none of them. **OD-209-E.**
3. fix.rs:585-588 attributes the lint --fix reverify gate to ADR-001, contradicting ADR-004 and the plan's own D6, inside the block the plan edits. **OD-209-D.**
4. CI has no `cargo doc` step, so the three intra-doc-link rewrites land unguarded. **OD-209-G.**
5. Doctests escape `-D warnings`, and a doc-fence `#[allow(deprecated)]` would trip the plan's own AC-209-2 grep. **D8.**
6. The positive control only proved the forward direction; the removal direction that AC-209-6 actually claims was never tested (PF-013 shape). **§4 step 7.**
7. The `#[expect]`-as-assertion mechanism has exactly one enforcement point, unstated.
8. `since` had a zero-cost alternative (omit it) that was never weighed. **OD-209-B.**
9. The CHANGELOG placement was justified by a Keep a Changelog rule the file itself violates.
10. The `note` draft was ~230 chars and contained a live `<OD-209-1>` placeholder.
11. R5 downgraded the WASM re-measure to 'confirmation only' when the wave rule requires a re-measure, and 'under budget' is the wrong assertion for a provably codegen-neutral change.
12. Three first-of-kind repo conventions (`#[deprecated]`, `#[expect]`, `AD-`) ride an XS PR without being flagged as a governance choice. **OD-209-F.**


## Improvements and Gaps Identified

- VERIFICATION RESULT — every line anchor in the plan checked out against 113f472. Confirmed accurate: fix.rs:690 `#[must_use]`, 691 `pub fn apply_fixes<F>`, rustdoc 663-689, FixPlan doc 238, apply_plan_unchecked doc 590 and 599, comment 624, comparison 801, FALLBACK_MAX_EDITS 774, FixOutcome `#[non_exhaustive]` 265, `#[cfg(test)]` 1006 / `mod tests` 1007 / `use super::*;` 1008, lib.rs:63-67 `pub use lint::{fix, ...}`, lint/mod.rs:31 `pub mod fix;`, api_surface.rs F-API-1 1374-1418, CHANGELOG `[Unreleased]` 8 / Security 89 / Added 427 / Changed 627 / Fixed 764, KNOWLEDGE.md 188 / 462 / 543 / 571. ALL TEN test fn-decl→call pairs verified exactly as tabled (1392→1448, 1654→1661, 1681→1687, 1739→1749, 1833→1850, 1869→1878, 1890→1898, 1957→1995, 2024→2050, 2371→2392). The plan's rejection of the issue's stale `fileRefs` (407-465) is correct. Zero unconfirmable claims. This is a well-verified plan; the gaps below are things it did not look for, not things it got wrong.
- BLOCKER-CLASS GAP 1 — the issue's core premise is falsified by the tree, and the plan accepted it without re-checking. `git show v0.3.0:crates/mds-core/src/lint/fix.rs` returns 'does not exist in v0.3.0'; the file was ADDED by 5a227dc (feat: mds lint #61 / PR #171) and `git tag --list 'v*'` shows the newest tag is v0.3.0. `mds::fix::apply_fixes` HAS NEVER BEEN PUBLISHED to crates.io, and per RELEASE CONTEXT the v0.4.0 tag is NOT cut. The issue asserts it 'cannot be deleted without a semver-breaking change since it is a public mds-core export' — that is false at this commit. Deleting it, or demoting it to `pub(crate)`, is FREE right now: zero downstream consumers exist and none can. Deprecating instead ships a brand-new public function that is born deprecated, plus 11 lint suppressions, plus a permanent v0.5.0 removal chore, plus a tracker issue, to preserve compatibility with nobody. ADR-010's own recorded rationale points the other way verbatim: 'the pre-publish window is the last moment the break is free and every unmarked public type is a permanent semver trap.' This is OD-209-A and it must be settled before a line is written.
- BLOCKER-CLASS GAP 2 — v0.5.0 test-coverage cliff, entirely unnoticed by the plan. Six ID-tagged regression guards live ONLY on the `apply_fixes` path and have no `apply_fixes_incremental` counterpart: (a) fix.rs:1869 `reverify_preexisting_untargeted_survives_and_fix_applies` — the AC-F-23 guard; (b) 1890 `reverify_new_untargeted_diagnostic_is_rejected`; (c) 1957 `tier_b_unused_function_standalone_apply_succeeds` — the I-13 end-to-end Tier B guard, whose own docstring says it 'closes the coverage gap identified in I-13'; (d) 2024 `l_fix_rev1_output_delta_causes_rejection` — L-FIX-REV1; (e) 1681 `l_fix_rev1_a5_rejection_message_pins_stable_prefix_and_suffix` — the A5 message contract; (f) 1392 `a4_partial_overlap_still_rejected_after_dedup` — A4 overlap-after-dedup. The incremental suite is INC-1..INC-8 (2075, 2094, 2113, 2150, 2204, 2236, 2278, 2404), `pf005_unsorted_edits_rejected_in_incremental` (2336) and `incremental_rejection_reason_escapes_embedded_error_display` (1767) — it covers NONE of (a)-(f). Two consequences: deleting `apply_fixes` at v0.5.0 silently deletes the only coverage of five ADR-004 reverify-gate behaviors; and, worse, TODAY those safety-critical behaviors are pinned against a function the production CLI no longer calls (mds-cli/src/lint.rs:451 and :571 both use `apply_fixes_incremental`). The deprecation makes the future deletion look free precisely because nobody has counted what it takes with it.
- GAP 3 — an ADR mis-citation sits inside the exact rustdoc block the plan edits. fix.rs:585 reads '# `_unchecked` suffix — ADR-001' and 586-588 read 'This function bypasses the ADR-001 reverify gate (compile-equivalence check)'. Per the ledger, ADR-001 is the *mds fmt* compile-equivalence gate, and ADR-004 states that gate 'is inapplicable BY CONSTRUCTION' to lint --fix. The plan rewrites 589-591 and asserts `applies ADR-004` in D6 — landing that without touching 585-588 ships one paragraph citing two mutually exclusive ADRs. Same defect at KNOWLEDGE.md:180 ('char-boundary guard (fail-closed, ADR-001)'). Correcting 585-588 is adjacent-breakage-in-the-same-block, not scope creep; KNOWLEDGE.md:180 is a scope call (OD-209-D).
- GAP 4 — no CI rustdoc gate exists, so the plan's three intra-doc-link rewrites are unguarded. `.github/workflows/ci.yml` has no `cargo doc` step and no `RUSTDOCFLAGS` anywhere; the rust job (lines 22-41) is exactly fmt + clippy + `cargo test --workspace`. A typo'd `[`apply_fixes_incremental`]` renders as literal text and no gate notices. The plan lists `cargo doc -p mds-core --no-deps` as a local check only — promote it to a blocking AC run with `RUSTDOCFLAGS="-D warnings"`, and decide whether to add the CI step (OD-209-G).
- GAP 5 — doctests are the one hole in the `-D warnings` gate, and the plan's own D2 walks into it. `cargo clippy --all-targets` does NOT compile doctests; CI's `cargo test --workspace` DOES run them but does not fail on warnings. So a migration example added to the new `# Deprecated (AD-209-1)` section that calls `apply_fixes` would emit an ungated `deprecated` warning in every downstream `cargo test` forever. And because AC-209-2's audit is a lexical grep over `crates/**/*.rs`, writing `#[allow(deprecated)]` inside a doc-comment code fence in `src/` would ALSO trip that grep. The plan never notices this interaction between D2 (write the migration guidance) and its own AC-209-2. Rule to state explicitly: the `# Deprecated` section may contain a ```text or ```ignore block only; no compiled doctest, and the literal string `allow(deprecated)` must not appear anywhere under `crates/*/src/`.
- GAP 6 — the plan's positive control only proves one direction. Step 2 proves `#[deprecated]` FIRES (expect exactly 10 clippy errors). It never proves the claim AC-209-6 actually makes — that removing the attribute BREAKS the build. Without a reversible mutation run, AC-209-6 is an absence-only assertion, which is the PF-013 shape the ledger already flags. Required second control: with all suppressions in place, temporarily delete the `#[deprecated(...)]` attribute and confirm `cargo clippy --workspace --all-targets -- -D warnings` fails with exactly 11 `unfulfilled_lint_expectations` diagnostics (10 in fix.rs + 1 in api_surface.rs), then restore. Verified as sound: all 10 call sites bind the result (`let outcome = apply_fixes(...)`), so `must_use` never fires and the count is not polluted.
- GAP 7 — the `#[expect]`-as-assertion mechanism has exactly one enforcement point and the plan does not say so. `unfulfilled_lint_expectations` is warn-by-default; only `cargo clippy --workspace --all-targets -- -D warnings` (ci.yml:38) promotes it to an error. `cargo test --workspace`, `cargo nextest run`, and the MSRV job (`cargo check -p mds-core -p mds-cli -p mds-python` — no `--all-targets`, so tests are never compiled) all let it pass as a warning. The 'build breaks loudly' claim in D3 is true of exactly one command. State it, so nobody later assumes a green `cargo build` means the attribute survived.
- GAP 8 — R2 (`since` drift) has a zero-cost fix the plan never considers: omit `since` entirely. `#[deprecated(note = "...")]` is legal with no `since` field. Verified `scripts/bump-version.mjs` rewrites only Cargo.toml `[workspace.package] version`, the four crate manifests, eight package.json files, and the CHANGELOG heading — never a `.rs` file — so the plan's own risk analysis is correct, but its mitigation (a manual grep line in RELEASING.md) is the weakest of three available options. Ranked: omit `since` (risk eliminated) > extend bump-version.mjs to rewrite `since = "..."` (risk automated away) > hardcode `0.4.0` + runbook grep (risk survives as a human step). See OD-209-B.
- GAP 9 — the CHANGELOG placement rationale cites a rule the file violates. The plan justifies inserting at line 764 with 'Keep a Changelog orders Added → Changed → Deprecated → Removed → Fixed → Security', but `[Unreleased]` in this repo puts `### Security` at line 89, ahead of `### Added` at 427. The file does not follow KaC ordering. The insertion point is still right; justify it as 'immediately before the existing `### Fixed` at 764, matching the file's own Added(427) → Changed(627) → Fixed(764) run', and drop the KaC appeal.
- GAP 10 — KNOWLEDGE.md collateral. `git ls-files` confirms `.devflow/features/mds-lint/KNOWLEDGE.md` is tracked, and `.gitignore:64-70` is the un-ignore block exactly as claimed (PF-016 check satisfied from the repo root, not a worktree). Beyond line 462, the plan should state what NOT to touch: KNOWLEDGE.md:4 (frontmatter `description:` keyword blob) and `.devflow/features/index.md:4` both list `apply_fixes_incremental` and do NOT list `apply_fixes` — correct as-is, leave them. Line 188 is a factual FixOutcome statement — leave. Line 543 (key-files list) and 571 (ADR-004 linkage) name only `apply_fixes_incremental` — leave. Only 462 changes. Saying this explicitly prevents a Coder from 'helpfully' adding the deprecated name to the keyword index.
- GAP 11 — the WASM re-measure must assert byte identity, not budget compliance. ci.yml:87-118 loops over BOTH `crates/mds-wasm/pkg/mds_wasm_bg.wasm` and `crates/mds-wasm/pkg-web/mds_wasm_bg.wasm`, emits `::notice::WASM <label>: <raw> bytes raw, <gz> bytes gzipped`, and fails only at `raw > 850000`. Because `#[deprecated]`, `#[expect]`, and doc comments emit zero codegen and the function body (695-762) is untouched, the correct assertion is delta == 0 bytes against the wave base under the same toolchain, on BOTH artifacts. A '≤ 850,000' pass would silently absorb a regression from an unrelated cause — with 3.5% headroom that is not a margin to spend on ambiguity. The plan's R5 'confirmation only, do not budget a full re-measure' is too weak given the wave-level rule that WASM-reachable PRs must re-measure.
- GAP 12 — the `note` string is downstream-facing UX and the draft is too long. rustc renders `note` verbatim, inline, at every downstream warning site. The plan's draft is ~230 characters on one logical line and wraps badly in terminals and CI logs. Keep `note` ≤ 160 chars carrying the replacement name + the one blocking caveat (closure must be `Fn`, not `FnOnce`); put the full three-delta migration record in the `# Deprecated (AD-209-1)` rustdoc section the plan already creates. Also: the draft embeds the literal token `<OD-209-1>`, which is a placeholder — the profile rejects placeholders, so OD-209-A/C must resolve before Step 2, and 'no placeholder token in the shipped note or CHANGELOG' needs to be an AC, not a footnote.
- GAP 13 — collateral-deletion hazard at fix.rs:236-240. The plan quotes only two lines of the FixPlan rustdoc, but the block continues: 'External crates that need an empty plan can use `FixPlan::default()`; its fields are `pub`, so they remain directly readable and writable.' That sentence is ADR-010 guidance and is unaffected by this PR. Instruct the Coder to preserve 239-240 verbatim while rewriting 238.
- GAP 14 — no criterion pins behavioral invariance mechanically. The strongest available guarantee here is that the diff contains zero executable change. Make it checkable: every hunk in `crates/mds-core/src/lint/fix.rs` must consist solely of attribute lines (`#[...]`), doc-comment lines (`///`), or `//` comment lines. `git diff -U0` on that file should show no added or removed line that is a statement. That is a stronger and cheaper assurance than re-running the suite alone.
- GAP 15 — repo-convention creep inside an XS PR. Verified: `grep -rn 'AD-[0-9]' --include='*.rs' crates/` returns nothing, `grep -rn '#\[expect('` returns nothing, and `grep -rn 'deprecated'` in crates/ returns nothing outside fix.rs. This PR would simultaneously introduce the repo's first `#[deprecated]`, first `#[expect]`, and a brand-new repo-wide `AD-<issue>-<n>` comment convention. The ID form is PF-010-clean (no leading `#`), so the convention itself is fine — but three first-of-kind conventions riding a two-line change should be a deliberate call, not a side effect (OD-209-F).
- GAP 16 — the plan never pins that the binding surfaces are untouched. Verified: zero `apply_fixes` references in crates/mds-napi, crates/mds-wasm, crates/mds-python, or packages/*. That is exactly why it is worth an explicit NEGATIVE criterion — the cheapest way to prove the deprecation leaked nowhere is to assert `crates/mds-napi/index.d.ts`, the `@mdscript/mds` type surface, and the Python `.pyi` stubs are byte-identical to base, rather than re-reasoning about it in review.
- GAP 17 — D4's rejection of deprecating `apply_plan_unchecked` is correct and I verified it: the only reference outside fix.rs is mds-cli/src/lint.rs:845, which is a comment ('Previously called apply_plan_unchecked directly, bypassing the reverify gate'), not a call. Its three live callers are fix.rs:720, 846, 895 — all internal. Keeping it public and undeprecated is right. Worth noting the plan should not, however, leave its safety rustdoc pointing at a deprecated function AND mis-citing ADR-001 (Gap 3) — those are the same block.
- MINOR — the plan's §6 verification block omits `cargo nextest run --workspace` skipping doctests as the reason `cargo test --doc` is mandatory locally, but CI runs `cargo test --workspace` (ci.yml:41), which DOES include doctests. So the doctest surface is CI-gated even though it is not clippy-gated. Stating both halves prevents a false sense that doctest warnings are caught by `-D warnings` (they are not) or that they are uncaught entirely (they run, they just do not fail).

## Acceptance Criteria

1. AC-209-01 (API contract, positive): Compiling an external crate that calls `mds::fix::apply_fixes` MUST emit exactly one `deprecated` diagnostic, and its rendered note MUST name `apply_fixes_incremental`, MUST state that the reverify closure bound changes from `FnOnce` to `Fn`, and MUST name the scheduled removal version. The note MUST NOT exceed 160 characters.
2. AC-209-02 (API contract, positive): `mds::fix::apply_fixes` MUST remain callable from an external crate with an unchanged signature `fn(&str, FixPlan, &LintResult, F) -> FixOutcome where F: FnOnce(&str) -> Result<LintResult, MdsError>`, and MUST return `FixOutcome::NothingToFix` when given a plan with zero edits and `overlap_rejected == false`. Every value in that call MUST come from a named constructor — never a struct literal (applies ADR-010). For the empty-plan case those constructors are `LintResult::new` and `plan_fixes`; `ByteEdit::deletion` is the named constructor to use IF a non-empty plan is ever needed, but the `NothingToFix` assertion requires zero edits, so its absence is correct here and MUST NOT be read as a violation.
3. AC-209-03 (functionality, NEGATIVE): `cargo clippy --workspace --all-targets -- -D warnings` MUST NOT emit any diagnostic of any kind.
4. AC-209-04 (functionality, NEGATIVE): No `#[allow(deprecated)]` or `#[expect(deprecated, ...)]` attribute — in any rustfmt-wrapped multi-line form — MUST appear in any file under `crates/*/src/` outside a `#[cfg(test)]` module; the prohibition also covers text inside `///` doc comments and doc-test code fences. The audit MUST use the shape-tolerant grep `grep -rn -A2 -e '#\[expect(' -e '#\[allow(' crates/ --include='*.rs' | grep deprecated` because rustfmt wraps multi-line attributes across lines and a single-line `expect(deprecated)` pattern is blind to all ten suppressions in the tree (applies ADR-009, avoids PF-013). The positive control MUST plant the multi-line attribute form (not a `// comment`) to prove the grep catches the exact shape used in fix.rs — see test plan #4 for the full procedure. Occurrences are permitted at exactly three groups: (a) inside `crates/mds-core/src/lint/fix.rs` below line 1006, (b) inside `crates/mds-core/tests/`, and (c) `crates/mds-core/src/lint/config.rs` lines 287 and 289 — the `/// #[allow(deprecated)]` doctest-prose lines for the already-deprecated `LintConfig::from_rules` (present since #302, before this PR; the `#\[allow(` outer pattern matches them as substrings even inside `///` doc comments). The audit grep WILL return hits at (c); they are expected and pre-existing. Any hit outside (a), (b), or (c) is a regression introduced by this PR. (Amends the original absolute prohibition per PF-015: an enumerated whitelist is less liable than an implicit completeness claim when pre-existing exceptions exist.)
5. AC-209-05 (functionality, positive, mutation-verified): With the implementation complete, deleting ONLY the `#[deprecated(...)]` attribute from `apply_fixes` (leaving the function and all suppressions in place) MUST trigger exactly 11 distinct `unfulfilled_lint_expectations` diagnostics — 10 anchored at the ten `deprecated,` lines inside the `#[expect(...)]` blocks in `crates/mds-core/src/lint/fix.rs` and 1 anchored at the `F-API-3` `#[expect(...)]` block in `crates/mds-core/tests/api_surface.rs` (applies ADR-009). The count MUST be asserted as the UNION of `cargo clippy --workspace --all-targets -- -D warnings` and `cargo clippy -p mds-core --test api_surface -- -D warnings`, never as a per-command count: removing the attribute leaves the lib rlib compiling clean, so the lib-test and `api_surface` targets become ready simultaneously and how the 11 split across the two commands is build-scheduling dependent. In the run verified for this PR the workspace command alone reported all 11; a serialized scheduler may instead report only the 10 fix.rs expectations, leaving the targeted command to supply the 11th. Both commands MUST exit 0 once the attribute is restored. If the union is fewer than 11, the pin is weaker than claimed and AC-209-05 fails.
6. AC-209-06 (functionality, positive control): At the intermediate state where `#[deprecated]` is attached but no suppression has been added, `cargo clippy -p mds-core --all-targets -- -D warnings` MUST report exactly 10 `deprecated` diagnostics, one per `let outcome = apply_fixes(` call site inside the `#[cfg(test)]` module. Those call sites sit at `crates/mds-core/src/lint/fix.rs` lines 1448, 1661, 1687, 1749, 1850, 1878, 1898, 1995, 2050, and 2392 at the wave base; add the line count of the inserted `#[deprecated(...)]` block (5 at the shipped shape) to each to get the anchors reported at the intermediate state. Fewer than 10 MUST be treated as a failed run, not a clean one.
7. AC-209-07 (documentation, NEGATIVE): After the change, no rustdoc sentence or code comment under `crates/mds-core/src/` and no line in `.devflow/features/mds-lint/KNOWLEDGE.md` MUST instruct a caller to USE `apply_fixes`. Surviving mentions are permitted only as (a) text inside the deprecated item's own doc block, (b) a factual comparison between the two functions — the fail-closed-validation comment inside `apply_plan_unchecked` and the `Unlike [\`apply_fixes\`] which requires \`F: FnOnce\`` sentence in the `apply_fixes_incremental` rustdoc (fix.rs:624 and fix.rs:847 post-change; these were 624 and 801 at the wave base, before the rustdoc and attribute insertions shifted the second one) — or (c) a Rust test identifier.
8. AC-209-08 (documentation consistency, NEGATIVE): `crates/mds-core/src/lint/fix.rs` MUST NOT contain any text attributing the `mds lint --fix` reverify gate to ADR-001. The `apply_plan_unchecked` doc block (currently 585-600) MUST cite ADR-004, consistent with ADR-004's recorded finding that ADR-001's compile-equivalence gate is inapplicable to lint --fix by construction.
9. AC-209-09 (behavior invariance, NEGATIVE): The diff MUST NOT alter any executable statement. Every added or removed line in `crates/mds-core/src/lint/fix.rs` MUST be an attribute line, a `///` doc line, or a `//` comment line. `cargo nextest run --workspace && cargo test --doc` MUST pass with zero test-assertion text changed anywhere in the repository.
10. AC-209-10 (performance, explicit threshold): The change MUST be codegen-neutral. Both `crates/mds-wasm/pkg/mds_wasm_bg.wasm` and `crates/mds-wasm/pkg-web/mds_wasm_bg.wasm`, built at this PR's HEAD, MUST have a raw byte size EQUAL to the same artifact built from the wave base commit with the identical toolchain (delta exactly 0 bytes), and MUST each remain ≤ 850,000 bytes. There is NO runtime-latency or throughput requirement for this PR; the reverify call bound of `apply_fixes_incremental` (≤ `edits.len() + 1`, capped by `FALLBACK_MAX_EDITS = 50`) is unchanged and out of scope.
11. AC-209-11 (documentation build, positive): `RUSTDOCFLAGS="-D warnings" cargo doc -p mds-core --no-deps` MUST succeed with zero diagnostics, and the generated pages for `FixPlan`, `apply_plan_unchecked`, and `apply_fixes` MUST each contain a resolved hyperlink to `apply_fixes_incremental` (not literal bracket text).
12. AC-209-12 (documentation, NEGATIVE): The new `# Deprecated` rustdoc section MUST NOT introduce any compiled doctest. `cargo test --doc -p mds-core` MUST emit zero `deprecated` warnings and MUST pass.
13. AC-209-13 (release artifact, positive) [Amended: original placement "immediately before `### Fixed`" would have buried the notice 1100+ lines into `[Unreleased]`; the section was moved to the visible upper portion of the file so users actually see it, which is the PR's purpose. Amending the AC rather than reverting the move — avoids PF-009]: `CHANGELOG.md` `[Unreleased]` MUST contain a `### Deprecated` section positioned in the visible upper portion of `[Unreleased]`, after `### Added` and before the first `### **BREAKING**` section, naming `mds::fix::apply_fixes`, naming `apply_fixes_incremental` as the replacement, listing the `Fn`-vs-`FnOnce` bound change and the newly reachable `FixOutcome::PartiallyFixed`, and citing a live GitHub issue number for the removal tracker. It MUST NOT contain any placeholder token (`#NEW`, `<OD-...>`, `TBD`, `XXX`) and MUST NOT use em dashes or en dashes. The em/en-dash check MUST be scoped to the `### Deprecated` section itself (lines between `### Deprecated` and the next `###` heading) to avoid false positives from pre-existing dashes in carried diff context lines.
14. AC-209-14 (traceability, positive): The `apply_fixes` rustdoc MUST carry an `AD-209-1` record stating: why the function is deprecated, the literal phrase `applies ADR-004`, all three migration deltas, and an explicit statement of why the body was retained rather than deleted at this commit given that `crates/mds-core/src/lint/fix.rs` does not exist at tag `v0.3.0`. The new `api_surface.rs` test MUST carry an `AD-209-2` record stating why an `#[expect]`-based pin was chosen over a `trybuild` compile-fail fixture. Neither ID MUST use a leading `#` (avoids PF-010).
15. AC-209-15 (coverage protection, positive): Every ADR-004 reverify-gate behavior that is currently asserted ONLY through `apply_fixes` MUST either gain an equivalent assertion on the `apply_fixes_incremental` path in this PR, or be enumerated by test name and line number in the v0.5.0 removal tracker issue body. The list that MUST be accounted for is exactly: `a4_partial_overlap_still_rejected_after_dedup` (1392), `l_fix_rev1_a5_rejection_message_pins_stable_prefix_and_suffix` (1681), `reverify_preexisting_untargeted_survives_and_fix_applies` (1869, AC-F-23), `reverify_new_untargeted_diagnostic_is_rejected` (1890), `tier_b_unused_function_standalone_apply_succeeds` (1957, I-13), `l_fix_rev1_output_delta_causes_rejection` (2024, L-FIX-REV1).
16. AC-209-16 (release consistency, positive): `node scripts/verify-versions.mjs` MUST pass. Additionally, after `node scripts/bump-version.mjs X.Y.Z` is run at release time, every `since = "..."` string under `crates/**/*.rs` MUST equal `X.Y.Z`; if the `since` field is omitted per OD-209-B, then `crates/**/*.rs` MUST contain zero `since =` strings and this check is vacuously satisfied by a grep returning no hits WITH a planted positive control proving the grep can find one.
17. AC-209-17 (surface isolation, NEGATIVE): The napi, WASM, Python, and universal-JS surfaces MUST NOT change. `crates/mds-napi/index.d.ts` (as regenerated), the `@mdscript/mds` exported type surface, and `crates/mds-python`'s type stubs MUST be byte-identical to the wave base, `npm test --workspaces --if-present` MUST pass, and `pytest crates/mds-python/tests -q` MUST pass with an unchanged test count.

## Test Plan

### 1. AC-209-01 — An external consumer calls the deprecated function and reads the compiler's note.

- **Scenario:** An external consumer calls the deprecated function and reads the compiler's note.
- **Setup:** In a scratch cargo project outside the repo (or a temporary `crates/mds-core/tests/deprecation_note.rs` deleted before commit), add `mds = { path = "<repo>/crates/mds-core" }` and write a fn that calls `mds::fix::apply_fixes("x\n", mds::fix::FixPlan::default(), &mds::LintResult::new(vec![]), |_| Ok(mds::LintResult::new(vec![])))`. Build with `cargo build --message-format=json 2>&1`.
- **Expected outcome:** Exactly one diagnostic with code `deprecated`. Its rendered message contains the substrings `apply_fixes_incremental`, `Fn`, `FnOnce`, and the removal version. The note substring is ≤ 160 characters as measured from the `#[deprecated(note = ...)]` literal in fix.rs.
- **Verification method:** integration

### 2. AC-209-02 — The deprecated function stays reachable and behaviorally unchanged across the crate boundary.

- **Scenario:** The deprecated function stays reachable and behaviorally unchanged across the crate boundary.
- **Setup:** Run the new `F-API-3` test in `crates/mds-core/tests/api_surface.rs`, which is compiled as a separate crate. It must mirror `fix_api_incremental_exists` (1381-1418): `use mds::fix::{apply_fixes, plan_fixes, ByteEdit, FixOutcome};` inside the fn body, build `LintResult::new(vec![])`, obtain the plan via `plan_fixes`, and call with a closure typed `|_s| -> Result<LintResult, MdsError>`.
- **Expected outcome:** `cargo nextest run -p mds-core --test api_surface` passes; the assertion `matches!(outcome, FixOutcome::NothingToFix)` holds. No struct literal appears anywhere in the test (grep the new test body for `FixPlan {`, `LintResult {`, `ByteEdit {` — zero hits).
- **Verification method:** integration

### 3. AC-209-03 — The zero-warnings gate holds at the final state.

- **Scenario:** The zero-warnings gate holds at the final state.
- **Setup:** Repo-local `.cargo/config.toml` with `rustc-wrapper=""` and `jobs=2` present but NOT committed. Run `cargo clippy --workspace --all-targets -- -D warnings` from the repo root.
- **Expected outcome:** Exit code 0, zero warning or error lines. Any `unfulfilled_lint_expectations` is a hard stop.
- **Verification method:** unit

### 4. AC-209-04 — Audit that no suppression leaked onto a production path, with a positive control proving the audit can see one.

- **Scenario:** Audit that no suppression leaked onto a production path, with a positive control proving the audit can see one.
- **Setup:** Step 1 (positive control, mandatory first): temporarily insert the multi-line attribute block — `#[expect(` on one line, `    deprecated,` on the next — into `crates/mds-core/src/lexer.rs` (wrap it in `#[cfg(never)] fn _audit() { todo!() }` to keep the file parseable), then run the shape-tolerant grep: `grep -rn -A2 -e '#\[expect(' -e '#\[allow(' crates/ --include='*.rs' | grep deprecated`. The planted form MUST be the multi-line attribute shape, NOT `// allow(deprecated)` — a comment only proves the old single-line pattern works, not the multi-line form that all ten fix.rs suppressions actually use. Step 2: remove the planted block and re-run the same grep.
- **Expected outcome:** Step 1 MUST report a `deprecated,` context line in `crates/mds-core/src/lexer.rs` — proving the grep detects the multi-line attribute form actually used in fix.rs (applies ADR-009, avoids PF-013). Step 2 MUST report hits only in the three permitted groups: (a) `crates/mds-core/src/lint/fix.rs` below line 1006, where each `#[expect(` block produces a `deprecated,` context line and a `reason = ...` context line (ten blocks total); (b) inside `crates/mds-core/tests/api_surface.rs`, including the `deprecated,` context lines from the three `#[expect(` blocks and the two doc-comment lines at 1663 and 1668; and (c) `crates/mds-core/src/lint/config.rs` lines 287 and 289 — `/// #[allow(deprecated)]` doctest-prose lines (the `#\[allow(` outer pattern matches them as substrings even inside `///` doc comments; confirmed pre-existing by `git show wave/v0.4.0-wave1:crates/mds-core/src/lint/config.rs | grep -n 'allow(deprecated)'`). Any hit in `crates/*/src/` outside zone (a) (above line 1006 in fix.rs) or outside zone (c) is a suppression leak introduced by this PR. Read every hit individually; `-A2` produces multiple output lines per suppression, making bare counts unreliable.
- **Verification method:** manual

### 5. AC-209-05 — Mutation test proving the #[expect] pins actually assert the deprecation, in the removal direction.

- **Scenario:** Mutation test proving the #[expect] pins actually assert the deprecation, in the removal direction.
- **Setup:** On a clean tree at the final implementation state, delete only the `#[deprecated(...)]` attribute block above `pub fn apply_fixes` in `crates/mds-core/src/lint/fix.rs`. Run `cargo clippy --workspace --all-targets -- -D warnings` and record the diagnostic count. Then also run `cargo clippy -p mds-core --test api_surface -- -D warnings` and record that count. Finally, `git checkout -- crates/mds-core/src/lint/fix.rs` and re-run both commands.
- **Expected outcome:** Both mutated runs MUST exit non-zero. The UNION of their diagnostics MUST be exactly 11 distinct `unfulfilled_lint_expectations`: 10 anchored at the ten `deprecated,` lines inside the `#[expect(...)]` blocks in fix.rs, and 1 anchored at the `F-API-3` `#[expect(...)]` block in api_surface.rs. Do NOT assert a per-command count — the split is build-scheduling dependent (the lib rlib compiles clean without the attribute, so the lib-test and `api_surface` targets become ready simultaneously). The workspace run alone reported all 11 in the run verified for this PR; a serialized scheduler may report only the 10 fix.rs expectations, in which case the targeted run supplies the 11th. Both restored runs MUST exit 0. If the union is fewer than 11, the pin is weaker than claimed and AC-209-05 fails.
- **Verification method:** integration

### 6. AC-209-06 — Positive control on the deprecation attribute itself, before any suppression exists.

- **Scenario:** Positive control on the deprecation attribute itself, before any suppression exists.
- **Setup:** Implement Step 2 only (attach `#[deprecated]` + write the rustdoc). Do NOT add any `#[expect(deprecated)]`. Run `cargo clippy -p mds-core --all-targets -- -D warnings 2>&1 | grep -c 'use of deprecated function'` and capture the full anchored line list.
- **Expected outcome:** Exactly 10 diagnostics at fix.rs 1448, 1661, 1687, 1749, 1850, 1878, 1898, 1995, 2050, 2392. Zero `unused_must_use` diagnostics (all ten sites bind the result). A count below 10 means the attribute is not applying and the PR is vacuous — stop and diagnose before adding a single suppression.
- **Verification method:** integration

### 7. AC-209-07 — No surviving documentation tells anyone to call the deprecated function.

- **Scenario:** No surviving documentation tells anyone to call the deprecated function.
- **Setup:** Run `grep -n 'apply_fixes\b' crates/mds-core/src/lint/fix.rs`, `grep -rn 'apply_fixes\b' crates/mds-core/src/ --include='*.rs'`, and `grep -n 'apply_fixes' .devflow/features/mds-lint/KNOWLEDGE.md`. Read every hit in full context.
- **Expected outcome:** Every fix.rs hit falls into exactly one of: inside 663-700 (the deprecated item's own docs), line 624 (factual comparison, unchanged), line 801 (factual comparison, unchanged), or a line above 1006 that is a test identifier or test doc. Lines 238, 590, and 599 name `apply_fixes_incremental` only. KNOWLEDGE.md line 462 names `apply_fixes_incremental()` only; lines 4, 188, 543, 571 are unchanged; `.devflow/features/index.md` is unchanged.
- **Verification method:** manual

### 8. AC-209-08 — The reverify gate is attributed to the ADR that actually governs it.

- **Scenario:** The reverify gate is attributed to the ADR that actually governs it.
- **Setup:** Run `grep -n 'ADR-001' crates/mds-core/src/lint/fix.rs` and read the surrounding `apply_plan_unchecked` doc block (currently 585-600). Cross-check against `.devflow/learning/decisions.md` ADR-001 (mds fmt compile-equivalence gate) and ADR-004 (lint --fix three-tier model, which states ADR-001's gate is inapplicable by construction).
- **Expected outcome:** Zero occurrences of `ADR-001` remain in any paragraph describing the lint --fix reverify gate. The `_unchecked` heading and its body cite ADR-004. If the scope decision (OD-209-D) also covers KNOWLEDGE.md:180, the same check applies there.
- **Verification method:** manual

### 9. AC-209-09 — The change provably touches no executable code.

- **Scenario:** The change provably touches no executable code.
- **Setup:** Run `git diff -U0 <wave-base>..HEAD -- crates/mds-core/src/lint/fix.rs` and inspect every `+`/`-` line. Then run `cargo nextest run --workspace && cargo test --doc` with the repo-local `.cargo/config.toml` workaround.
- **Expected outcome:** Every changed line begins (after leading whitespace) with `#[`, `///`, or `//`. Zero statement lines added or removed. All Rust tests pass; total test count is base + 1 (the new F-API-3) plus any tests added under OD-209-E. Note: the watch-suite (cli_watch.rs) and the Linux-gated HMR JS specs are known timing-flaky — re-run before treating a failure there as a regression.
- **Verification method:** integration

### 10. AC-209-10 — WASM artifacts are byte-for-byte unchanged, proving codegen neutrality.

- **Scenario:** WASM artifacts are byte-for-byte unchanged, proving codegen neutrality.
- **Setup:** Requires Binaryen v129+ locally (`brew install binaryen`). At the wave base commit run `npm run build -w @mdscript/mds-wasm` and record `wc -c < crates/mds-wasm/pkg/mds_wasm_bg.wasm` and `wc -c < crates/mds-wasm/pkg-web/mds_wasm_bg.wasm`. Clean, check out the PR head, rebuild with the identical toolchain, and record both again. Alternatively read the two `::notice::WASM ...` lines from ci.yml's 'Report WASM binary sizes' step (ci.yml:85-118) on both the base run and the PR run.
- **Expected outcome:** Both artifacts identical in raw byte size across base and head (delta exactly 0), and both ≤ 850,000. A nonzero delta means something other than this PR moved — investigate before merging; do not raise the budget. Explicitly record 'no runtime performance requirement applies to this PR' in the PR body.
- **Verification method:** integration

### 11. AC-209-11 — Rewritten intra-doc links resolve, in the absence of any CI rustdoc gate.

- **Scenario:** Rewritten intra-doc links resolve, in the absence of any CI rustdoc gate.
- **Setup:** Run `RUSTDOCFLAGS="-D warnings" cargo doc -p mds-core --no-deps`. Then open `target/doc/mds/fix/struct.FixPlan.html`, `.../fn.apply_plan_unchecked.html`, and `.../fn.apply_fixes.html` and confirm the `apply_fixes_incremental` references render as anchor tags.
- **Expected outcome:** Exit 0, zero `rustdoc::broken_intra_doc_links` diagnostics, and three resolved hyperlinks (not literal `[\`apply_fixes_incremental\`]` text). The `apply_fixes` page shows the rustdoc deprecation banner carrying the note.
- **Verification method:** integration

### 12. AC-209-12 — No compiled doctest calls the deprecated function.

- **Scenario:** No compiled doctest calls the deprecated function.
- **Setup:** Run `cargo test --doc -p mds-core 2>&1 | tee /tmp/doc.log`, then `grep -c 'deprecated' /tmp/doc.log`. Separately, inspect the new `# Deprecated (AD-209-1)` rustdoc section for code fences; any fence must be ```text or ```ignore.
- **Expected outcome:** Doctests pass; zero `deprecated` warnings in the log; zero un-annotated ```rust fences in the new section. Note nextest skips doctests, so this run is mandatory and cannot be inferred from a green `cargo nextest run`.
- **Verification method:** integration

### 13. AC-209-13 — The CHANGELOG entry is present, correctly placed, and placeholder-free.

- **Scenario:** The CHANGELOG entry is present, correctly placed, and placeholder-free.
- **Setup:** Run `grep -n '^### ' CHANGELOG.md | head -20` and read the new section in full. Then scope the placeholder and dash check to the `### Deprecated` section content only: `awk '/^### Deprecated$/{found=1; next} found && /^### /{exit} found{print}' CHANGELOG.md | grep -n '#NEW\|<OD-\|TBD\|XXX\|—\|–'`. Do NOT use `git diff`-hunk scoping or `sed` range patterns: (a) a carried context line at CHANGELOG.md:229 contains a pre-existing em dash that the diff hunk includes, producing a false positive, and (b) `sed` range patterns include the terminating `### ` line, which in this case is `### **BREAKING** —...` and introduces its own em dash hit. The `awk` form skips both the `### Deprecated` heading and the next section heading, printing only the section body.
- **Expected outcome:** `### Deprecated` appears after `### Added` and before the first `### **BREAKING**` section in `[Unreleased]`, in the visible upper portion of the file (the section was deliberately moved here so users skimming `[Unreleased]` see the deprecation notice, which is the PR's purpose). Content names `mds::fix::apply_fixes`, `apply_fixes_incremental`, the `Fn`/`FnOnce` bound change, `FixOutcome::PartiallyFixed`, the removal version, and a numeric GitHub issue reference. The section-scoped grep returns zero hits (no placeholder tokens, no em/en dashes within the `### Deprecated` section).
- **Verification method:** manual

### 14. AC-209-14 — Decision records are present and correctly formed.

- **Scenario:** Decision records are present and correctly formed.
- **Setup:** Run `grep -rn 'AD-209-1\|AD-209-2' crates/ --include='*.rs'` and read both blocks. Cross-check the ADR-004 claim against `.devflow/learning/decisions.md`.
- **Expected outcome:** Exactly two AD-209 RECORDS — count defining blocks, not grep hits. The raw grep returns 14 hits at the shipped tree (each of the ten `#[expect(...)]` suppressions cites `AD-209-1` in its `reason` string, the `apply_fixes` rustdoc heading is the eleventh, and the `F-API-3` docstring cites `AD-209-1` once and `AD-209-2` twice); a bare count is therefore not evidence either way. The two records are: `AD-209-1` in the `apply_fixes` rustdoc, which must contain the literal phrase `applies ADR-004`, all three migration deltas, and a why-not-deleted statement that references the v0.3.0 tag absence; and `AD-209-2` on `F-API-3`, which must explain the `#[expect]`-over-trybuild choice. Neither ID is written as `#209-1` (PF-010).
- **Verification method:** manual

### 15. AC-209-15 — The v0.5.0 deletion cannot silently drop ADR-004 coverage.

- **Scenario:** The v0.5.0 deletion cannot silently drop ADR-004 coverage.
- **Setup:** For each of the six named tests (fix.rs 1392, 1681, 1869, 1890, 1957, 2024), search the `apply_fixes_incremental` test block (fix.rs 2071-2488) for an assertion covering the same behavior. Record present/absent per behavior.
- **Expected outcome:** Under OD-209-E option 'migrate now': each of the six has a named `incremental_*` counterpart asserting the same outcome, and `cargo nextest run -p mds-core` passes with six additional tests. Under option 'defer': the removal tracker issue body contains the six test names with line numbers and the behavior each pins (AC-F-23, I-13, L-FIX-REV1, A5, A4-after-dedup), and the issue URL is cited in AD-209-1.
- **Verification method:** integration

### 16. AC-209-16 — Version consistency holds now and does not silently drift at release time.

- **Scenario:** Version consistency holds now and does not silently drift at release time.
- **Setup:** Run `node scripts/verify-versions.mjs`. Then run the drift audit WITH a positive control: first plant `// since = "9.9.9"` in `crates/mds-core/src/lib.rs`, run `grep -rn 'since = ' crates/ --include='*.rs'`, confirm the planted line is found, remove it, and re-run.
- **Expected outcome:** `verify-versions.mjs` exits 0. The planted control is detected (proving the grep works on this shell — BSD grep quirks excluded). The clean run returns EVERY `since =` string in the tree, and each one must equal the version the wave will tag. At the shipped tree that is exactly two hits, both `"0.4.0"`: `crates/mds-core/src/lint/config.rs` (the `LintConfig::from_rules` deprecation, landed earlier in this wave) and `crates/mds-core/src/lint/fix.rs` (this PR's `apply_fixes` deprecation). Do NOT assert a hit count of one — this PR is not the only v0.4.0 deprecation. Because a `since` string is kept, RELEASING.md gains a pre-flight line for this grep.
- **Verification method:** manual

### 17. AC-209-17 — No binding surface changed.

- **Scenario:** No binding surface changed.
- **Setup:** Run `npm ci && npm run build -w @mdscript/mds-wasm && npm run build --workspaces --if-present`, then `npm test --workspaces --if-present`. Diff the regenerated `crates/mds-napi/index.d.ts` (gitignored by design — regenerate on base and head and `diff` the two outputs). Run `python -m venv .venv && . .venv/bin/activate && pip install "maturin==1.13.3" pytest && maturin develop -m crates/mds-python/Cargo.toml && pytest crates/mds-python/tests -q`.
- **Expected outcome:** All JS workspace tests pass; pytest passes with an unchanged test count; the base-vs-head `index.d.ts` diff is empty. Note `mds lint` exiting 2 on `examples/` is BY DESIGN and is not a regression. Run this from the repo root working tree, never from an isolated worktree (PF-016), because the `index.d.ts` regeneration is tree-state dependent.
- **Verification method:** integration

## Merge Position

**Position 6 of 6** in the recommended merge order: PR5 — Deprecate apply_fixes (#209)

Reason: Inert as scoped and safe anywhere, but landing last validates its ten-call-site enumeration and reverse-mutation control against the final tree, and covers the case where OD-209-A/E grow it into end-to-end guards that would observe PR1's new diagnostic ordering.

### Plan Amendments from the Cross-PR Conflict Audit

**PR5 — Deprecate apply_fixes**

1) Record the verified cross-PR fact that resolves the flagged concern: crates/mds-cli/src/lint.rs calls only `mds::fix::apply_fixes_incremental` (:451, :571) and never `apply_fixes`, so no other wave PR touches the deprecated function and PR5's ten-call-site enumeration is stable. 2) Add a CONDITIONAL ordering note: if OD-209-A resolves to option A or D (delete now), or OD-209-E to option 1 (migrate the six guards), PR5 must land AFTER PR1. Reason, verified: fix.rs:355-360 sorts edits with a STABLE `sort_by`, so among identical `(start, end)` ranges the input order — i.e. `lint_result.diagnostics` order, iterated at fix.rs:337 — decides which edit survives `dedup_contained_or_identical`, and PR1's #202 reorders exactly that vector. Migrated end-to-end guards (notably the I-13 Tier B port) would observe the new ordering. 3) Record the insulating fact for the CURRENT corpus: `make_result` at fix.rs:1031 builds LintResult by struct literal, bypassing both `LintResult::new` and `LintResultBuilder::build`, so PR1's sort does not reach the existing fix.rs unit tests and OD-209-A option C is genuinely order-independent. 4) PR5's AC-209-02 test constructs `LintResult::new(vec![])` — empty, so PR1's open decision on whether to sort inside `new()` is a no-op here either way. 5) Landing last means the 10-site count and the 11-diagnostic reverse-mutation control are validated against the final tree; run both controls post-rebase, not only in isolation.
