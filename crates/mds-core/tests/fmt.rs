//! Integration tests for the `mds fmt` engine (`format_str` / `format_str_with`).
//!
//! MDS is a content language: the formatter MUST produce byte-identical compile
//! output (`compile(fmt(src)).output == compile(src).output`) and MUST be
//! idempotent (`fmt(fmt(src)) == fmt(src)`). These tests verify that invariant
//! across a small corpus plus rule-specific and edge-case scenarios.
//!
//! Several exact whitespace-collapsing thresholds below were verified empirically
//! against the live `clean_output` compiler pass (not just inferred from prose),
//! per the plan's own instruction to verify claims against live code. See the
//! `r3_*` and `r4_*` tests and their comments for the specific behavior locked in.

use mds::{format_str, format_str_with, MdsError};

// ── Small corpus of representative, syntactically-valid MDS snippets ─────────
//
// Every entry here is known to tokenize AND compile without runtime vars or
// imports (self-contained). Import/base_dir scenarios get their own dedicated
// tests below (they need a real tempdir).
const CORPUS: &[&str] = &[
    "Hello world!\n",
    "---\nname: World\n---\nHello {name}!\n",
    "---\npremium: true\n---\n@if premium:\nThanks!\n@end\n",
    "---\nitems: [a, b, c]\n---\n@for item in items:\n- {item}\n@end\n",
    "```python\ndef f():\n    pass\n```\n",
    "@define greet(x):\nHello {x}!\n@end\n{greet(\"World\")}\n",
    "@message user:\nHi!\n@end\n",
    "Line with \\{escaped\\} braces.\n",
    "",
    "   \n",
    "No trailing newline",
    "---\nname: x\n---\n",
    "---\npremium: true\n---\nBefore\n\n\n\nAfter @if premium: inline text {premium}\n@end\n",
];

fn compiled_markdown(src: &str) -> Option<String> {
    mds::compile_str(src)
        .ok()
        .and_then(|r| r.into_markdown().ok())
}

// ── AC-EF-1: idempotence ──────────────────────────────────────────────────────

#[test]
fn idempotent_across_corpus() {
    for src in CORPUS {
        let once = format_str(src).unwrap_or_else(|e| panic!("format_str failed for {src:?}: {e}"));
        let twice = format_str(&once)
            .unwrap_or_else(|e| panic!("format_str (2nd pass) failed for {once:?}: {e}"));
        assert_eq!(once, twice, "not idempotent for input: {src:?}");
    }
}

// ── AC-EF-2: compile-equivalence ──────────────────────────────────────────────

#[test]
fn compile_equivalent_across_corpus() {
    for src in CORPUS {
        let Some(orig_md) = compiled_markdown(src) else {
            // Skip entries that don't compile standalone (none currently, but
            // keeps this test robust if the corpus grows to include one).
            continue;
        };
        let formatted = format_str(src).expect("format_str should succeed for corpus entry");
        let formatted_md = compiled_markdown(&formatted).unwrap_or_else(|| {
            panic!("formatted output of {src:?} failed to compile: {formatted:?}")
        });
        assert_eq!(
            orig_md, formatted_md,
            "compile output changed for input: {src:?}\nformatted: {formatted:?}"
        );
    }
}

// ── API surface (also pinned in api_surface.rs) ───────────────────────────────

#[test]
fn format_str_returns_result_string_mdserror() {
    let _: Result<String, MdsError> = format_str("Hello!\n");
}

#[test]
fn format_str_with_accepts_optional_base_dir() {
    let _: Result<String, MdsError> = format_str_with("Hello!\n", None);
}

// ── R1: CRLF/CR removal, including inside protected regions ─────────────────

#[test]
fn r1_crlf_converted_to_lf_everywhere_including_frontmatter_and_fences() {
    let src = "---\r\nname: x\r\n---\r\nHello\r\n```\r\ncode\r\n```\r\n";
    let out = format_str(src).expect("should format");
    assert!(
        !out.contains('\r'),
        "no \\r should survive formatting: {out:?}"
    );
    assert_eq!(out, "---\nname: x\n---\nHello\n```\ncode\n```\n");
}

