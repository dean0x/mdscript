//! Rule: `unused-import`
//!
//! **Severity**: Warn (default) | **Tier**: B (report-only in practice — no
//! `fix_removals` wired; a file with imports is never structural-standalone)
//!
//! An import that is never used in the module body wastes the resolver's work
//! (and in partial-eval contexts, the loading of an external file).
//!
//! ## Per-form semantics (Appendix A)
//!
//! ### Alias import (`@import "path" as alias`)
//!
//! Used when `alias` appears as:
//! - `Expr::QualifiedCall { namespace: alias, .. }` — `{alias.func(...)}`
//! - `IncludeDirective { alias }` — `@include alias`
//!
//! ### Selective import (`@import { name1, name2 } from "path"`)
//!
//! Each name is checked individually (better UX). A name is used when it appears as:
//! - `Expr::Call { name, .. }` — `{name(...)}`
//! - `Arg::Call { name, .. }` — `func(name(...))`
//! - `Arg::Var(name)` — `func(name)` (passing a function ref as argument)
//! - `Expr::Var(name)` — `{name}` (using the imported name as a variable)
//!
//! ### Merge import (`@import "path"`)
//!
//! **Always treated as used** (conservative). A merge import injects all the
//! imported module's exports plus the `prompt` variable into scope — tracking
//! which injected symbols are actually used would require cross-file analysis,
//! which is out of scope for v1.
//!
//! ### Re-export exemption
//!
//! A selective import name that appears in a `@export name` or
//! `@export name from "path"` directive is considered "used" even without a
//! call-site reference (the module re-exports it).
//!
//! ### Frontmatter `imports:` key
//!
//! The frontmatter `imports:` YAML key is not tracked as AST import nodes —
//! it is handled by the resolver outside the AST. This lint rule does not flag
//! frontmatter imports in v1 (document in rule doc comment).
//!
//! ## Suppression on partials/@extends
//!
//! Suppressed when `ctx.is_partial_or_extends` is true.

use crate::ast::Module;
use crate::error::SerializedSpan;
use crate::lint::config::LintConfig;
use crate::lint::diagnostic::{LintDiagnostic, LintResultBuilder, Severity};
use crate::lint::facts::{AnalysisContext, ExportKind, ImportKind};

pub(crate) const RULE: &str = "unused-import";

