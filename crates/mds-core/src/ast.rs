/// A complete MDS source file.
#[derive(Debug, Clone)]
pub struct Module {
    pub frontmatter: Option<Frontmatter>,
    /// `@extends "path"` directive, present only in child templates.
    ///
    /// When `Some`, this module is a child template and its body may only contain
    /// `@block` override nodes (plus blank lines). When `None`, it is a standalone
    /// module that renders its own body directly.
    ///
    /// `module.extends.is_some()` is the canonical child-vs-standalone discriminator —
    /// illegal states are unrepresentable: a module without `extends` cannot have
    /// inherited blocks, and a module with `extends` cannot render stand-alone.
    pub extends: Option<ExtendsDirective>,
    pub body: Vec<Node>,
}

/// The `@extends "path"` directive that makes a module a child template.
///
/// Modeled after `ImportDirective` — one path string plus the byte offset of the
/// directive in the source (for error span reporting).
#[derive(Debug, Clone)]
pub struct ExtendsDirective {
    /// The raw quoted path, e.g. `"./base.mds"`.
    pub path: String,
    /// Byte offset of the `@extends` token in the source.
    ///
    /// The resolver uses this to attach precise error spans when the base file is not
    /// found or when a stray `@extends` occurs mid-file, via
    /// `attach_import_span(e, path, file, source, ext.offset)`.
    pub offset: usize,
}

/// A condition in an `@if` or `@elseif` directive.
///
/// # Why no `PartialEq`
///
/// `Condition` intentionally does **not** derive `PartialEq`. Its variants hold
/// `Expr` values, and `Expr::NumberLiteral(f64)` uses IEEE 754 semantics where
/// `NaN != NaN`, so a blanket derived `PartialEq` on `Condition` would be surprising
/// and error-prone (the evaluator handles NaN safety explicitly). If structural
/// comparison of `Condition` values is needed in the future, implement `PartialEq`
/// manually with a clear comment about the NaN case rather than deriving it.
#[derive(Debug, Clone)]
pub enum Condition {
    /// `@if config.debug:` or `@if func(x):` — truthy check on an expression
    Truthy(Expr),
    /// `@if !config.debug:` or `@if !func(x):` — negated truthy check
    Not(Expr),
    /// `@if role == "admin":` or `@if func(a) == func(b):` — strict equality (both sides are expressions)
    Eq(Expr, Expr),
    /// `@if role != "admin":` or `@if func(a) != func(b):` — strict inequality
    NotEq(Expr, Expr),
    /// `@if a && b && c:` — all operands must be truthy (short-circuits on first false)
    And(Vec<Condition>),
    /// `@if a || b || c:` — any operand must be truthy (short-circuits on first true)
    Or(Vec<Condition>),
}

/// YAML frontmatter block.
#[derive(Debug, Clone)]
pub struct Frontmatter {
    pub raw: String,
}

/// Top-level AST nodes.
#[derive(Debug, Clone)]
pub enum Node {
    /// Raw text content (may contain interpolation markers).
    Text(TextNode),
    /// Variable or function call interpolation: `{{name}}` or `{{greet("x")}}`.
    Interpolation(Interpolation),
    /// An escaped double-brace: `\{{` → literal `{{` in output.
    ///
    /// The `offset` field captures the byte position of the `\` in the source,
    /// so that later phases can emit a source-map point mapping for this node.
    EscapedBrace { offset: usize },
    /// `@if var:` ... `@else:` ... `@end`
    If(IfBlock),
    /// `@for item in items:` ... `@end`
    For(ForBlock),
    /// `@define name(params):` ... `@end`
    Define(DefineBlock),
    /// `@import "path" as alias` / `@import "path"` / `@import { names } from "path"`
    Import(ImportDirective),
    /// `@export name` / `@export name from "path"` / `@export * from "path"`
    Export(ExportDirective),
    /// `@include alias`
    Include(IncludeDirective),
    /// `@message role:` ... `@end` — structured message block for LLM chat APIs.
    Message(MessageBlock),
    /// `@block name:` ... `@end` — a named placeholder block for template inheritance.
    ///
    /// In a standalone (non-extending) module the body is rendered inline as the default
    /// content. In a child module these are override nodes that replace the base template's
    /// corresponding block.
    Block(BlockNode),
}

/// A named template block: `@block name:` ... `@end`.
///
/// In standalone mode the body is evaluated inline (the markers are invisible).
/// In inheritance mode the resolver splices overrides from the child into the base skeleton
/// before validation and evaluation.
#[derive(Debug, Clone)]
pub struct BlockNode {
    /// The block name — a valid identifier, e.g. `instructions`.
    pub name: String,
    pub body: Vec<Node>,
    /// Byte offset of the `@block` token in the source (for error spans).
    pub offset: usize,
}