#[test]
fn r1_bare_cr_without_lf_is_removed() {
    // A bare \r (no following \n) must also be removed, matching clean_output's
    // unconditional \r-skip (verified: clean_output deletes every \r it sees,
    // \n-paired or not).
    let src = "a\rb\n";
    let out = format_str(src).expect("should format");
    assert!(!out.contains('\r'));
}

// ── R2: exactly one final newline; empty/whitespace-only -> empty ───────────

#[test]
fn r2_empty_input_yields_empty_output() {
    assert_eq!(format_str("").unwrap(), "");
}

#[test]
fn r2_whitespace_only_input_yields_empty_output() {
    // Matches clean_output("   \n") == "" (verified empirically against the
    // live compiler: `printf '   \n' | mds build -` produces zero bytes).
    assert_eq!(format_str("   \n").unwrap(), "");
    assert_eq!(format_str("\n\n\n").unwrap(), "");
    assert_eq!(format_str("   \n\n\n").unwrap(), "");
}

#[test]
fn r2_missing_trailing_newline_is_added() {
    assert_eq!(format_str("Hello").unwrap(), "Hello\n");
}

#[test]
fn r2_excess_trailing_newlines_collapse_to_one() {
    assert_eq!(format_str("Hello\n\n\n").unwrap(), "Hello\n");
}

#[test]
fn r2_frontmatter_only_no_trailing_newline_still_gets_one() {
    // Edge case: source ends exactly at the closing frontmatter fence with no
    // trailing newline. The overall document must still end in exactly one \n.
    let out = format_str("---\nname: x\n---").expect("should format");
    assert!(
        out.ends_with('\n'),
        "expected trailing newline, got: {out:?}"
    );
}

// ── R3: 3+ blank lines collapse to one, except inside fences ────────────────

#[test]
fn r3_blank_line_run_collapses_matching_clean_output() {
    // Verified empirically: clean_output("Hello\n\n\n\nWorld\n") == "Hello\n\nWorld\n".
    let out = format_str("Hello\n\n\n\nWorld\n").unwrap();
    assert_eq!(out, "Hello\n\nWorld\n");
}

#[test]
fn r3_single_blank_line_unchanged() {
    let out = format_str("Hello\n\nWorld\n").unwrap();
    assert_eq!(out, "Hello\n\nWorld\n");
}

#[test]
fn r3_never_collapses_blank_lines_inside_code_fence() {
    let src = "```\ncode\n\n\n\nmore\n```\nAfter\n";
    let out = format_str(src).expect("should format");
    assert!(
        out.contains("code\n\n\n\nmore"),
        "blank lines inside a code fence must be preserved verbatim, got: {out:?}"
    );
}

#[test]
fn r3_leading_blank_lines_in_body_fully_stripped() {
    // Verified empirically: clean_output of a body with leading blank lines
    // removes them ENTIRELY (not capped at one) -- both with and without
    // frontmatter present.
    let out = format_str("\n\n\nHello\nWorld\n").unwrap();
    assert_eq!(out, "Hello\nWorld\n");

    let out_fm = format_str("---\nname: x\n---\n\n\n\nHello\n").unwrap();
    assert_eq!(out_fm, "---\nname: x\n---\nHello\n");
}

// ── R4: trailing whitespace on directive lines; body content UNCHANGED ──────

#[test]
fn r4_directive_trailing_whitespace_stripped() {
    let src = "---\npremium: true\n---\n@if premium:   \nThanks!\n@end\t\n";
    let out = format_str(src).expect("should format");
    assert!(
        out.contains("@if premium:\n"),
        "directive trailing spaces should be stripped, got: {out:?}"
    );
    assert!(
        out.contains("@end\n") && !out.contains("@end\t"),
        "directive trailing tab should be stripped, got: {out:?}"
    );
}