/// Check the module for unused imports.
pub(crate) fn check(
    _module: &Module,
    ctx: &AnalysisContext,
    filename: &str,
    config: &LintConfig,
    builder: &mut LintResultBuilder,
) {
    let severity = resolve_severity(config);
    if severity == Severity::Off {
        return;
    }

    // Suppressed on partials / @extends children.
    if ctx.is_partial_or_extends {
        return;
    }

    // Build re-export set: names that are re-exported (exemption for selective imports).
    // Wildcard re-exports (@export * from ...) are intentionally NOT in this exemption set:
    // a wildcard re-export operates on the imported module's own exports, not on a local
    // import binding — a file with @import + @export * re-exports from different paths and
    // there is no syntactic link between them at the name level. Adding wildcard to this
    // set would require cross-file resolution that is deliberately out of scope for v1
    // (adjudicated more-correct than Appendix A's literal wording).
    let reexport_names: std::collections::HashSet<String> = ctx
        .exports
        .iter()
        .filter(|e| matches!(e.kind, ExportKind::Named | ExportKind::ReExport))
        .filter_map(|e| e.name.clone())
        .collect();

    for imp in &ctx.imports {
        match imp.kind {
            ImportKind::Merge => {
                // Always treated as used (conservative — cross-file analysis needed).
                continue;
            }
            ImportKind::Alias => {
                let alias = imp.alias.as_deref().unwrap_or("");
                let is_used =
                    ctx.used_namespaces.contains(alias) || ctx.used_include_aliases.contains(alias);
                if !is_used
                    && !builder.push(make_diag(
                        severity,
                        filename,
                        format!(
                            "Import alias '{}' from '{}' is never used.",
                            alias, imp.path
                        ),
                        Some(
                            "Remove the @import or use the alias with @include or as \
                             a qualified call (`alias.func(...)`)."
                                .to_string(),
                        ),
                        imp.offset,
                        "@import".len(),
                    ))
                {
                    return;
                }
            }
            ImportKind::Selective => {
                // AD-203-3 / PF-005: `name_offsets` is index-aligned with `names` by
                // construction — `parse_import_directive` builds both in one pass and
                // carries a `debug_assert_eq!` on their lengths at that site, which is
                // where a regression would originate. Per PF-005 that assertion is dev
                // feedback only; it compiles away in release. The real guard is the
                // unconditional fallback below, which is why it is written as a total
                // match rather than an `expect`.
                //
                // Deliberately NOT duplicated as a `debug_assert!` here: it would abort
                // debug builds before the fallback could run, making the degradation
                // path (AC-P1-19) untestable under `cargo test`.
                //
                // Per-name flagging: each name checked individually.
                // AD-203-1 / PF-012: anchor the span at the name, not @import.
                for (i, name) in imp.names.iter().enumerate() {
                    let is_used = ctx.used_calls.contains(name)
                        || ctx.used_vars.contains(name)
                        || reexport_names.contains(name);
                    if !is_used {
                        // AD-203-3: prefer the per-name offset. If `name_offsets` is
                        // unexpectedly short, degrade to the WHOLE `@import` keyword
                        // span — offset AND length together. Falling back on the
                        // offset alone while keeping `name.len()` would highlight an
                        // arbitrary prefix of `@import`, which is exactly the
                        // in-bounds-but-wrong span PF-012 warns about.
                        let (name_offset, name_length) = match imp.name_offsets.get(i) {
                            Some(&off) => (off, name.len()),
                            None => (imp.offset, "@import".len()),
                        };
                        if !builder.push(make_diag(
                            severity,
                            filename,
                            format!(
                                "Imported name '{}' from '{}' is never used.",
                                name, imp.path
                            ),
                            Some(format!(
                                "Remove '{}' from the selective import or use it in the body.",
                                name
                            )),
                            name_offset,
                            name_length,
                        )) {
                            return;
                        }
                    }
                }
            }
        }
    }
}

fn resolve_severity(config: &LintConfig) -> Severity {
    config.severity_for(RULE).copied().unwrap_or(Severity::Warn)
}