/// A structured message block: `@message role:` ... `@end`.
///
/// In text mode the body is rendered inline (the `@message` markers are invisible).
/// In messages mode the body is evaluated to a string and collected as a `Message`.
#[derive(Debug, Clone)]
pub struct MessageBlock {
    /// The role expression. Bare words become `Expr::StringLiteral`; `{expr}` forms
    /// are parsed via `parse_expr_inner`.
    pub role: Expr,
    pub body: Vec<Node>,
    pub offset: usize,
}

#[derive(Debug, Clone)]
pub struct TextNode {
    pub text: String,
    /// Byte offset of the first character of this text node in the source.
    pub offset: usize,
}

/// An interpolation expression inside `{ }`.
#[derive(Debug, Clone)]
pub struct Interpolation {
    pub expr: Expr,
    pub offset: usize,
    pub len: usize,
}

/// Expressions that can appear inside `{ }`, directive conditions, and directive iterables.
#[derive(Debug, Clone)]
pub enum Expr {
    /// Simple variable reference: `{name}`
    Var(String),
    /// Function call: `{greet("Alice")}` or `{greet(name)}`
    Call { name: String, args: Vec<Arg> },
    /// Qualified call: `{utils.greet("Alice")}`
    QualifiedCall {
        namespace: String,
        name: String,
        args: Vec<Arg>,
    },
    /// Object field access: `{config.key}` or `{a.b.c}`
    MemberAccess { object: String, fields: Vec<String> },
    /// A string literal used in condition comparisons: `"admin"`
    StringLiteral(String),
    /// A numeric literal used in condition comparisons: `42`, `3.14`
    NumberLiteral(f64),
    /// A boolean literal used in condition comparisons: `true`, `false`
    BooleanLiteral(bool),
    /// The null literal used in condition comparisons: `null`
    NullLiteral,
}

/// A function argument — a string literal, variable reference, or nested call.
#[derive(Debug, Clone)]
pub enum Arg {
    StringLiteral(String),
    /// A numeric literal argument: `func(42)` or `func(-3.14)`
    NumberLiteral(f64),
    /// A boolean literal argument: `func(true)` or `func(false)`
    BooleanLiteral(bool),
    /// A null literal argument: `func(null)`
    NullLiteral,
    Var(String),
    /// Nested function call: `{outer(inner("arg"))}`
    Call {
        name: String,
        args: Vec<Arg>,
    },
    /// Object field access passed as argument: `greet(config.name)`
    MemberAccess {
        object: String,
        fields: Vec<String>,
    },
}

/// A single `@elseif` branch in an `@if` block.
///
/// Replaces the prior `(Condition, Vec<Node>)` tuple to carry a per-branch
/// byte offset, closing the span-accuracy gap tracked in #181: lint rules can
/// now anchor diagnostics at the exact `@elseif` line rather than the
/// enclosing `@if` offset.
///
/// # Structural equality
///
/// `structural_eq.rs` compares `ElseifBranch` by `condition` and `body` only
/// — `offset` is intentionally excluded (it is a span annotation, not part of
/// the template's logical identity).
#[derive(Debug, Clone)]
pub struct ElseifBranch {
    pub condition: Condition,
    pub body: Vec<Node>,
    /// Byte offset of the `@elseif` token in the source (for diagnostic spans).
    pub offset: usize,
}

#[derive(Debug, Clone)]
pub struct IfBlock {
    /// The primary condition (`@if <condition>:`).
    pub condition: Condition,
    pub then_body: Vec<Node>,
    /// Zero or more `@elseif` branches, evaluated in order (short-circuit).
    pub elseif_branches: Vec<ElseifBranch>,
    pub else_body: Option<Vec<Node>>,
    pub offset: usize,
    /// Byte offset of the `@else:` token in the source, when present.
    ///
    /// `None` when there is no `@else` clause. Used by lint rules to anchor
    /// diagnostics at the exact `@else` line rather than the enclosing `@if`.
    pub else_offset: Option<usize>,
    /// Byte offset of the `@end` token in the source (for fix span computation).
    ///
    /// This is a span annotation — intentionally excluded from structural
    /// equality (see `structural_eq.rs`), similar to `ElseifBranch.offset`.
    pub end_offset: usize,
}

#[derive(Debug, Clone)]
pub struct ForBlock {
    pub var: String,
    /// Optional key variable for `@for key, value in obj:` iteration.
    pub key_var: Option<String>,
    /// The expression to iterate over. May be a variable, dot-path, function call, etc.
    pub iterable: Expr,
    pub body: Vec<Node>,
    pub offset: usize,
    /// Byte offset of the `@end` token in the source (for fix span computation).
    ///
    /// This is a span annotation — intentionally excluded from structural
    /// equality (see `structural_eq.rs`), similar to `ElseifBranch.offset`.
    pub end_offset: usize,
}