#[test]
fn r4_headline_body_content_trailing_spaces_survive_markdown_hard_break() {
    // THE canonical case from the plan: two trailing spaces on a body-content
    // line form a Markdown hard line break and MUST survive formatting
    // byte-for-byte, unlike a directive line's trailing whitespace.
    let src = "line  \nnext\n";
    let out = format_str(src).expect("should format");
    assert_eq!(
        out, "line  \nnext\n",
        "trailing spaces on content lines must be preserved"
    );
}

#[test]
fn r4_whitespace_only_line_in_middle_of_document_is_preserved_verbatim() {
    // DELIBERATE, VERIFIED DEVIATION from a literal "strip all blank-line
    // whitespace" reading of R4: empirically, clean_output does NOT treat a
    // whitespace-only line as inert when it sits between two content lines --
    // `printf 'Hello\n   \nWorld\n' | mds build -` preserves the 3 spaces
    // byte-for-byte (clean_output's per-char loop only collapses truly-EMPTY
    // \n runs; a space character resets its counter and is pushed verbatim,
    // and clean_output's final trim_end() only touches the absolute end of
    // the document, never the middle). Stripping this line would therefore
    // change compiled output and violate AC-EF-2, which the plan states is
    // the overriding, non-negotiable constraint. So this formatter leaves an
    // isolated whitespace-only "blank" line completely untouched. The
    // `format_str_with('   \n', ...)` whole-document case is still covered by
    // R2 above (whitespace-only whole input -> empty).
    let src = "Hello\n   \nWorld\n";
    let out = format_str(src).expect("should format");
    assert_eq!(
        out, src,
        "an isolated whitespace-only line must not be touched (see comment)"
    );
    // Compile-equivalence must hold regardless.
    let compiled_before = mds::compile_str(src).unwrap().into_markdown().unwrap();
    let compiled_after = mds::compile_str(&out).unwrap().into_markdown().unwrap();
    assert_eq!(compiled_before, compiled_after);
}

#[test]
fn r4_trailing_tabs_on_directive_and_blank_lines_stripped_body_tabs_preserved() {
    let src = "---\npremium: true\n---\n@if premium:\t\t\nkeep\ttab\there\n@end\n";
    let out = format_str(src).expect("should format");
    assert!(out.contains("@if premium:\n"), "got: {out:?}");
    assert!(
        out.contains("keep\ttab\there\n"),
        "body-content tabs must be preserved, got: {out:?}"
    );
}

// ── R7: protected regions byte-identical after format except \r removal ─────

#[test]
fn r7_frontmatter_content_byte_identical_mod_cr() {
    let src = "---\nname: World\nimports:\n  - path: ./lib.mds\ntype: mds\n---\nHello {name}!\n";
    let out = format_str(src).expect("should format");
    assert!(
        out.starts_with("---\nname: World\nimports:\n  - path: ./lib.mds\ntype: mds\n---\n"),
        "frontmatter must be byte-identical (not reformatted/stripped), got: {out:?}"
    );
}

#[test]
fn r7_code_fence_content_byte_identical_mod_cr() {
    let src = "```python\ndef f( x ):\n    return   x\n```\n";
    let out = format_str(src).expect("should format");
    assert_eq!(out, src, "code fence content must be byte-identical");
}

// ── AC-EF-8: syntax errors surfaced, never a garbled string ─────────────────

#[test]
fn syntax_error_unclosed_code_fence_is_err() {
    let err = format_str("```\ncode without a close\n").unwrap_err();
    assert!(matches!(err, MdsError::Syntax { .. }));
}

#[test]
fn syntax_error_unclosed_interpolation_is_err() {
    let err = format_str("Hello {name\n").unwrap_err();
    assert!(matches!(err, MdsError::Syntax { .. }));
}