/// Build an unused-import diagnostic.
///
/// `offset` is the byte position of the span anchor within the source.
/// `length` is the byte length of the highlighted token.
///
/// For Alias and Merge forms the caller passes `imp.offset` /
/// `"@import".len()` so the span covers the `@import` keyword.
///
/// For Selective forms the caller passes the per-name offset from
/// `imp.name_offsets` and `name.len()` so the span covers the unused name
/// (AD-203-1 / PF-012).
fn make_diag(
    severity: Severity,
    filename: &str,
    message: String,
    help: Option<String>,
    offset: usize,
    length: usize,
) -> LintDiagnostic {
    LintDiagnostic {
        rule: RULE.to_string(),
        severity,
        message,
        help,
        span: Some(SerializedSpan {
            offset,
            length,
            line: None,
            column: None,
        }),
        file: Some(filename.to_string()),
        fix_removals: None, // unused-import is report-only (partial-name removal unsafe)
        fix_edits: None,
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::lint::facts::collect_facts;
    use crate::parser::parse_with_ctx;

    fn lint_src(src: &str) -> Vec<LintDiagnostic> {
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "test.mds", src).unwrap();
        let ctx = collect_facts(&module, false, src).unwrap();
        let mut builder = LintResultBuilder::new();
        check(
            &module,
            &ctx,
            "test.mds",
            &LintConfig::default(),
            &mut builder,
        );
        builder.build(false).diagnostics
    }

    /// L-U-UI1: Unused alias import fires.
    #[test]
    fn unused_alias_import_fires() {
        let src = "@import \"./lib.mds\" as lib\nHello!\n";
        let diags = lint_src(src);
        assert!(
            diags.iter().any(|d| d.rule == RULE),
            "should fire for unused alias import; got: {:?}",
            diags
        );
    }

    /// Used alias import (via qualified call) does not fire.
    #[test]
    fn used_alias_via_qualified_call_does_not_fire() {
        let src = "@import \"./lib.mds\" as lib\n{{lib.greet(\"world\")}}\n";
        let diags = lint_src(src);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "should not fire for alias used in QualifiedCall; got: {:?}",
            diags
        );
    }

    /// Used alias import (via @include) does not fire.
    #[test]
    fn used_alias_via_include_does_not_fire() {
        let src = "@import \"./lib.mds\" as lib\n@include lib\n";
        let diags = lint_src(src);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "should not fire for alias used in @include; got: {:?}",
            diags
        );
    }

    /// L-U-UI2: Unused selective import name fires per-name.
    #[test]
    fn unused_selective_import_fires() {
        let src = "@import { greet } from \"./lib.mds\"\nHello!\n";
        let diags = lint_src(src);
        assert!(
            diags
                .iter()
                .any(|d| d.rule == RULE && d.message.contains("greet")),
            "should fire for unused selective import 'greet'; got: {:?}",
            diags
        );
    }

    /// Used selective import name does not fire.
    #[test]
    fn used_selective_import_does_not_fire() {
        let src = "@import { greet } from \"./lib.mds\"\n{{greet(\"world\")}}\n";
        let diags = lint_src(src);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "should not fire for used selective import; got: {:?}",
            diags
        );
    }

    /// Merge import is always treated as used (conservative).
    #[test]
    fn merge_import_always_used() {
        let src = "@import \"./lib.mds\"\nHello!\n";
        let diags = lint_src(src);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "merge import should always be treated as used; got: {:?}",
            diags
        );
    }

    /// Re-export exemption: selective name that is re-exported is not flagged.
    #[test]
    fn selective_name_reexported_not_flagged() {
        let src = "@import { greet } from \"./lib.mds\"\n@export greet\n";
        let diags = lint_src(src);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "re-exported selective import should not be flagged; got: {:?}",
            diags
        );
    }

    /// Partial file suppresses unused-import.
    #[test]
    fn partial_suppresses_unused_import() {
        let src = "@import \"./lib.mds\" as lib\nHello!\n";
        let tokens = tokenize(src, "_partial.mds").unwrap();
        let module = parse_with_ctx(&tokens, "_partial.mds", src).unwrap();
        let ctx = collect_facts(&module, true, src).unwrap(); // is_partial=true
        let mut builder = LintResultBuilder::new();
        check(
            &module,
            &ctx,
            "_partial.mds",
            &LintConfig::default(),
            &mut builder,
        );
        assert!(
            builder.build(false).diagnostics.is_empty(),
            "partial should suppress unused-import"
        );
    }

    /// TEST-5: Wildcard re-export (`@export * from "path"`) does NOT exempt a
    /// selective import from the unused-import rule.
    ///
    /// The KB-adjudicated semantic: `@export * from "..."` operates on the imported
    /// module's own exports at a different namespace level — there is no syntactic
    /// link between the wildcard re-export and any local import binding. Only
    /// `@export name` (Named) and `@export name from "path"` (ReExport) suppress
    /// their matching local import binding.
    #[test]
    fn wildcard_reexport_does_not_exempt_unused_import() {
        // `greet` is selectively imported but never referenced in the body.
        // The wildcard re-export is from an unrelated path and carries no name
        // binding — it must NOT exempt `greet` from the unused-import finding.
        let src = "@import { greet } from \"./lib.mds\"\n@export * from \"./other.mds\"\n";
        let diags = lint_src(src);
        assert!(
            diags
                .iter()
                .any(|d| d.rule == RULE && d.message.contains("greet")),
            "wildcard re-export must NOT exempt a selective import from unused-import; \
             got: {:?}",
            diags
        );
    }

    /// Rule=off suppresses.
    #[test]
    fn rule_off_suppresses() {
        let src = "@import \"./lib.mds\" as lib\nHello!\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "test.mds", src).unwrap();
        let ctx = collect_facts(&module, false, src).unwrap();
        let mut builder = LintResultBuilder::new();
        let config = LintConfig {
            rules: [(RULE.to_string(), Severity::Off)].into_iter().collect(),
        };
        check(&module, &ctx, "test.mds", &config, &mut builder);
        assert!(builder.build(false).diagnostics.is_empty());
    }

    // ── AC-P1-14/15/16 / AD-203-1: span anchors at the unused name ───────────

    /// Extract every `unused-import` diagnostic as
    /// `(reported name, source slice under its span)`.
    ///
    /// **This is the AC-P1-14 positive control**, and it is deliberately stronger
    /// than an offset comparison: it re-reads `src` at the emitted span and hands
    /// back what a consumer would actually highlight. An offset that is in bounds
    /// but points at the wrong token — the PF-012 failure class this whole change
    /// risks introducing — produces a mismatching slice and fails. Asserting only
    /// that the offset moved, or that it is in bounds, would not.
    ///
    /// Panics if a span is missing or does not land on UTF-8 char boundaries, so a
    /// malformed span can never be silently skipped.
    fn unused_import_name_and_slice(src: &str) -> Vec<(String, String)> {
        lint_src(src)
            .iter()
            .filter(|d| d.rule == RULE)
            .map(|d| {
                let span = d
                    .span
                    .as_ref()
                    .unwrap_or_else(|| panic!("unused-import diagnostic must carry a span: {d:?}"));
                let end = span.offset + span.length;
                assert!(
                    end <= src.len()
                        && src.is_char_boundary(span.offset)
                        && src.is_char_boundary(end),
                    "span {}..{} is out of bounds or not on a char boundary for a \
                     {}-byte source",
                    span.offset,
                    end,
                    src.len()
                );
                // The message is `Imported name '<name>' from '<path>' is never used.`
                let name = d
                    .message
                    .split('\'')
                    .nth(1)
                    .expect("message must quote the name")
                    .to_string();
                (name, src[span.offset..end].to_string())
            })
            .collect()
    }

    /// Assert that every emitted `unused-import` span slices to exactly the name
    /// the diagnostic reports, and that the reported names are `expected`.
    ///
    /// The `expected` check is what keeps this non-vacuous: a source that produced
    /// zero diagnostics would otherwise satisfy "every span slices correctly"
    /// trivially (PF-013).
    fn assert_spans_slice_to_their_names(src: &str, expected: &[&str]) {
        let found = unused_import_name_and_slice(src);
        let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names, expected,
            "unexpected set/order of unused-import diagnostics for {src:?}"
        );
        for (name, slice) in &found {
            assert_eq!(
                slice, name,
                "AC-P1-14: the span for '{name}' must slice to '{name}', got {slice:?} \
                 — an in-bounds but mis-anchored span (PF-012). Source: {src:?}"
            );
        }
    }

    /// AC-P1-14: for a single unused name in a selective import, the span slices
    /// to the name, not to the `@import` keyword.
    #[test]
    fn selective_span_anchors_at_name_not_at_import_keyword() {
        let src = "@import { greet } from \"./lib.mds\"\nHello!\n";
        assert_spans_slice_to_their_names(src, &["greet"]);

        // Pin the concrete offset too, so a change that shifted every name by a
        // constant (and therefore still sliced to *some* name) is caught.
        let diags = lint_src(src);
        let span = diags
            .iter()
            .find(|d| d.rule == RULE)
            .and_then(|d| d.span.as_ref())
            .expect("unused-import diagnostic for 'greet' must fire with a span");
        assert_eq!(span.offset, "@import { ".len());
        assert_eq!(span.length, "greet".len());
    }

    /// AC-P1-14 (second name): in a multi-name selective import, each unused name
    /// has an independently anchored span.
    #[test]
    fn selective_multi_name_each_span_anchored_independently() {
        let src = "@import { foo, bar } from \"./lib.mds\"\nHello!\n";
        assert_spans_slice_to_their_names(src, &["foo", "bar"]);

        let diags = lint_src(src);
        let offsets: Vec<usize> = diags
            .iter()
            .filter(|d| d.rule == RULE)
            .filter_map(|d| d.span.as_ref().map(|s| s.offset))
            .collect();
        assert_eq!(
            offsets,
            vec!["@import { ".len(), "@import { foo, ".len()],
            "each name must anchor at its own byte position"
        );
    }

    /// AC-P1-15: a comma segment that trims to nothing is dropped from `names`
    /// AFTER the split. A per-segment offset vector built independently of that
    /// filter desyncs, and every later name silently anchors at the previous
    /// name's offset — in bounds, plausible, wrong (PF-012). `unwrap_or` cannot
    /// rescue this: the indices shift rather than run short.
    #[test]
    fn empty_comma_segment_does_not_desync_name_offsets() {
        let src = "@import { a, , b } from \"./l.mds\"\nHello!\n";
        assert_spans_slice_to_their_names(src, &["a", "b"]);

        // Explicit anti-desync pin: 'b' must NOT land on 'a'.
        let diags = lint_src(src);
        let offsets: Vec<usize> = diags
            .iter()
            .filter(|d| d.rule == RULE)
            .filter_map(|d| d.span.as_ref().map(|s| s.offset))
            .collect();
        assert_eq!(offsets, vec!["@import { ".len(), "@import { a, , ".len()]);
    }

    /// AC-P1-15 (trailing comma): same desync hazard, the form users actually
    /// write.
    #[test]
    fn trailing_comma_does_not_desync_name_offsets() {
        let src = "@import { a, b, } from \"./l.mds\"\nHello!\n";
        assert_spans_slice_to_their_names(src, &["a", "b"]);
    }

    /// AC-P1-16(d): trailing whitespace on the directive line must not shift name
    /// offsets.
    ///
    /// End-to-end pin of the lexer → `parse_directive` → `parse_import_directive`
    /// offset chain. Note it does NOT discriminate `trim()` from `trim_start()` in
    /// the delta computation: `parse_directive` trims the token before
    /// `parse_import_directive` ever sees it, so the trailing run is already gone
    /// by then. `parse_import_directive_delta_ignores_trailing_whitespace` in
    /// `parser_tests.rs` is the test that does discriminate them.
    #[test]
    fn trailing_whitespace_on_directive_line_does_not_shift_offsets() {
        let src = "@import { a, b } from \"./l.mds\"   \nHello!\n";
        assert_spans_slice_to_their_names(src, &["a", "b"]);
    }

    /// AC-P1-16(a): irregular interior whitespace.
    #[test]
    fn irregular_interior_whitespace_anchors_correctly() {
        let src = "@import {  a ,   b  } from \"./l.mds\"\nHello!\n";
        assert_spans_slice_to_their_names(src, &["a", "b"]);
    }

    /// AC-P1-16(b): a name that is a strict prefix of another name in the same
    /// import. A source re-scan would anchor `foobar` at `foo`'s position; the
    /// parser-supplied offsets do not.
    #[test]
    fn prefix_name_collision_anchors_at_own_offset() {
        let src = "@import { foo, foobar } from \"./l.mds\"\nHello!\n";
        assert_spans_slice_to_their_names(src, &["foo", "foobar"]);

        let diags = lint_src(src);
        let offsets: Vec<usize> = diags
            .iter()
            .filter(|d| d.rule == RULE)
            .filter_map(|d| d.span.as_ref().map(|s| s.offset))
            .collect();
        assert_eq!(offsets, vec!["@import { ".len(), "@import { foo, ".len()]);
    }

    /// AC-P1-16(c): a name that also occurs inside the quoted import path. The
    /// span must land between the braces, never inside the path literal.
    #[test]
    fn name_also_present_in_path_anchors_inside_braces() {
        let src = "@import { lib } from \"./lib.mds\"\nHello!\n";
        assert_spans_slice_to_their_names(src, &["lib"]);

        let diags = lint_src(src);
        let span = diags
            .iter()
            .find(|d| d.rule == RULE)
            .and_then(|d| d.span.as_ref())
            .expect("diagnostic must fire");
        let quote = src.find('"').expect("path is quoted");
        assert!(
            span.offset < quote,
            "span must land inside the braces (before byte {quote}), got {}",
            span.offset
        );
    }

    /// AC-P1-16(e): CRLF line endings. `scan_directive` strips only the trailing
    /// `\r`, so name offsets are unaffected.
    #[test]
    fn crlf_line_endings_do_not_shift_offsets() {
        let src = "@import { a, b } from \"./l.mds\"\r\nHello!\r\n";
        assert_spans_slice_to_their_names(src, &["a", "b"]);
    }

    /// AC-P1-16(f): multi-byte UTF-8 content preceding the import shifts the BASE
    /// offset. Import names themselves are ASCII-only (`is_valid_identifier`), so
    /// `name.len()` is always a safe span length — what this pins is the base
    /// offset chain from the lexer through to the diagnostic.
    #[test]
    fn multibyte_prefix_shifts_base_offset_without_splitting_names() {
        let src = "héllo wörld\n@import { a, b } from \"./l.mds\"\nHello!\n";
        assert_spans_slice_to_their_names(src, &["a", "b"]);

        let diags = lint_src(src);
        let span = diags
            .iter()
            .find(|d| d.rule == RULE)
            .and_then(|d| d.span.as_ref())
            .expect("diagnostic must fire");
        let directive_start = src.find("@import").expect("directive present");
        assert_eq!(
            span.offset,
            directive_start + "@import { ".len(),
            "the base offset must account for the multi-byte prefix"
        );
    }

    /// AC-P1-19 / AD-203-3 / PF-005: if `name_offsets` ever runs short, the
    /// diagnostic degrades to the whole `@import` keyword span rather than
    /// mis-anchoring or panicking.
    ///
    /// Built by hand because the parser cannot produce a desynced `ImportFact` —
    /// which is the point: the unconditional fallback, not the `debug_assert!`,
    /// is the guard that survives a release build.
    #[test]
    fn short_name_offsets_degrade_to_import_keyword_span() {
        use crate::lint::facts::{ImportFact, ImportKind};

        let src = "@import { a, b } from \"./l.mds\"\nHello!\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "test.mds", src).unwrap();
        let mut ctx = collect_facts(&module, false, src).unwrap();

        // Truncate the offset vector to simulate the desync AD-203-3 guards.
        for imp in &mut ctx.imports {
            if imp.kind == ImportKind::Selective {
                imp.name_offsets.truncate(1);
            }
        }
        assert!(
            ctx.imports
                .iter()
                .any(|i: &ImportFact| i.kind == ImportKind::Selective && i.name_offsets.len() == 1),
            "the fixture must actually be desynced, or this test proves nothing"
        );

        let mut builder = LintResultBuilder::new();
        check(
            &module,
            &ctx,
            "test.mds",
            &LintConfig::default(),
            &mut builder,
        );
        let diags = builder.build(false).diagnostics;
        let spans: Vec<(usize, usize)> = diags
            .iter()
            .filter(|d| d.rule == RULE)
            .filter_map(|d| d.span.as_ref().map(|s| (s.offset, s.length)))
            .collect();

        // 'a' still has an offset; 'b' falls back to the whole @import keyword.
        assert_eq!(
            spans,
            vec![(0, "@import".len()), ("@import { ".len(), "a".len())],
            "the name that lost its offset must degrade to the @import keyword span"
        );
        // And the degraded span must still slice to something meaningful.
        assert_eq!(&src[0.."@import".len()], "@import");
    }

    /// Alias form still anchors at the `@import` keyword (not changed by #203).
    #[test]
    fn alias_span_anchors_at_import_keyword() {
        let src = "@import \"./lib.mds\" as lib\nHello!\n";
        let diags = lint_src(src);
        let diag = diags
            .iter()
            .find(|d| d.rule == RULE)
            .expect("unused alias import diagnostic must fire");
        let span = diag.span.as_ref().expect("span must be present");
        assert_eq!(
            span.offset, 0,
            "alias span must start at byte 0 (@import); got {}",
            span.offset
        );
        assert_eq!(
            span.length,
            "@import".len(),
            "alias span.length must equal '@import' length; got {}",
            span.length
        );
    }
}
