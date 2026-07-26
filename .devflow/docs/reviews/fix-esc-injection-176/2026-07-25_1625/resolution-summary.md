# Resolution Summary

**Branch**: fix/esc-injection-176 -> main
**Date**: 2026-07-25
**Review**: .devflow/docs/reviews/fix-esc-injection-176/2026-07-25_1625
**Command**: /resolve

## Decisions Citations

- applies PF-014 — resolve-B2a (regression-1 cluster: sanitize inputs, never the rendered frame), resolve-B2b, resolve-B1b
- avoids PF-013 — resolve-B1a (testing-3, testing-7), resolve-B4 (python-2), resolve-B5 (consistency-1), resolve-B7 (testing-1, testing-9, testing-12), resolve-B3 (display_sanitized tests)
- avoids PF-004 — resolve-B2a (formatter.rs parallel NamedSource path), resolve-B4 (python-5 typed path), resolve-B2b (5 hand-rolled sites unified)
- applies PF-007 — resolve-B4 (typed/wire parity), resolve-B5 (native-vs-WASM differential)
- avoids PF-008 — resolve-B7 (testing-12 fail-closed gates)

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 110 |
| Fixed | 94 |
| False Positive | 0 |
| By Design | 0 |
| Deferred | 11 |
| Blocked | 0 |
| Escalated | 5 |

_(Note: `Deferred` = `## Fix Separately` count + `## Deferred to Tech Debt` count combined — the two sections are distinct by scope, but the Statistics row aggregates both for the convergence parser.)_

## Verification
| Command | Result |
|---------|--------|
| cargo fmt --all -- --check | PASS |
| cargo clippy --workspace --all-targets -- -D warnings | PASS |
| cargo nextest run --workspace (1925 tests) | PASS |
| cargo test --doc --workspace (36 doc tests) | PASS |
| maturin develop + pytest crates/mds-python/tests (223 tests) | PASS |
| npm test -w @mdscript/mds-napi (98 tests) | PASS |
| npm test -w @mdscript/mds (263 tests) | PASS |

Regression tests added: 27

Final gate: PASS