#[test]
fn syntax_error_unclosed_frontmatter_is_err() {
    let err = format_str("---\nname: x\nHello\n").unwrap_err();
    assert!(matches!(err, MdsError::Syntax { .. }));
}

// ── T1-gate-fallback: undefined-var source still formats; import source uses full gate ──

#[test]
fn gate_fallback_undefined_var_source_still_formats() {
    // `mds::compile_str` fails (undefined variable), so format_str_with must
    // fall back to the structural check rather than hard-failing.
    let src = "Hello {undefined_var}!\n\n\n\nBye.\n";
    assert!(
        mds::compile_str(src).is_err(),
        "sanity: source must not compile standalone"
    );
    let out = format_str(src).expect("format_str must still succeed via structural fallback");
    assert_eq!(out, "Hello {undefined_var}!\n\nBye.\n");
}

#[test]
fn gate_full_check_runs_when_base_dir_resolves_imports() {
    let dir = tempfile::tempdir().unwrap();
    let lib_path = dir.path().join("lib.mds");
    std::fs::write(&lib_path, "@define greet(x):\nHello {x}!\n@end\n").unwrap();

    let src = "@import \"./lib.mds\"\n{greet(\"World\")}\n\n\n\nBye.\n";
    let out =
        format_str_with(src, Some(dir.path())).expect("format_str_with should resolve imports");
    assert_eq!(out, "@import \"./lib.mds\"\n{greet(\"World\")}\n\nBye.\n");

    // Compile-equivalence via the REAL gate: compile both with the same base_dir.
    let before = mds::compile_str_with(src, Some(dir.path()), None)
        .unwrap()
        .into_markdown()
        .unwrap();
    let after = mds::compile_str_with(&out, Some(dir.path()), None)
        .unwrap()
        .into_markdown()
        .unwrap();
    assert_eq!(before, after);
}

// ── Edge cases (EC-1..EC-8ish; EC-9 partial/parent lives in cli_fmt.rs) ──────

#[test]
fn ec_empty_and_whitespace_only_input_to_empty_output() {
    assert_eq!(format_str("").unwrap(), "");
    assert_eq!(format_str("   ").unwrap(), "");
    assert_eq!(format_str("\t\t").unwrap(), "");
}

#[test]
fn ec_missing_trailing_newline_added() {
    assert_eq!(
        format_str("no newline at all").unwrap(),
        "no newline at all\n"
    );
}

#[test]
fn ec_frontmatter_only_file() {
    let out = format_str("---\nname: World\n---\n").unwrap();
    assert_eq!(out, "---\nname: World\n---\n");
}

#[test]
fn ec_file_ending_mid_directive_at_eof_no_trailing_newline() {
    let src = "---\npremium: true\n---\n@if premium:\nBody\n@end";
    let out = format_str(src).expect("should format despite no trailing newline");
    assert!(out.ends_with('\n'));
    assert!(out.ends_with("@end\n"));
}

#[test]
fn ec_unclosed_fence_interp_frontmatter_all_error() {
    assert!(format_str("```\nunclosed").is_err());
    assert!(format_str("{unclosed").is_err());
    assert!(format_str("---\nunclosed").is_err());
}

#[test]
fn ec_unclosed_input_is_not_written_by_caller_contract() {
    // format_str returning Err (not a garbled Ok(String)) is the contract the
    // CLI relies on to avoid ever writing a corrupted file.
    match format_str("```\nunclosed") {
        Err(MdsError::Syntax { .. }) => {}
        other => panic!("expected Syntax error, got: {other:?}"),
    }
}

