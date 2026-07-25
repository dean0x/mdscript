//! Lint engine — static analysis of MDS templates beyond `mds check`.
//!
//! The engine runs AFTER the check gate (resolve+validate) passes, confirming the
//! template compiles correctly. It then independently tokenizes and parses the entry
//! source for a single-pass facts walk, applies the 9 lint rules as plain
//! functions, and returns a `LintResult`.
//!
//! ## Pipeline (per file)
//!
//! 1. **Check gate** — run existing `resolve_*_intrinsic` ONCE (validity gate).
//!    Analysis failure → return `Err(MdsError)`. Never run the pipeline twice.
//! 2. **Re-parse entry** — `tokenize` + `parse_with_ctx` the entry source string
//!    independently (mirrors the `scan_imports` pattern).
//! 3. **Facts walk** — one traversal building `AnalysisContext`.
//!    Recursion bounded at `MAX_NESTING_DEPTH=64` → `ResourceLimit` if exceeded.
//! 4. **Rule dispatch** — local-AST rules (empty_block, redundant_else,
//!    unreachable_branch, duplicate_*) each re-walk the AST; semantic rules
//!    query only `AnalysisContext`. Total: 1 facts walk + N rule walks.
//! 5. **Return** `LintResult` with diagnostics capped at `MAX_DIAGNOSTICS`.
//!
//! ## Architecture invariants
//!
//! - `resolver.rs`/`validator.rs`/runtime `Scope` are NEVER touched by this module.
//! - No new mds-core dependencies (zero-dep budget for WASM size).
//! - Per-file fresh resolve (v1): no cross-file ModuleCache "optimization".
//! - Non-generic rule dispatch: no monomorphization per rule.

pub mod config;
pub mod diagnostic;
pub(crate) mod facts;
pub mod fix;
pub(crate) mod rules;
pub(crate) mod tier;

pub use config::LintConfig;
pub use diagnostic::{
    neutralize_source_for_render, sanitize_control_chars, sanitize_control_chars_wire, FixLineSpan,
    LintDiagnostic, LintResult, Severity, TextEdit,
};

use crate::error::MdsError;
use crate::{lexer, parser};

use self::diagnostic::LintResultBuilder;
use self::facts::collect_facts;

// ── Partial/extends detection ─────────────────────────────────────────────────

/// Detect whether a module path/filename indicates a partial file.
///
/// Partials are detected by filename convention: a file whose basename starts
/// with `_` is a partial (e.g. `_header.mds`). Combined with the parsed module's
/// `extends` field (checked at the call site) to suppress unused-* rules.
pub(crate) fn is_partial_by_name(path: &str) -> bool {
    std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with('_'))
        .unwrap_or(false)
}

// ── Core lint runner ──────────────────────────────────────────────────────────

/// Run the lint engine over a parsed module source.
///
/// `source` is the raw entry source string (already used for the check gate).
/// `filename` is the display name used in diagnostics and JSON grouping.
/// Returns `Ok(LintResult)` with diagnostics produced by all 10 rules.
pub(crate) fn lint_source(
    source: &str,
    filename: &str,
    config: &LintConfig,
) -> Result<LintResult, MdsError> {
    // Re-parse the entry source independently (mirrors scan_imports, lib.rs:952).
    let tokens = lexer::tokenize(source, filename)?;
    let module = parser::parse_with_ctx(&tokens, filename, source)?;

    let is_partial = is_partial_by_name(filename);
    let is_extends = module.extends.is_some();

    let ctx = collect_facts(&module, is_partial || is_extends, source)?;

    // A file is standalone when it has no @import or @extends — Tier B fixes (unused-import,
    // unused-function) are only safe for standalone files because removing an export changes
    // what importers receive.
    let is_standalone = !ctx.is_partial_or_extends && ctx.imports.is_empty();

    // Rule dispatch — non-generic plain-fn dispatch (AC-PERF-02, no monomorphization).
    let mut builder = LintResultBuilder::new();
    run_rules(
        &module,
        &ctx,
        &tokens,
        source,
        filename,
        config,
        &mut builder,
    );

    Ok(builder.build(is_standalone))
}

/// Apply all 10 lint rules over the module and facts context.
///
/// Non-generic dispatch: each rule is a plain function call with no monomorphization.
/// Rules are listed in the same order as the implementation steps (local-AST first,
/// semantic second, token-based last) for readability.
fn run_rules(
    module: &crate::ast::Module,
    ctx: &facts::AnalysisContext,
    tokens: &[crate::lexer::Token],
    source: &str,
    filename: &str,
    config: &LintConfig,
    builder: &mut LintResultBuilder,
) {
    // Step 4 — local-AST rules
    rules::empty_block::check(module, ctx, filename, config, builder);
    rules::redundant_else::check(module, ctx, filename, config, builder);
    rules::unreachable_branch::check(module, ctx, filename, config, builder);
    rules::duplicate_import::check(module, ctx, filename, config, builder);
    rules::duplicate_export::check(module, ctx, filename, config, builder);

    // Step 5 — semantic rules
    rules::unused_variable::check(module, ctx, filename, config, builder);
    rules::unused_import::check(module, ctx, filename, config, builder);
    rules::unused_function::check(module, ctx, filename, config, builder);
    rules::shadow_variable::check(module, ctx, filename, config, builder);

    // Step 6 — token-based rules (require raw token stream)
    rules::legacy_interpolation::check(tokens, source, filename, config, builder);
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lint::config::LintConfig;

    fn lint_str(source: &str) -> Result<LintResult, MdsError> {
        lint_source(source, "test.mds", &LintConfig::default())
    }

    #[test]
    fn lint_source_valid_returns_no_diagnostics_for_plain_text() {
        let result = lint_str("Hello!\n").unwrap();
        assert!(result.diagnostics.is_empty());
        assert!(!result.truncated);
    }

    #[test]
    fn lint_source_invalid_source_returns_err() {
        // Missing @end — parse error.
        let result = lint_str("@if x:\nhello\n");
        assert!(result.is_err(), "should fail for invalid source");
    }

    #[test]
    fn is_partial_by_name_detects_underscore_prefix() {
        assert!(is_partial_by_name("_header.mds"));
        assert!(is_partial_by_name("dir/_partial.mds"));
        assert!(!is_partial_by_name("header.mds"));
        assert!(!is_partial_by_name("dir/main.mds"));
    }
}
