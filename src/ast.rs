/// AST node types for MDS.

/// A complete MDS source file.
#[derive(Debug, Clone)]
pub struct Module {
    pub frontmatter: Option<Frontmatter>,
    pub body: Vec<Node>,
}

/// YAML frontmatter block.
#[derive(Debug, Clone)]
pub struct Frontmatter {
    pub raw: String,
    pub offset: usize,
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
}

#[derive(Debug, Clone)]
pub struct TextNode {
    pub text: String,
    pub offset: usize,
}

/// An interpolation expression inside `{ }`.
#[derive(Debug, Clone)]
pub struct Interpolation {
    pub expr: Expr,
    pub offset: usize,
    pub len: usize,
}

/// Expressions that can appear inside `{ }`.
#[derive(Debug, Clone)]
pub enum Expr {
    /// Simple variable reference: `{name}`
    Var(String),
    /// Function call: `{greet("Alice")}` or `{greet(name)}`
    Call {
        name: String,
        args: Vec<Arg>,
    },
    /// Qualified call: `{utils.greet("Alice")}`
    QualifiedCall {
        namespace: String,
        name: String,
        args: Vec<Arg>,
    },
}

/// A function argument — either a string literal or a variable reference.
#[derive(Debug, Clone)]
pub enum Arg {
    StringLiteral(String),
    Var(String),
}

#[derive(Debug, Clone)]
pub struct IfBlock {
    pub condition: String,
    pub then_body: Vec<Node>,
    pub else_body: Option<Vec<Node>>,
    pub offset: usize,
}

#[derive(Debug, Clone)]
pub struct ForBlock {
    pub var: String,
    pub iterable: String,
    pub body: Vec<Node>,
    pub offset: usize,
}

#[derive(Debug, Clone)]
pub struct DefineBlock {
    pub name: String,
    pub params: Vec<String>,
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
    Merge {
        path: String,
        offset: usize,
    },
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
    Named {
        name: String,
        offset: usize,
    },
    /// `@export name from "path"`
    ReExport {
        name: String,
        path: String,
        offset: usize,
    },
    /// `@export * from "path"`
    Wildcard {
        path: String,
        offset: usize,
    },
}

#[derive(Debug, Clone)]
pub struct IncludeDirective {
    pub alias: String,
    pub offset: usize,
}