#[test]
fn ec_utf8_bom_before_frontmatter_fence_left_verbatim() {
    // A UTF-8 BOM before `---` means the source does NOT start with the exact
    // byte sequence "---\n"/"---\r\n" the lexer requires, so frontmatter is
    // never recognized -- consistent with the compiler. The whole thing
    // (BOM included) is ordinary body text.
    let src = "\u{FEFF}---\nname: x\n---\nHello\n";
    let out = format_str(src).expect("should format");
    assert!(
        out.starts_with('\u{FEFF}'),
        "BOM must be preserved verbatim, got: {out:?}"
    );
    // Must remain compile-equivalent (BOM-prefixed source is not treated as
    // frontmatter by the compiler either).
    let before = mds::compile_str(src).unwrap().into_markdown().unwrap();
    let after = mds::compile_str(&out).unwrap().into_markdown().unwrap();
    assert_eq!(before, after);
}

#[test]
fn ec_multibyte_utf8_byte_offset_correctness() {
    let src = "---\npremium: true\n---\n@if premium:\nCafé 日本語 emoji: \u{1F600}\n@end\n\n\n\nBye \u{1F600}\n";
    let out = format_str(src).expect("should format without panicking on multi-byte offsets");
    assert!(out.contains("Café 日本語 emoji: \u{1F600}"));
    assert!(out.contains("Bye \u{1F600}\n"));
    let before = mds::compile_str(src).unwrap().into_markdown().unwrap();
    let after = mds::compile_str(&out).unwrap().into_markdown().unwrap();
    assert_eq!(before, after);
}

#[test]
fn ec_mixed_crlf_lf_all_become_lf() {
    let src = "Hello\r\nWorld\nAgain\r\n";
    let out = format_str(src).unwrap();
    assert_eq!(out, "Hello\nWorld\nAgain\n");
}

#[test]
fn ec_blank_line_collapse_inside_define_message_block_bodies_is_safe() {
    // R3 collapsing is safe to apply inside a standalone @block body (still
    // ordinary markdown-mode content, subject to the normal clean_output
    // pass) but is DELIBERATELY NOT applied inside @message or @define
    // bodies. Verified empirically against the live evaluator/compiler (not
    // just inferred): `@message` content is built via
    // `evaluate_nodes(...).trim()` with NO clean_output pass, so an
    // uncollapsed blank-line run survives verbatim in the compiled message
    // JSON (`@message user:\nHi\n\n\n\nthere\n@end\n` compiles to content
    // `"Hi\n\n\n\nthere"`, all 4 raw newlines intact). `@define` bodies get
    // the same conservative treatment because a function's body can be
    // called from EITHER a markdown-mode site (clean_output applies) or a
    // @message-mode site (it does not) -- the formatter can't tell which
    // without a full call-graph analysis, so it never collapses inside one.
    // In all three cases the direct compile-equivalence assertion below is
    // the thing that actually matters; the "contains" checks document
    // exactly what the formatter chose to do (and, for the define/message
    // cases, that it correctly chose NOT to touch the blank-line run).

    // @define body: collapsing must NOT happen (conservative exemption).
    let define_src = "@define greet(x):\nHello\n\n\n\n{x}!\n@end\n{greet(\"World\")}\n";
    let define_out = format_str(define_src).expect("should format");
    assert!(
        define_out.contains("Hello\n\n\n\n{x}!"),
        "blank-line run inside @define body must NOT collapse, got: {define_out:?}"
    );
    let define_before = mds::compile_str(define_src).unwrap();
    let define_after = mds::compile_str(&define_out).unwrap();
    assert_eq!(define_before.output, define_after.output);

    // @message body: collapsing must NOT happen (verified unsafe above).
    let message_src = "@message user:\nHi\n\n\n\nthere\n@end\n";
    let message_out = format_str(message_src).expect("should format");
    assert!(
        message_out.contains("Hi\n\n\n\nthere"),
        "blank-line run inside @message body must NOT collapse, got: {message_out:?}"
    );
    let message_before = mds::compile_str(message_src).unwrap();
    let message_after = mds::compile_str(&message_out).unwrap();
    assert_eq!(message_before.output, message_after.output);

    // @block body (standalone mode: renders its default body inline).
    let block_src = "@block instructions:\nStep one\n\n\n\nStep two\n@end\n";
    let block_out = format_str(block_src).expect("should format");
    assert!(
        block_out.contains("Step one\n\nStep two"),
        "blank-line run inside @block body should collapse, got: {block_out:?}"
    );
    let block_before = mds::compile_str(block_src).unwrap();
    let block_after = mds::compile_str(&block_out).unwrap();
    assert_eq!(block_before.output, block_after.output);
}

