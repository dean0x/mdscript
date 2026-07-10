//! Facts walk: single pre-sized AST traversal building `AnalysisContext`.
//!
//! The facts walk visits the entry module's AST exactly ONCE, collecting all
//! information that rule functions will need (symbol tables, import maps, export
//! sets, etc.). Rules are then applied as plain functions over the completed
//! `AnalysisContext` — no second traversal required (AC-PERF-02).
//!
//! **Recursion bound**: the walk tracks nesting depth and returns a
//! `MdsError::ResourceLimit` when depth exceeds `MAX_NESTING_DEPTH` (AC-PERF-04).
//! This mirrors the parser's own depth limit to prevent stack overflow on
//! adversarially-crafted inputs.
//!
//! **S1 scope**: this module contains the walk skeleton with stubs for each rule's
//! fact-collection phase. The actual symbol-table fields and per-rule collection
//! logic are added in S2 when the 9 rule implementations arrive.

use crate::ast::{Module, Node};
use crate::error::MdsError;
use crate::limits::MAX_NESTING_DEPTH;

// ── AnalysisContext ────────────────────────────────────────────────────────────

/// Pre-sized fact set built from a single AST traversal.
///
/// All fields are populated by `collect_facts`; rule functions read them
/// but never mutate them. S2 adds per-rule symbol-table fields here.
#[derive(Debug, Default)]
pub struct AnalysisContext {
    /// Whether the module has any explicit `@export` directives (affecting
    /// unused-function semantics — default-public modules export everything).
    pub has_explicit_exports: bool,

    /// Whether the module is a partial (suppresses unused-* rules).
    /// Detected from the file name (starts with `_`) in the caller; stored here
    /// for rule access without re-checking the filename on each rule call.
    ///
    /// Read by unused-* rule implementations in S2. Dead in S1 stubs.
    #[allow(dead_code)]
    pub is_partial_or_extends: bool,
    // S2 will add:
    //   pub imports: Vec<ImportFact>,
    //   pub exports: Vec<ExportFact>,
    //   pub defines: Vec<DefineFact>,
    //   pub frontmatter_vars: Vec<FmVarFact>,
    //   pub call_sites: Vec<CallFact>,
    //   pub var_uses: Vec<VarUseFact>,
}

// ── Facts walk ────────────────────────────────────────────────────────────────

/// Perform a single pre-sized traversal of `module.body`, collecting all facts
/// into an `AnalysisContext`.
///
/// Returns `Err(MdsError::ResourceLimit)` when the AST nesting exceeds
/// `MAX_NESTING_DEPTH` (defense against adversarially crafted inputs that would
/// exhaust the call stack in recursive rule implementations).
pub(super) fn collect_facts(
    module: &Module,
    is_partial_or_extends: bool,
) -> Result<AnalysisContext, MdsError> {
    let mut ctx = AnalysisContext {
        is_partial_or_extends,
        ..Default::default()
    };

    walk_nodes(&module.body, &mut ctx, 0)?;

    Ok(ctx)
}

/// Recursive body walker — bounded by `MAX_NESTING_DEPTH`.
fn walk_nodes(nodes: &[Node], ctx: &mut AnalysisContext, depth: usize) -> Result<(), MdsError> {
    if depth > MAX_NESTING_DEPTH {
        return Err(MdsError::resource_limit(format!(
            "template nesting exceeds maximum depth of {MAX_NESTING_DEPTH}"
        )));
    }

    for node in nodes {
        match node {
            Node::Export(_) => {
                ctx.has_explicit_exports = true;
            }
            Node::If(b) => {
                walk_nodes(&b.then_body, ctx, depth + 1)?;
                for (_, branch_body) in &b.elseif_branches {
                    walk_nodes(branch_body, ctx, depth + 1)?;
                }
                if let Some(else_body) = &b.else_body {
                    walk_nodes(else_body, ctx, depth + 1)?;
                }
            }
            Node::For(b) => {
                walk_nodes(&b.body, ctx, depth + 1)?;
            }
            Node::Define(b) => {
                walk_nodes(&b.body, ctx, depth + 1)?;
            }
            Node::Message(b) => {
                walk_nodes(&b.body, ctx, depth + 1)?;
            }
            Node::Block(b) => {
                walk_nodes(&b.body, ctx, depth + 1)?;
            }
            // Leaf nodes — no children to recurse.
            Node::Text(_)
            | Node::Interpolation(_)
            | Node::EscapedBrace
            | Node::Import(_)
            | Node::Include(_) => {}
        }
    }

    Ok(())
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::parse_with_ctx;

    fn parse(src: &str) -> Module {
        let tokens = tokenize(src, "test.mds").unwrap();
        parse_with_ctx(&tokens, "", src).unwrap()
    }

    #[test]
    fn collect_facts_empty_module() {
        let module = parse("Hello!\n");
        let ctx = collect_facts(&module, false).unwrap();
        assert!(!ctx.has_explicit_exports);
        assert!(!ctx.is_partial_or_extends);
    }

    #[test]
    fn collect_facts_detects_explicit_exports() {
        let src = "@define greet():\nhello\n@end\n@export greet\n";
        let module = parse(src);
        let ctx = collect_facts(&module, false).unwrap();
        assert!(ctx.has_explicit_exports, "should detect @export directive");
    }

    #[test]
    fn collect_facts_partial_flag_propagated() {
        let module = parse("Hello!\n");
        let ctx = collect_facts(&module, true).unwrap();
        assert!(ctx.is_partial_or_extends);
    }

    /// AC-PERF-04: Nesting deeper than MAX_NESTING_DEPTH (64) returns ResourceLimit.
    ///
    /// The parser also enforces `MAX_NESTING_DEPTH`, so we cannot build a deeply-nested
    /// AST through the normal parse path — the parser would reject it first. Instead we
    /// construct the `Module` AST directly to test the facts-walk depth guard independently
    /// (defense-in-depth: the walk limit protects against future parser changes).
    #[test]
    fn collect_facts_depth_limit_enforced() {
        use crate::ast::{Condition, Expr, IfBlock, Module, Node, TextNode};

        let depth = MAX_NESTING_DEPTH + 1;

        // Build depth-level nested @if blocks from the inside out.
        let mut inner: Vec<Node> = vec![Node::Text(TextNode {
            text: "deep\n".to_string(),
            offset: 0,
        })];
        for _ in 0..depth {
            inner = vec![Node::If(IfBlock {
                condition: Condition::Truthy(Expr::Var("x".to_string())),
                then_body: inner,
                elseif_branches: vec![],
                else_body: None,
                offset: 0,
            })];
        }
        let module = Module {
            frontmatter: None,
            extends: None,
            body: inner,
        };

        let result = collect_facts(&module, false);
        assert!(
            result.is_err(),
            "nesting depth {depth} should exceed MAX_NESTING_DEPTH={MAX_NESTING_DEPTH}"
        );
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("nesting"),
            "error should mention nesting, got: {err}"
        );
    }
}
