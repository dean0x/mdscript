//! Manual structural equality helpers for AST nodes.
//!
//! The AST deliberately does NOT derive `PartialEq` on `Node`, `Condition`, `Expr`,
//! or `Arg` because `Expr::NumberLiteral(f64)` uses IEEE 754 semantics where
//! `NaN != NaN` (see ast.rs comment). These helpers implement structural comparison
//! that:
//!
//! - Ignores all `offset` and `len` fields (position-independent comparison).
//! - Compares `f64` values via `to_bits()` (bitwise equality, treating `NaN == NaN`).
//!   Note: the parser rejects NaN/infinity literals, so in practice this case never
//!   occurs on well-formed input.
//!
//! Used by `redundant_else` (body identity) and `unreachable_branch` (duplicate
//! @elseif condition detection).

use crate::ast::{Arg, Condition, Expr, ImportDirective, Node};

/// Compare two expression trees for structural identity (offset/len ignored).
pub(crate) fn exprs_eq(a: &Expr, b: &Expr) -> bool {
    match (a, b) {
        (Expr::Var(x), Expr::Var(y)) => x == y,
        (Expr::StringLiteral(x), Expr::StringLiteral(y)) => x == y,
        // NaN-safe: compare via bits. Parser rejects NaN/infinity, but be defensive.
        (Expr::NumberLiteral(x), Expr::NumberLiteral(y)) => x.to_bits() == y.to_bits(),
        (Expr::BooleanLiteral(x), Expr::BooleanLiteral(y)) => x == y,
        (Expr::NullLiteral, Expr::NullLiteral) => true,
        (Expr::Call { name: n1, args: a1 }, Expr::Call { name: n2, args: a2 }) => {
            n1 == n2 && args_eq(a1, a2)
        }
        (
            Expr::QualifiedCall {
                namespace: ns1,
                name: n1,
                args: a1,
            },
            Expr::QualifiedCall {
                namespace: ns2,
                name: n2,
                args: a2,
            },
        ) => ns1 == ns2 && n1 == n2 && args_eq(a1, a2),
        (
            Expr::MemberAccess {
                object: o1,
                fields: f1,
            },
            Expr::MemberAccess {
                object: o2,
                fields: f2,
            },
        ) => o1 == o2 && f1 == f2,
        _ => false,
    }
}

/// Compare two argument lists for structural identity.
pub(crate) fn args_eq(a: &[Arg], b: &[Arg]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| arg_eq(x, y))
}

/// Compare two function arguments for structural identity.
fn arg_eq(a: &Arg, b: &Arg) -> bool {
    match (a, b) {
        (Arg::StringLiteral(x), Arg::StringLiteral(y)) => x == y,
        (Arg::NumberLiteral(x), Arg::NumberLiteral(y)) => x.to_bits() == y.to_bits(),
        (Arg::BooleanLiteral(x), Arg::BooleanLiteral(y)) => x == y,
        (Arg::NullLiteral, Arg::NullLiteral) => true,
        (Arg::Var(x), Arg::Var(y)) => x == y,
        (Arg::Call { name: n1, args: a1 }, Arg::Call { name: n2, args: a2 }) => {
            n1 == n2 && args_eq(a1, a2)
        }
        (
            Arg::MemberAccess {
                object: o1,
                fields: f1,
            },
            Arg::MemberAccess {
                object: o2,
                fields: f2,
            },
        ) => o1 == o2 && f1 == f2,
        _ => false,
    }
}

/// Compare two conditions for structural identity (offset/len ignored).
pub(crate) fn conditions_eq(a: &Condition, b: &Condition) -> bool {
    match (a, b) {
        (Condition::Truthy(e1), Condition::Truthy(e2)) => exprs_eq(e1, e2),
        (Condition::Not(e1), Condition::Not(e2)) => exprs_eq(e1, e2),
        (Condition::Eq(l1, r1), Condition::Eq(l2, r2)) => exprs_eq(l1, l2) && exprs_eq(r1, r2),
        (Condition::NotEq(l1, r1), Condition::NotEq(l2, r2)) => {
            exprs_eq(l1, l2) && exprs_eq(r1, r2)
        }
        (Condition::And(v1), Condition::And(v2)) | (Condition::Or(v1), Condition::Or(v2)) => {
            v1.len() == v2.len() && v1.iter().zip(v2).all(|(x, y)| conditions_eq(x, y))
        }
        _ => false,
    }
}

/// Compare two node slices for structural identity (all offset/len fields ignored).
pub(crate) fn nodes_eq(a: &[Node], b: &[Node]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| node_eq(x, y))
}