#[test]
fn message_body_carriage_return_is_preserved_not_stripped() {
    // Verified empirically: `@message` content bypasses clean_output's \r
    // removal entirely -- a \r inside a message body reaches the compiled
    // JSON verbatim (as the literal two-character sequence \r\n in the
    // JSON-escaped string). The formatter must therefore NOT apply R1
    // inside a message body, unlike everywhere else in the document.
    let src = "@message user:\r\nHi\r\nthere\r\n@end\r\n";
    let out = format_str(src).expect("should format");
    assert!(
        out.contains("Hi\r\nthere"),
        "\\r inside a @message body must be preserved verbatim, got: {out:?}"
    );
    let before = mds::compile_str(src).unwrap();
    let after = mds::compile_str(&out).unwrap();
    assert_eq!(before.output, after.output);
}

#[test]
fn code_fence_nested_inside_message_body_is_fully_raw_not_just_r1_protected() {
    // A code fence nested inside a @message body is doubly-exempt: it would
    // normally be "protected" (R1 \r-strip applies, R3 doesn't), but because
    // its content flows into the message's raw (non-clean_output'd) string,
    // even R1's \r-strip must not apply to it here. This locks in the
    // priority ordering in `rewrite_body` (raw-content is checked before
    // protected).
    let src = "@message user:\n```\r\ncode\r\n```\r\n@end\r\n";
    let out = format_str(src).expect("should format");
    assert!(
        out.contains("```\r\ncode\r\n```\r\n"),
        "\\r inside a fence nested in a @message body must be preserved verbatim, got: {out:?}"
    );
    let before = mds::compile_str(src).unwrap();
    let after = mds::compile_str(&out).unwrap();
    assert_eq!(before.output, after.output);
}

// ── Idempotency for malformed/degenerate inputs must still error consistently ─

#[test]
fn idempotency_not_claimed_for_non_tokenizing_input() {
    // Not a formal AC, but documents the boundary: malformed input errors both
    // times rather than being "fixed" by a first pass.
    assert!(format_str("```\nunclosed").is_err());
}

// ── Perf: linear time on a large mixed file (AC-PERF-1) ──────────────────────

#[test]
fn perf_linear_one_megabyte_file_formats_under_two_seconds() {
    let mut src = String::from("---\npremium: true\n---\n");
    // Mix directives, prose, and a code block, repeated to reach ~1MB.
    let chunk = "@if premium:\nSome prose line with plain text.\n@end\n\n\n\n```text\nblock\n```\n";
    while src.len() < 1_000_000 {
        src.push_str(chunk);
    }
    src.push_str("Final line.\n");

    let start = std::time::Instant::now();
    let out = format_str(&src).expect("should format a large file");
    let elapsed = start.elapsed();

    assert!(!out.is_empty());
    assert!(
        elapsed.as_secs_f64() < 2.0,
        "formatting ~1MB took too long: {elapsed:?}"
    );
}

// ── Runtime-vars-independent: format_str_with never needs runtime vars ───────

#[test]
fn format_str_with_none_base_dir_matches_format_str() {
    for src in CORPUS {
        let a = format_str(src);
        let b = format_str_with(src, None);
        match (a, b) {
            (Ok(a), Ok(b)) => assert_eq!(a, b, "format_str/format_str_with diverged for {src:?}"),
            (Err(_), Err(_)) => {}
            (a, b) => panic!(
                "format_str and format_str_with disagreed on Ok/Err for {src:?}: {a:?} vs {b:?}"
            ),
        }
    }
}
