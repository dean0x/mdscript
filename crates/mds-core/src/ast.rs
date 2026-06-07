/// A complete MDS source file.
#[derive(Debug, Clone)]
pub struct Module {
    pub frontmatter: Option<Frontmatter>,
    pub body: Vec<Node>,
}

/// A literal value for a default parameter in `@define` blocks.
///
/// Only string, number, boolean, and null literals are supported.
///
/// # TODO: Type Duplication
///
/// `CondValue` (String, Number, Boolean, Null) is structurally identical to the
/// literal variants of `Expr` (StringLiteral, NumberLiteral, BooleanLiteral, NullLiteral),
/// and `Arg` has the same four literal variants as well — creating three parallel hierarchies.
///
/// The correct fix is:
/// - Replace `Param.default: Option<CondValue>` with `Param.default: Option<Expr>`
/// - Remove `CondValue` and `condvalue_to_value` from the evaluator
/// - Change `parse_cond_value` to call `parse_expr_inner` and return `Expr`
///
/// This was deferred because it is cross-cutting (touches `Param`, `parse_define_params`,
/// `condvalue_to_value`, and all test code that pattern-matches on `CondValue`), and
/// the current PR scope is expression directives in `@for`/`@if`. Address in a dedicated
/// cleanup PR — no behaviour change, pure type unification.
#[derive(Debug, Clone, PartialEq)]
pub enum CondValue {
    /// A string literal: `"admin"` or `'admin'`
    String(String),
    /// A numeric literal: `42`, `3.14`, `-5`
    ///
    /// # Invariant
    ///
    /// The parser rejects non-finite values (`NaN`, `+Inf`, `-Inf`) via
    /// `is_finite()` before constructing this variant, so any `Number` in a
    /// well-formed AST holds a finite `f64`. `PartialEq` is derived for
    /// convenience; callers must not compare two `Number` values expecting
    /// IEEE 754 NaN-equality — the invariant guarantees NaN is never stored,
    /// but the derive means two `NaN` values would compare unequal if the
    /// invariant were ever violated.
    Number(f64),
    /// A boolean literal: `true` or `false`
    Boolean(bool),
    /// The null literal
    Null,
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
    /// Variable or function call interpolation: `{name}` or `{greet("x")}`.
    Interpolation(Interpolation),
    /// An escaped brace: `\{` → literal `{`.
    EscapedBrace,
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

#[derive(Debug, Clone)]
pub struct IfBlock {
    /// The primary condition (`@if <condition>:`).
    pub condition: Condition,
    pub then_body: Vec<Node>,
    /// Zero or more `@elseif` branches, evaluated in order (short-circuit).
    pub elseif_branches: Vec<(Condition, Vec<Node>)>,
    pub else_body: Option<Vec<Node>>,
    pub offset: usize,
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
}

/// A parameter in a `@define` function definition.
///
/// Parameters may have an optional default value. Parameters without defaults
/// must appear before any parameter with a default.
#[derive(Debug, Clone)]
pub struct Param {
    /// The parameter name (a valid identifier).
    pub name: String,
    /// Optional default value, parsed as a `CondValue` at definition time.
    pub default: Option<CondValue>,
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
/// (those with `Some(CondValue)`) may be omitted at call sites and receive their
/// default value at runtime via `condvalue_to_value`.
///
/// Defined here alongside `Param` because it is purely a property of the AST
/// type — both the validator and evaluator import it from this module.
pub(crate) fn required_param_count(params: &[Param]) -> usize {
    params.iter().filter(|p| p.default.is_none()).count()
}

#[derive(Debug, Clone)]
pub struct DefineBlock {
    pub name: String,
    pub params: Vec<Param>,
    pub body: Vec<Node>,
    pub offset: usize,
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
    },
}

#[derive(Debug, Clone)]
pub enum ExportDirective {
    /// `@export name`
    Named { name: String },
    /// `@export name from "path"`
    ReExport { name: String, path: String },
    /// `@export * from "path"`
    Wildcard { path: String },
}

#[derive(Debug, Clone)]
pub struct IncludeDirective {
    pub alias: String,
    pub offset: usize,
}