/// Compare two AST nodes for structural identity.
pub(crate) fn node_eq(a: &Node, b: &Node) -> bool {
    match (a, b) {
        (Node::Text(t1), Node::Text(t2)) => t1.text == t2.text,
        (Node::EscapedBrace { .. }, Node::EscapedBrace { .. }) => true,
        (Node::Interpolation(i1), Node::Interpolation(i2)) => exprs_eq(&i1.expr, &i2.expr),
        (Node::If(b1), Node::If(b2)) => {
            conditions_eq(&b1.condition, &b2.condition)
                && nodes_eq(&b1.then_body, &b2.then_body)
                && b1.elseif_branches.len() == b2.elseif_branches.len()
                && b1
                    .elseif_branches
                    .iter()
                    .zip(&b2.elseif_branches)
                    .all(|(br1, br2)| {
                        conditions_eq(&br1.condition, &br2.condition)
                            && nodes_eq(&br1.body, &br2.body)
                    })
                && match (&b1.else_body, &b2.else_body) {
                    (None, None) => true,
                    (Some(n1), Some(n2)) => nodes_eq(n1, n2),
                    _ => false,
                }
        }
        (Node::For(f1), Node::For(f2)) => {
            f1.var == f2.var
                && f1.key_var == f2.key_var
                && exprs_eq(&f1.iterable, &f2.iterable)
                && nodes_eq(&f1.body, &f2.body)
        }
        (Node::Define(d1), Node::Define(d2)) => {
            d1.name == d2.name
                && d1
                    .params
                    .iter()
                    .map(|p| &p.name)
                    .eq(d2.params.iter().map(|p| &p.name))
                && nodes_eq(&d1.body, &d2.body)
        }
        (Node::Import(i1), Node::Import(i2)) => imports_eq(i1, i2),
        (Node::Export(_), Node::Export(_)) => false, // offset-bearing; not used in structural checks
        (Node::Include(i1), Node::Include(i2)) => i1.alias == i2.alias,
        (Node::Message(m1), Node::Message(m2)) => {
            exprs_eq(&m1.role, &m2.role) && nodes_eq(&m1.body, &m2.body)
        }
        (Node::Block(b1), Node::Block(b2)) => b1.name == b2.name && nodes_eq(&b1.body, &b2.body),
        _ => false,
    }
}

fn imports_eq(a: &ImportDirective, b: &ImportDirective) -> bool {
    match (a, b) {
        (
            ImportDirective::Alias {
                path: p1,
                alias: a1,
                ..
            },
            ImportDirective::Alias {
                path: p2,
                alias: a2,
                ..
            },
        ) => p1 == p2 && a1 == a2,
        (ImportDirective::Merge { path: p1, .. }, ImportDirective::Merge { path: p2, .. }) => {
            p1 == p2
        }
        (
            ImportDirective::Selective {
                names: n1,
                path: p1,
                ..
            },
            ImportDirective::Selective {
                names: n2,
                path: p2,
                ..
            },
        ) => n1 == n2 && p1 == p2,
        _ => false,
    }
}