/// A parameter in a `@define` function definition.
///
/// Parameters may have an optional default value. Parameters without defaults
/// must appear before any parameter with a default.
#[derive(Debug, Clone)]
pub struct Param {
    /// The parameter name (a valid identifier).
    pub name: String,
    /// Optional default value, parsed at definition time.
    ///
    /// When present, the `Expr` is guaranteed by the parser to be a *literal*
    /// variant (`StringLiteral`, `NumberLiteral`, `BooleanLiteral`, or
    /// `NullLiteral`) — `parse_define_params` rejects non-literal defaults such
    /// as function calls or variable references. The evaluator converts it to a
    /// runtime `Value` via `literal_expr_to_value` when the caller omits the
    /// argument.
    pub default: Option<Expr>,
}

impl Param {
    /// Convenience constructor for a required parameter with no default value.
    pub fn required(name: impl Into<String>) -> Self {
        Param {
            name: name.into(),
            default: None,
        }
    }
}

/// Count the number of required (no-default) parameters in a param list.
///
/// A parameter is required when its `default` field is `None`. Optional parameters
/// (those with `Some(Expr)`) may be omitted at call sites and receive their
/// default value at runtime via `literal_expr_to_value`.
///
/// Defined here alongside `Param` because it is purely a property of the AST
/// type — both the validator and evaluator import it from this module.
pub(crate) fn required_param_count(params: &[Param]) -> usize {
    params.iter().filter(|p| p.default.is_none()).count()
}

/// Render a function signature as a human-readable string for arity-mismatch help (B3 — F7).
///
/// Output format: `name(param1, param2 = default)`.
/// Required params are shown by name only; optional params include their default.
/// Used by `MdsError::arity` / `arity_at` to populate `signature_note`.
pub(crate) fn render_signature(name: &str, params: &[Param]) -> String {
    let args = params
        .iter()
        .map(render_param)
        .collect::<Vec<_>>()
        .join(", ");
    format!("{name}({args})")
}

/// Render a single parameter for `render_signature`.
fn render_param(p: &Param) -> String {
    match &p.default {
        None => p.name.clone(),
        Some(expr) => format!("{} = {}", p.name, render_default(expr)),
    }
}

/// Render a literal default-value expression as a compact string.
///
/// The parser guarantees that parameter defaults are always literal expressions
/// (the validator rejects function calls and variable references as defaults).
/// Complex or future expression forms fall back to `"…"`.
pub(crate) fn render_default(expr: &Expr) -> String {
    match expr {
        Expr::StringLiteral(s) => format!("\"{s}\""),
        Expr::NumberLiteral(n) => {
            if n.fract() == 0.0 {
                format!("{}", *n as i64)
            } else {
                format!("{n}")
            }
        }
        Expr::BooleanLiteral(b) => b.to_string(),
        Expr::NullLiteral => "null".to_string(),
        // Guarded fallback — parser currently rejects non-literal defaults.
        _ => "\u{2026}".to_string(),
    }
}

#[derive(Debug, Clone)]
pub struct DefineBlock {
    pub name: String,
    pub params: Vec<Param>,
    pub body: Vec<Node>,
    pub offset: usize,
    /// Byte offset of the `@end` token in the source (for fix span computation).
    ///
    /// This is a span annotation — intentionally excluded from structural
    /// equality (see `structural_eq.rs`), similar to `ElseifBranch.offset`.
    pub end_offset: usize,
}

#[derive(Debug, Clone)]
pub enum ImportDirective {
    /// `@import "path" as alias`
    Alias {
        path: String,
        alias: String,
        offset: usize,
    },
    /// `@import "path"` (merge)
    Merge { path: String, offset: usize },
    /// `@import { name1, name2 } from "path"`
    Selective {
        names: Vec<String>,
        path: String,
        offset: usize,
        /// Byte offset of each name within the source file (parallel to `names`).
        ///
        /// **AD-203-1 / PF-012:** computed by `parse_import_directive` in a single
        /// pass alongside name collection so the two vectors never desync.  Used by
        /// the `unused-import` rule to anchor the diagnostic span at the unused name
        /// rather than at the `@import` keyword.
        ///
        /// This field is intentionally excluded from structural equality (see
        /// `structural_eq.rs` — `Selective` uses `..` pattern) because it is a
        /// span annotation, not part of the semantic content of the directive.
        name_offsets: Vec<usize>,
    },
}

#[derive(Debug, Clone)]
pub enum ExportDirective {
    /// `@export name`
    Named {
        name: String,
        /// Byte offset of the `@export` token in the source (for diagnostic spans).
        offset: usize,
    },
    /// `@export name from "path"`
    ReExport {
        name: String,
        path: String,
        /// Byte offset of the `@export` token in the source (for diagnostic spans).
        offset: usize,
    },
    /// `@export * from "path"`
    Wildcard {
        path: String,
        /// Byte offset of the `@export` token in the source (for diagnostic spans).
        offset: usize,
    },
}

#[derive(Debug, Clone)]
pub struct IncludeDirective {
    pub alias: String,
    pub offset: usize,
}