## Fixed Issues
| Issue | File:Line | Commit |
|-------|-----------|--------|
| performance-1: sanitize_control_chars always allocates (String::with_capacity(s.len())) and copies char-by-char even when the | crates/mds-core/src/lint/diagnostic.rs:416 | ed24492 |
| rust-7: sanitize_control_chars returns String unconditionally where Cow<'_, str> is the idiomatic signature—a &str ->  | crates/mds-core/src/lint/diagnostic.rs:418 | ed24492 |
| reliability-3: String::with_capacity(s.len()) is guaranteed insufficient for inputs containing control characters: every esca | crates/mds-core/src/lint/diagnostic.rs:417 | ed24492 |
| performance-7: fmt::write(&mut out, format_args!("\\u{:04X}", ch as u32)) invokes the full width/zero-pad formatting machiner | crates/mds-core/src/lint/diagnostic.rs:424 | ed24492 |
| security-3: to_canonical_json() sanitizes message and help but emits the per-file group key verbatim: "file": file | crates/mds-core/src/lint/diagnostic.rs:341 | ed24492 |
| reliability-5: file is the one user-controlled text channel left unsanitized on every JSON and typed surface | crates/mds-core/src/lint/diagnostic.rs:292 | ed24492 |
| compliance-2: The PR claim 'every serialization boundary' overstates coverage: in to_canonical_json(), message and help are  | crates/mds-core/src/lint/diagnostic.rs:344 | ed24492 |
| security-8: Compile warnings are printed and serialized unsanitized at main.rs:278, :287, :340, build.rs:694, :1148, and C | crates/mds-cli/src/main.rs:278 | ed24492 |
| architecture-8: Unsanitized warning emission via emit_warnings (lib.rs:507-511), CompileResult::to_canonical_json warnings (li | crates/mds-core/src/lib.rs:507 | ed24492 |
| rust-8: let _ = fmt::write(&mut out, format_args!("\\u{:04X}", ch as u32)) discards a Result and uses fmt::write + for | crates/mds-core/src/lint/diagnostic.rs:426 | ed24492 |
| complexity-8: let _ = fmt::write(&mut out, format_args!(...)) is an unusual form that reads as though something subtle is ha | crates/mds-core/src/lint/diagnostic.rs:424 | ed24492 |
| rust-9: Public sanitize_control_chars lacks #[must_use] and a doctest, breaking mds-core's own API convention | crates/mds-core/src/lint/diagnostic.rs:418 | ed24492 |
| testing-3: The diff adds help sanitization at two new boundaries: diagnostic.rs:328 and mds-python/src/lib.rs:784 | crates/mds-core/src/lint/diagnostic.rs:328 | ed24492 |
| testing-7: Idempotency is claimed in three places (diagnostic.rs:414, output.rs:494, and the KB) and asserted nowhere | crates/mds-core/src/lint/diagnostic.rs:414 | ed24492 |
| regression-1: Whole-frame sanitization escapes miette's own ANSI colour codes, so interactive mds lint and mds watch output  | crates/mds-cli/src/lint.rs:289 | 5d35da0 |
| security-1: Whole-frame sanitization mangles miette's own ANSI styling: interactive mds lint / mds watch output is now lit | crates/mds-cli/src/lint.rs:289 | 5d35da0 |
| security-2: Caret/underline is misaligned whenever the source line contains a control char | crates/mds-cli/src/output.rs:495 | 5d35da0 |
| reliability-1: Post-render sanitization desynchronizes miette's caret/underline from the source line it points at | crates/mds-cli/src/output.rs:496 | 5d35da0 |
| reliability-2: render_result_human iterates result.diagnostics (capped at MAX_DIAGNOSTICS=1000) | crates/mds-cli/src/lint.rs:283 | 5d35da0 |
| architecture-3: The field-level pre-sanitize in render_diag_human builds a full LintDiagnostic clone to sanitize message and h | crates/mds-cli/src/lint.rs:266 | 5d35da0 |
| complexity-4: The comment at lint.rs:266-268 claims the double-pass 'guards against any future path that might bypass the ou | crates/mds-cli/src/lint.rs:266 | 5d35da0 |
| documentation-8: The comment at lint.rs:266-268 justifies the field-level sanitize as guarding 'against any future path that mi | crates/mds-cli/src/lint.rs:266 | 5d35da0 |
| rust-2: The comment at lint.rs:265-267 claims the inner field-level pass 'guards against any future path that might by | crates/mds-cli/src/lint.rs:265 | 5d35da0 |
| reliability-4: The field-level pass builds a complete LintDiagnostic clone including span.clone(), file.clone(), fix_removals | crates/mds-cli/src/lint.rs:266 | 5d35da0 |
| performance-3: The redundant field-level sanitize pass in render_diag_human is fully subsumed by the whole-report pass in epr | crates/mds-cli/src/lint.rs:272 | 5d35da0 |
| rust-1: render_diag_human builds a render-only sanitized local with a full-field struct literal that includes fix_remo | crates/mds-cli/src/lint.rs:269 | 5d35da0 |
| performance-6: The sanitized LintDiagnostic clone at lint.rs:276-277 clones fix_removals and fix_edits (Vec<TextEdit>, each w | crates/mds-cli/src/lint.rs:276 | 5d35da0 |
| testing-4: The named_source = None branch of render_diag_human is unreachable | crates/mds-cli/src/lint.rs:288 | 5d35da0 |
| documentation-6: output.rs:509-511 claims 'miette box-drawing and carets therefore survive intact' and lint.rs:284-287 claims ' | crates/mds-cli/src/output.rs:509 | 5d35da0 |
| testing-2: All five new e2e vectors force NO_COLOR=1 deliberately, so the suite has zero coverage of the default interact | crates/mds-cli/src/lint.rs:283 | 5d35da0 |
| regression-3: All five new ESC tests force NO_COLOR=1 deliberately, making the suite pin only the non-default configuration | crates/mds-cli/tests/cli_lint.rs:343 | 5d35da0 |
| testing-10: T-10 depends on ambient colour configuration: format!("{report:?}") uses miette's global handler | crates/mds-cli/src/output.rs:812 | 5d35da0 |
| reliability-8: T-10's assertion set (no 0x1B, contains \u001B, contains Hello) cannot distinguish a correct frame from a care | crates/mds-cli/src/output.rs:812 | 5d35da0 |
| testing-5: Two defence-in-depth layers added in this diff cannot be distinguished from their own absence by any test: (1) | crates/mds-cli/src/lint.rs:271 | 5d35da0 |
| rust-5: After this PR the crate ships two text accessors with opposite safety properties: e.serialize().message is san | crates/mds-core/src/error.rs:826 | 5d35da0 |
| architecture-2: MdsError derives Display via thiserror and is publicly re-exported; a downstream Rust consumer writing the nat | crates/mds-core/src/error.rs:826 | 5d35da0 |
| consistency-7: error.rs uses crate::lint::sanitize_control_chars(...) inline twice at lines 826 and 828; the symbol is re-exp | crates/mds-core/src/error.rs:826 | 5d35da0 |
| rust-12: error.rs uses crate::lint::sanitize_control_chars(...) inline at lines 826 and 828 while crate::sanitize_contr | crates/mds-core/src/error.rs:826 | 5d35da0 |
| compliance-3: err.detail bypasses the new MdsError::serialize() choke-point: it is set by the catch_unwind wrappers outside  | crates/mds-napi/src/lib.rs:310 | 5d35da0 |
| python-1: Change #2 breaks typed-vs-wire parity: message/help are sanitized when populating the typed pyclass at lines 7 | crates/mds-python/src/lib.rs:780 | dfb21a5 |
| architecture-4: The Python belt-and-suspenders guard re-sanitizes json_str(d, "message") where d is already a serde_json::Valu | crates/mds-python/src/lib.rs:776 | dfb21a5 |
| python-4: The code comment at lib.rs:775-779 claims the wrap 'ensures as_json()/to_dict() parity for any future code pat | crates/mds-python/src/lib.rs:775 | dfb21a5 |
| performance-4: message: mds::sanitize_control_chars(&json_str(d, "message")) causes two wasted allocations per diagnostic fie | crates/mds-python/src/lib.rs:780 | dfb21a5 |
| rust-4: message: mds::sanitize_control_chars(&json_str(d, "message")) double-allocates: json_str already returns an ow | crates/mds-python/src/lib.rs:779 | dfb21a5 |
| python-5: LintFileReport.file carries raw C0/DEL/C1 bytes—json_str(file_val, "file") is populated unsanitized in the sam | crates/mds-python/src/lib.rs:752 | dfb21a5 |
| python-2: E12 cannot fail if the pyclass sanitize wrap at lib.rs:780/784 is deleted—it validates core B4, not the Python | crates/mds-python/tests/test_errors.py:232 | dfb21a5 |
| python-6: The (b) comment block inside E12's first loop sits inside for diag in all_diags: but the (b) assertion actuall | crates/mds-python/tests/test_errors.py:274 | dfb21a5 |
| python-7: Only ESC (U+001B) is exercised on the Python surface | crates/mds-python/tests/test_errors.py:200 | dfb21a5 |
| python-3: E12 docstring states the vector uses 'a module whose NAME contains a raw ESC byte', but the resulting file key | crates/mds-python/tests/test_errors.py:272 | dfb21a5 |
| consistency-1: Three lint-path binding tests accept either escape casing via || alternatives: napi T-13b, Python E12, and WAS | crates/mds-napi/__test__/index.spec.mjs:1272 | 555095f |
| consistency-2: The vector-label scheme has three conflicting accountings and T-11 does not exist | crates/mds-napi/__test__/index.spec.mjs:1193 | 555095f |
| testing-11: Binding surfaces test ESC only (U+001B); DEL/C1 stop at core + CLI | crates/mds-napi/__test__/index.spec.mjs:1200 | 555095f |
| testing-6: Six per-surface goldens exist but zero differential assertions | crates/mds-napi/__test__/index.spec.mjs:1275 | 555095f |
| consistency-5: The control-char assertion predicate is spelled four ways with naming inconsistency: napi uses assertNoControl | crates/mds-napi/__test__/index.spec.mjs:1203 | 555095f |
| complexity-3: The control-byte assertion predicate (C0-excl-\t\n / DEL / C1) appears six times across the PR | crates/mds-wasm/tests/web.rs:790 | 555095f |
| consistency-6: The feature-KB anchor note states E11/E12 live in test_errors.py/test_lint.py, but crates/mds-python/tests/tes | .devflow/features/mds-lint/KNOWLEDGE.md | 555095f |
| architecture-6: Five sibling sites (lint.rs:95, lint.rs:1408, main.rs:232, main.rs:348, build.rs:1548) still hand-roll eprintl | crates/mds-cli/src/lint.rs:95 | 9b1e3c3 |
| security-7: mds build / mds check already mangle miette colours via pre-existing sanitize_control_chars(&format!("{e:?}")) | crates/mds-cli/src/main.rs:232 | 9b1e3c3 |
| security-5: mds lint status lines print attacker-controlled filenames raw at lint.rs:792, :820, :843, :866, :1163, :1300,  | crates/mds-cli/src/lint.rs:792 | 9b1e3c3 |
| architecture-10: ESC-bearing filenames are a distinct unaddressed vector: eprintln!("error writing {}: {e}", file.display()) at | crates/mds-cli/src/lint.rs:1136 | 9b1e3c3 |
| security-6: Same raw-filename pattern across fmt.rs (lines 61, 178, 181, 189, 194, 272, 318) and build.rs (lines 564, 1028 | crates/mds-cli/src/fmt.rs:61 | 9b1e3c3 |
| regression-4: mds check, mds build, and mds fmt already mangle colour via pre-existing eprint_error and inline-sanitize site | crates/mds-cli/src/fmt.rs:232 | 9b1e3c3 |
| documentation-12: eprint_error's doc states handlers 'MUST use this helper' and that centralizing the render 'means the sanitize | crates/mds-cli/src/output.rs:501 | 9b1e3c3 |
| consistency-4: eprint_error call style forks within this PR: watch.rs:42 adds eprint_error to its use crate::output::{...} bl | crates/mds-cli/src/watch.rs:42 | 9b1e3c3 |
| rust-3: render_error_sanitized is pub(crate) but its only non-test caller is eprint_error at output.rs:513—same module | crates/mds-cli/src/output.rs:495 | 9b1e3c3 |
| reliability-7: render_error_sanitized doc claims 'The render+sanitize pass is idempotent: calling it a second time on already | crates/mds-cli/src/output.rs:492 | 9b1e3c3 |
| documentation-14: render_diag_human's summary line still reads 'applying sanitize_control_chars at the boundary', written for th | crates/mds-cli/src/lint.rs:256 | 9b1e3c3 |
| documentation-4: 'ALL serialization and render boundaries' is factually false on two counts: (1) CompileResult::to_canonical_js | crates/mds-core/src/lint/diagnostic.rs:7 | 9ef6576 |
| architecture-1: The rewritten module doc asserts sanitization is applied at 'ALL serialization and render boundaries' and enum | crates/mds-core/src/lint/diagnostic.rs:7 | 9ef6576 |
| consistency-3: Two boundary lists in the same file (module header lines 7-14 and sanitize_control_chars fn doc lines 405-407, | crates/mds-core/src/lint/diagnostic.rs:7 | 9ef6576 |
| documentation-7: The module header (lines 7-17) and the sanitize_control_chars fn doc (lines 405-408), both rewritten in this P | crates/mds-core/src/lint/diagnostic.rs:405 | 9ef6576 |
| documentation-3: Line 135 reads '**Sanitization**: apply sanitize_control_chars at the CLI render boundary **only**.' This is f | crates/mds-core/src/lint/diagnostic.rs:135 | 9ef6576 |
| rust-6: LintDiagnostic struct doc at line 135 reads '**Sanitization**: apply sanitize_control_chars at the CLI render  | crates/mds-core/src/lint/diagnostic.rs:135 | 9ef6576 |
| architecture-7: Line 135 still reads '**Sanitization**: apply sanitize_control_chars at the CLI render boundary only.' The mod | crates/mds-core/src/lint/diagnostic.rs:135 | 9ef6576 |
| documentation-2: The LintDiagnostic doc comment reads: 'Implements std::error::Error + miette::Diagnostic so it can be rendered | crates/mds-core/src/lint/diagnostic.rs:123 | 9ef6576 |
| complexity-5: LintDiagnostic struct doc at line 123 still reads 'so it can be rendered by miette at the CLI boundary: eprint | crates/mds-core/src/lint/diagnostic.rs:123 | 9ef6576 |
| documentation-16: The field doc at line 143 reads 'Raw — do not sanitize in the constructor' which is accurate and should stay,  | crates/mds-core/src/lint/diagnostic.rs:143 | 9ef6576 |
| documentation-11: sanitize_control_chars is now a load-bearing public API of the crates.io-published mds-core, re-exported at th | crates/mds-core/src/lint/diagnostic.rs:401 | 9ef6576 |
| testing-1: T-9 is fully vacuous for three independent reasons: (1) the ESC never reaches any output field—serde_yaml_ng r | crates/mds-cli/tests/cli_lint.rs:1795 | a2a9cb0 |
| documentation-15: T-9's redesign rationale (that serde_yaml_ng rejects raw ESC in YAML frontmatter keys so T-9 became a wire-lev | crates/mds-cli/tests/cli_lint.rs:1784 | a2a9cb0 |
| testing-12: T-9's Gate 3 fails open: if let Some(files) = json["files"].as_array() silently skips the whole check when the | crates/mds-cli/tests/cli_lint.rs:1841 | a2a9cb0 |
| complexity-1: T-9's Gate 2 iterates stdout_str.bytes() and tests (0x80..=0x9F).contains(&byte) for C1 | crates/mds-cli/tests/cli_lint.rs:1827 | a2a9cb0 |
| complexity-2: Four new tests (lines 1614, 1663, 1700, 1757) hand-roll command construction that lint_path and lint_stdin hel | crates/mds-cli/tests/cli_lint.rs:1614 | a2a9cb0 |
| complexity-9: let stderr = out.stderr.clone() clones the buffer needlessly in four new tests (lines 1622, 1671, 1717, 1767) | crates/mds-cli/tests/cli_lint.rs:1622 | a2a9cb0 |
| testing-9: Pre-existing ESC test lint_esc_byte_in_syntax_error_is_sanitized_on_stderr (unchanged by this PR) calls lint_p | crates/mds-cli/tests/cli_lint.rs:1563 | a2a9cb0 |
| testing-8: A3 (11 watch.rs call sites) ships with no test coverage: crates/mds-cli/tests/cli_watch.rs is untouched and no | crates/mds-cli/src/watch.rs:790 | a2a9cb0 |
| documentation-1: CHANGELOG.md is untouched on this branch: grep for 'sanitiz', 'control char', 'CWE-150', '#176', and 'injectio | CHANGELOG.md | ae0fd23 |
| compliance-1: git diff main...HEAD touches 15 files and zero of them is CHANGELOG.md | CHANGELOG.md | ae0fd23 |
| regression-2: The PR body states a Behavioral change noting err.message, err.help, and lint diagnostic messages now carry \u | CHANGELOG.md | ae0fd23 |
| documentation-5: spec.md §7.5 is the normative wire-format documentation for mds lint --format json | spec.md:971 | ae0fd23 |
| architecture-5: to_canonical_json() now mutates the value domain of message/help while the envelope version stays 1 | crates/mds-core/src/lint/diagnostic.rs:327 | ae0fd23 |
| documentation-9: The PR body states '@mdscript/bundler-utils (normalizeError) .. | (none) | ae0fd23 |
| regression-5: The PR body names the bundler entry point normalizeError; the actual export is formatMdsError (packages/bundle | (none) | ae0fd23 |
| documentation-10: The PR body claims '15 test vectors added across all surfaces' and maps T-11..T-15 to four binding surfaces (f | (none) | ae0fd23 |

## False Positives
| Issue | File:Line | Reasoning |
|-------|-----------|-----------|

(none — every spot-checked reviewer claim held)

## By Design
| Issue | File:Line | Rationale (ADR/doc) |
|-------|-----------|---------------------|

(none)

## Fix Separately
| Issue | File:Line | Reason | Tracked |
|-------|-----------|--------|---------|
| performance-2: render_diag_human clones the entire source file for every single diagnostic via src.to_str | crates/mds-cli/src/lint.rs:285 | src.to_string() per diagnostic identical to main; Arc<str> refactor is its own ticket | #255 |
| performance-5: accumulate_result_json deep-clones every diagnostic Value via json_files.extend(arr.iter() | crates/mds-cli/src/lint.rs:1416 | accumulate_result_json deep clone, pre-existing; sibling to #173 | #173 (comment) |
| complexity-6: run_watch_file is ~311 lines, fully pre-existing | crates/mds-cli/src/watch.rs:886 | run_watch_file ~311 lines, fully pre-existing; PR changed one line | #256 |
| complexity-7: eprint_error + error-settle pattern is repeated in two identical pairs: watch.rs:869/:877  | crates/mds-cli/src/watch.rs:869 | watch.rs duplicate pairs; restructuring cost exceeds benefit in this PR | #257 |
| reliability-6: eprintln! panics with 'failed printing to stderr' if the write fails (e.g | crates/mds-cli/src/output.rs:513 | eprintln! EPIPE panic — every site was already eprintln! on main | #258 |
| rust-10: Public LintDiagnostic is not #[non_exhaustive] while its sibling public Message is | crates/mds-core/src/lint/diagnostic.rs | #[non_exhaustive] on LintDiagnostic — PRE-TAG deadline: free at zero users, breaking after v0.4.0 | #259 |
| rust-11: format!("{report:?}") materializes the entire rendered frame, then sanitize_control_chars  | crates/mds-cli/src/output.rs:495 | double materialization; likely moot after PF-014 redesign — re-check then close or fix | #260 |
| architecture-9: render_error_sanitized name describes the transformation but not that it is the boundary;  | crates/mds-cli/src/output.rs:495 | rename render_error_sanitized — revisit now that PF-014 reshaped it | #261 |
| architecture-11: sanitize_control_chars lives in the lint module but is now a core cross-cutting concern: e | crates/mds-core/src/lint/diagnostic.rs:416 | move sanitizer to mds-core/src/sanitize.rs; orthogonal to closing #176 | #262 |
| documentation-13: spec.md documents the lint JSON wire format (§7.5) but has no comparable normative section | spec.md | normative spec section for serialized error shape; pre-existing gap | #263 |
| python-8: E11 uses try/except/else + pytest.fail rather than pytest.raises, which is less idiomatic | crates/mds-python/tests/test_errors.py:246 | pytest.raises idiom conditioned on file-wide style migration | #264 |

## Deferred to Tech Debt
| Issue | File:Line | Risk Factor |
|-------|-----------|-------------|

(none — all deferrals are scoped FIX_SEPARATE tickets)

## Escalations
| Issue | File:Line | Security Concern | Decision (2026-07-25) |
|-------|-----------|-----------------|----------------------|
| security-4: The sanitizer character class is C0 (minus \n/\t), DEL, and C1 | crates/mds-core/src/lint/diagnostic.rs:416 | Sanitizer omits bidi/Trojan Source U+202E (CVE-2021-42574) + U+2028/29 — widening the escape class is a wire-format decision | **WIDEN** (implemented 2026-07-26): extend escape class to bidi/Trojan-Source characters (U+200E/200F, U+202A–202E, U+2066–2069) and JS-hazard separators/BOM (U+2028/2029, U+FEFF), escaped as the existing uppercase `\uXXXX` literal form. Rationale: CVE-2021-42574 class; these pass through C0/DEL/C1 filtering untouched. |
| security-9: \n pass-through permits diagnostic-line forging in unwrapped consumers: a YAML key 'a\nerr | crates/mds-core/src/lint/diagnostic.rs:419 | The \n carve-out enables CWE-117 log-record forging in unwrapped consumers — keep or close is an owner decision | **ESCAPE `\n` ON WIRE ONLY** (implemented 2026-07-26): wire/API/JSON surfaces escape newline as `\n` literal to close CWE-117 log-record forging; the CLI human render keeps real newlines so multi-line diagnostics still render. One shared escape map behind a mode flag — deliberately NOT two tables. |
| security-10: Sanitization is not injective: source text containing the 6 literal chars \u001B is indist | crates/mds-core/src/lint/diagnostic.rs:416 | Sanitizer is non-injective (no backslash escaping); fixing churns the wire format on all 5 surfaces pre-tag | **ACCEPT + DOCUMENT** (joint decision with reliability-9, implemented 2026-07-26): escaping stays one-way, lossy and non-injective. A literal 6-character `\u001B` string in template source and a real ESC byte are indistinguishable after sanitization. The contract now explicitly forbids consumers un-escaping `\uXXXX` back to bytes; round-tripping is a permanent non-goal. No backslash escaping — it would churn the wire format across five surfaces for no security gain. Documented normatively in `spec.md` §7.5 and in `sanitize_control_chars` rustdoc. |
| reliability-9: sanitize_control_chars is not injective: sanitize_control_chars("\\u001B") and sanitize_co | crates/mds-core/src/lint/diagnostic.rs:416 | Duplicate of security-10 — decide once | **ACCEPT + DOCUMENT** (same decision as security-10 — decided as one, implemented 2026-07-26). See security-10 row for full rationale. |
| security-11: --diff / --check output echoes raw source lines to the terminal at lint.rs:1431 and fmt.rs | crates/mds-cli/src/lint.rs:1431 | --diff/--check echo raw source bytes to stdout by design; explicit accept-or-fix decision requested | **TTY-GATED NEUTRALIZE** (implemented 2026-07-26): `--fix --diff` preview output neutralizes control bytes when stdout is a terminal, and stays byte-faithful when piped so diffs remain usable by `patch`/tooling. NO_COLOR does not affect it — this is safety, not styling. Note precisely: neutralization applies to the `--diff` preview text; `--check` alone emits only `Would fix:`/`Would reformat:` status lines, which are unconditionally sanitized via `safe_path` and are not TTY-gated. |

> **Decisions recorded 2026-07-25; implementation landed 2026-07-26.** Originally escalated as escape-map / wire-contract decisions (bidi coverage, \n carve-out, non-injectivity, --diff raw echo). All five decided as a batch in the last free wire-format window: zero users, pre-v0.4.0-tag, so wire-format churn costs nothing now and would be breaking later.

**Implementing commits (branch `fix/esc-injection-176`):** `d5c7975` (core: widened class + wire mode), `3c383ad` (cross-surface tests), `7f26e3f` (spec §7.5 + CHANGELOG), `5fa54f4` (TTY-gated preview), `4f591c7` (diff-renderer consolidation), `ad2a673` (WASM parity test), `61120e7` (strip raw control bytes from comments), `ba5dde0` (P0: SanitizedReport at the stderr choke-point), `f2e2874` (boundary-table closure), `cb6d860` (eprint_warning).

**Mid-flight expansion (2026-07-26, owner-approved):** After the five escalation decisions were implemented, a Scrutinizer pass demonstrated a raw-ESC-to-stderr path in `MdsError` message text on the CLI human error path (found by the Scrutinizer), plus the CLI-authored `miette!()` error family and warning prints. Fixing these was approved mid-flight as an addition to the original five escalations — not one of the five — and committed to the same branch.

**Alignment-review finding (round 1, closed):** A subsequent alignment review found the boundary-closure claim still incomplete: a bare `eprintln!` for unknown `mds.json` rule names at `crates/mds-cli/src/lint.rs:197`, and raw `MdsError` Display interpolated into `fix rejected:` reasons. Both were fixed (`e145e41`, `46fb326`) and the claims narrowed (`e1d1d73`, `35195d4`).

---

### Decision 3 RE-RATIFIED as a per-field rule (2026-07-26)

A **second** alignment review falsified the narrowed warning-path claim again — a third distinct unescaped print (`output.rs`'s own walker depth-limit warning) plus the discovery that the round-1 `lint.rs` fix used HUMAN mode, so a newline in the rule name still forged standalone status lines. Two rounds of "fix the enumerated sites" had each been correct and each been superseded.

The owner therefore **re-ratified Decision 3** in a stronger, per-field form, **superseding the earlier "wire mode at exactly four boundaries" enumeration**:

> **Untrusted identifiers and filenames are WIRE-escaped on every surface, human output included. Prose (diagnostic message / help bodies) stays HUMAN so multi-line frames keep rendering.**

Rationale: a filename or a config key is never legitimately multi-line, so preserving `\n` in one only enables status-line forgery (CWE-117); a diagnostic body legitimately *is* multi-line. This makes each remaining site decidable **by rule** rather than by re-deriving a list of boundaries. Recorded normatively in `spec.md` §7.5 and in the `crates/mds-core/src/lint/diagnostic.rs` module doc.

### Systemic guard approved and landed (2026-07-26)

The owner also approved a **systemic guard** as the deliverable that ends the whack-a-mole: `crates/mds-cli/tests/print_discipline.rs` fails CI if any print macro under `crates/mds-cli/src/**` interpolates a value that is not passed through one of the escape helpers, and applies the same rule to `format!` invocations nested inside `eprint_warning` calls. Exceptions live in an explicit allowlist with a written justification per entry; a companion test fails if an allowlist entry stops matching.

Consequences recorded here because they change earlier decisions:

- **`watch.rs` is no longer carved out.** Previously documented as a pre-existing gap outside #176's diff, its lifecycle status lines are now routed through `safe_path` / `safe_inline` / `eprint_warning`. Allowlisting them would have been a deliberate hole in the guard.
- **`eprint_warning` alone is explicitly not sufficient**, and is no longer documented as if it were. The boundary table now records its row as *prose HUMAN, interpolated identifiers/paths WIRE*.
- **Known residual, deliberately not claimed closed:** CLI `miette::miette!(…)` message construction. Those reports are HUMAN-escaped at `eprint_error` before miette renders them, so no raw control byte reaches stderr, but a `\n` in an interpolated path survives inside the rendered (indented, box-drawn) frame.

**Round-2 implementing commits:** see the branch log for `fix/esc-injection-176` after `35195d4`.

### Round 3 — the guard hardened, the normative claim narrowed (2026-07-26)

A **third** adversarial alignment review attacked the guard itself rather than the
prints, and falsified the spec's central claim. Both are closed here; this is intended
as the final code round.

**B1 (critical) — hoisting `format!` into a `let` defeated the guard.** `collect_sites`
looked for `format!` only where it appeared lexically inside the `eprint_warning(...)`
parens, so `let msg = format!("... {name}"); eprint_warning(&msg);` — completely
idiomatic — reintroduced M2 verbatim and invisibly. **B2 — `eprint_warning(<bare
identifier>)` was unchecked**, with five live instances (`build.rs:711`, `build.rs:1166`,
`main.rs:279`, `main.rs:288`, `main.rs:341`).

Fixed together. The helper's argument is now classified in its own right: a string
literal, a whole-expression sanitizer call, or a `format!` whose interpolations are each
accepted. A bare local is traced **one hop** through its `let` binding in the same file
and judged by the same rule. Anything unresolved is **reported, not trusted** — the
guard fails closed. Disposition of the five bare-`w` sites: **allowlisted with written
justification** in a new `ALLOWED_UNTRACED_HELPER_ARGS`, kept separate from the general
allowlist so the exemption applies *only* in the helper-argument position. The
justification states plainly what the reviewer observed: their safety rests on mds-core
producer discipline (`resolver.rs`, `evaluator.rs` WIRE-escape at construction), which
this lexical guard cannot verify across a crate boundary. Nothing mechanical holds it;
that is now written down rather than implied.

**B3 — `is_sanitizer_call` accepted postfix continuations** (`safe_path(p) + &evil`,
`safe_path(p).replace("a", &evil)`), contradicting the guard's own self-test, which
asserted the property but tested only the prefix direction. The call must now be the
whole expression; the self-test covers both directions and asserts strictly more than
before. **B4 — `write!` / `writeln!` to a stream handle was unscanned** (latent: zero
instances in the crate). Now scanned when the first argument names stdout/stderr,
including through a `let` binding.

**B5 / B6 accepted as limits, not chased**, and stated in the guard's own rustdoc under
"Accepted limits": sanitizers are matched by the last path segment (an alias, or a local
`fn safe_path`, defeats it); allowlist entries are **anti-rot, not anti-reuse** (keyed by
`(file, expression)`, so a future variable reusing an exempted name in the same file
inherits the exemption). Added alongside them: the trace is one hop within one file, and
stream detection is by name. The rustdoc now says outright that this is a lexical
scanner whose bar is *accidental* reintroduction, and that closing the remaining gaps
would need a rustc lint or a `syn`-based HIR analysis.

Each of B1-B4 was proven by **injecting the bypass into real source**, confirming the
guard fails naming the exact site, then reverting.

### Normative claim narrowed — option (b), with the mds-core residual named

spec 7.5 asserted filenames, paths and causes are "**WIRE everywhere**". Falsified:
mds-core `MdsError` message bodies interpolate exactly those values and stay HUMAN on
terminal surfaces (`fs.rs:478` `cannot read {normalized}: {e}`, `:487` invalid-UTF-8,
`parser_helpers.rs:853` `invalid import alias: '{alias}'`). The declared residual had
also been scoped to *CLI `miette!()` construction* only, understating it — the same
defect exists at a second construction site of equal severity.

**Chose (b) — narrow the claim — over (a) — make it true.** The blast radius of (a) is
not small: 110+ `MdsError::*(format!(...))` construction sites across `parser_helpers.rs`,
`evaluator.rs`, `resolver.rs`, `builtins.rs`, `fs.rs` and `lib.rs`, changing the public
`MdsError` message text seen by all three binding layers (a further breaking wire change
beyond what is already declared) and churning goldens in four suites. Decisively: fixing
only the two sites the reviewer named would leave the claim false at ~100 others — the
exact overclaim that reopened this issue twice already.

The rule is therefore stated per FIELD, precisely: a path in a `file` **field** (CLI
status line, `[file:line:col]` header, JSON `file` key) is WIRE on every surface; a path
or identifier interpolated into a message **body** is prose and follows the message row.
The residual is named in all four places — a new "Residual: paths and identifiers inside
a message body" section in spec 7.5, the boundary table in
`crates/mds-core/src/lint/diagnostic.rs`, a "Declared residual" paragraph in the
CHANGELOG, and this ledger. The reviewer's characterization is preserved: frame content
is indented and box-prefixed, and the prefix survives `strip()`, so it cannot masquerade
as a bare status line — the surface is genuinely weaker, only its scope was understated.

**Documentation overclaims corrected:** "All serialization and diagnostic-render
boundaries are now hardened" (closed-set-by-enumeration, in the file that retires
enumeration) is now an audit list; "Human-render output is unchanged" is scoped to
diagnostic prose; "sanitizes renderer inputs byte-length-preservingly" is scoped to
source text (`message` / `help` are escaped to `\uXXXX` literals, not length-preserved);
"all five surfaces" corrected to four; two bidi rustdoc lists that omitted U+061C (11 of
12) completed; and the escape class, previously defined as "C0 except `\n` / `\t`" in
`diagnostic.rs` against the spec's "`\n` in class, `\t` sole exemption", now reads the
same in both — `\n` is in the class, and HUMAN/WIRE is the mode choice, not a class
difference.

**Round-3 implementing commits:** `0e79385` (guard hardening), `c973331` (normative
claim + overclaims).

## Blocked
| Issue | File:Line | Blocker |
|-------|-----------|---------|

(none)