/// Return `true` if `expr` is a literal value (cannot be a variable or call).
pub(crate) fn is_literal(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::StringLiteral(_)
            | Expr::NumberLiteral(_)
            | Expr::BooleanLiteral(_)
            | Expr::NullLiteral
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Condition, ElseifBranch, Expr, IfBlock, Node, TextNode};

    #[test]
    fn exprs_eq_var() {
        let a = Expr::Var("x".to_string());
        let b = Expr::Var("x".to_string());
        let c = Expr::Var("y".to_string());
        assert!(exprs_eq(&a, &b));
        assert!(!exprs_eq(&a, &c));
    }

    #[test]
    fn exprs_eq_number_literal_nan_safe() {
        // NaN != NaN in IEEE 754, but to_bits() comparison makes them "equal" structurally.
        // In practice the parser never produces NaN, but the helper must not panic.
        let nan1 = Expr::NumberLiteral(f64::NAN);
        let nan2 = Expr::NumberLiteral(f64::NAN);
        // Both have the same bit pattern if produced by f64::NAN (canonical NaN).
        assert!(exprs_eq(&nan1, &nan2));
    }

    #[test]
    fn nodes_eq_text_same() {
        let a = Node::Text(TextNode {
            text: "hello".to_string(),
            offset: 0,
        });
        let b = Node::Text(TextNode {
            text: "hello".to_string(),
            offset: 99, // different offset — should still be equal
        });
        assert!(node_eq(&a, &b));
    }

    #[test]
    fn nodes_eq_text_different() {
        let a = Node::Text(TextNode {
            text: "hello".to_string(),
            offset: 0,
        });
        let b = Node::Text(TextNode {
            text: "world".to_string(),
            offset: 0,
        });
        assert!(!node_eq(&a, &b));
    }

    #[test]
    fn is_literal_detects_literals() {
        assert!(is_literal(&Expr::StringLiteral("x".to_string())));
        assert!(is_literal(&Expr::NumberLiteral(42.0)));
        assert!(is_literal(&Expr::BooleanLiteral(true)));
        assert!(is_literal(&Expr::NullLiteral));
        assert!(!is_literal(&Expr::Var("x".to_string())));
    }

    /// AC-P1-18 / AD-203-2: `name_offsets` MUST NOT participate in `imports_eq`.
    ///
    /// `ImportDirective::Selective` carries `name_offsets: Vec<usize>` as a span
    /// annotation (byte positions within the source file for each imported name).
    /// Structural equality compares only `names` and `path` — `name_offsets`
    /// (and `offset`) are excluded via the `..` pattern at line 175/180.
    ///
    /// This is load-bearing: the `duplicate-import` rule uses `imports_eq` to
    /// detect two directives that import the same names from the same path,
    /// regardless of how they were written (interior whitespace, trailing spaces,
    /// or any other formatting difference shifts `name_offsets`).  If `name_offsets`
    /// leaked into the comparison, two semantically-identical imports at different
    /// source positions would never be flagged as duplicates.
    ///
    /// The positive control below asserts that the same helper DOES return `false`
    /// when `names` differ — proving the test is capable of distinguishing unequal
    /// pairs and is not vacuously true (PF-013 / ADR-009).
    #[test]
    fn selective_name_offsets_excluded_from_imports_eq() {
        // Two Selective imports: same `names` and `path`, but different `name_offsets`
        // (as would happen when the imports are written with different interior
        // whitespace, or when a preceding directive shifts byte positions).
        let a = ImportDirective::Selective {
            names: vec!["foo".to_string(), "bar".to_string()],
            path: "./lib.mds".to_string(),
            offset: 0,
            name_offsets: vec![10, 15], // positions in one source
        };
        let b = ImportDirective::Selective {
            names: vec!["foo".to_string(), "bar".to_string()],
            path: "./lib.mds".to_string(),
            offset: 42, // different import-keyword offset too
            name_offsets: vec![53, 58], // shifted positions in another source
        };
        assert!(
            imports_eq(&a, &b),
            "AC-P1-18: imports_eq must return true for Selective imports that differ \
             only in name_offsets / offset — those are span annotations, not semantic content"
        );

        // Positive control: imports with DIFFERENT names are NOT equal.
        // This proves the assertion above is not vacuously true — the same
        // `imports_eq` call CAN return false when the logical content differs.
        let c = ImportDirective::Selective {
            names: vec!["foo".to_string(), "baz".to_string()], // "baz" != "bar"
            path: "./lib.mds".to_string(),
            offset: 0,
            name_offsets: vec![10, 15],
        };
        assert!(
            !imports_eq(&a, &c),
            "positive control: imports with different names must NOT be structurally equal"
        );
    }

    /// ElseifBranch.offset is intentionally excluded from structural equality.
    ///
    /// Two IfBlock nodes that are logically identical (same condition, same bodies)
    /// but parsed from different source positions must compare equal. This locks in
    /// the invariant documented in ast.rs: `offset` is a span annotation, not part
    /// of the template's logical identity.
    #[test]
    fn elseif_branch_offset_excluded_from_structural_eq() {
        let make_if = |elseif_offset: usize| {
            Node::If(IfBlock {
                condition: Condition::Truthy(Expr::Var("a".to_string())),
                then_body: vec![Node::Text(TextNode {
                    text: "A".to_string(),
                    offset: 0,
                })],
                elseif_branches: vec![ElseifBranch {
                    condition: Condition::Truthy(Expr::Var("b".to_string())),
                    body: vec![Node::Text(TextNode {
                        text: "B".to_string(),
                        offset: 0,
                    })],
                    offset: elseif_offset, // the only difference between the two nodes
                }],
                else_body: None,
                offset: 0,
                else_offset: None,
                end_offset: 0, // span annotation — excluded from structural equality
            })
        };
        let a = make_if(9);
        let b = make_if(999);
        assert!(
            node_eq(&a, &b),
            "IfBlocks differing only in ElseifBranch.offset must be structurally equal"
        );
    }
}
