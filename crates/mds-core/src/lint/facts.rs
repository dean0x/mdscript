//! Facts walk: single-pass AST traversal building `AnalysisContext`.
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

use std::collections::HashSet;

use crate::ast::{
    Arg, Condition, DefineBlock, Expr, ForBlock, IfBlock, ImportDirective, IncludeDirective,
    Module, Node,
};
use crate::error::MdsError;
use crate::limits::MAX_NESTING_DEPTH;

// ── Fact types ─────────────────────────────────────────────────────────────────

/// Collected information about an import directive.
#[derive(Debug, Clone)]
pub struct ImportFact {
    /// Raw path string as written in source (not normalized).
    pub path: String,
    /// Import kind (Alias / Merge / Selective).
    pub kind: ImportKind,
    /// Alias name for `@import "path" as alias` forms.
    pub alias: Option<String>,
    /// Names for `@import { name1, name2 } from "path"` forms.
    pub names: Vec<String>,
    /// Byte offset of the `@import` token in the source.
    pub offset: usize,
}

/// Import kind discriminant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportKind {
    /// `@import "path" as alias`
    Alias,
    /// `@import "path"` (merge — injects all exports + `prompt` into scope)
    Merge,
    /// `@import { name1, name2 } from "path"`
    Selective,
}

/// Collected information about an export directive.
#[derive(Debug, Clone)]
pub struct ExportFact {
    /// Export kind (Named / ReExport / Wildcard).
    pub kind: ExportKind,
    /// Exported name for Named/ReExport variants.
    pub name: Option<String>,
    /// Path for ReExport/Wildcard variants.
    pub path: Option<String>,
    /// Byte offset of the `@export` token in the source (D2 offsets).
    pub offset: usize,
}

/// Export kind discriminant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportKind {
    /// `@export name`
    Named,
    /// `@export name from "path"`
    ReExport,
    /// `@export * from "path"`
    Wildcard,
}

/// Collected information about a `@define` block.
#[derive(Debug, Clone)]
pub struct DefineFact {
    /// Function name.
    pub name: String,
    /// Byte offset of the `@define` token in the source.
    pub offset: usize,
}

/// A frontmatter variable key fact with approximate source span.
#[derive(Debug, Clone)]
pub struct FmVarFact {
    /// The YAML key name.
    pub name: String,
    /// Approximate byte offset in the source (from FM content + substring search).
    /// `None` when the key cannot be located via substring search.
    pub approx_offset: Option<usize>,
}

/// A shadow variable pair: a name in an inner scope that shadows an outer-scope binding.
#[derive(Debug, Clone)]
pub struct ShadowPair {
    /// The shadowed variable name (same for inner and outer).
    pub name: String,
    /// Kind of the inner (shadowing) binding.
    pub inner_kind: ShadowKind,
    /// Kind of the outer (shadowed) binding.
    pub outer_kind: ShadowKind,
    /// Byte offset of the inner (shadowing) binding site.
    pub offset: usize,
}

/// Discriminant for what kind of binding is involved in a shadow relationship.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShadowKind {
    /// A frontmatter variable key.
    FmVar,
    /// An import alias (`@import "path" as alias`).
    ImportAlias,
    /// A `@for` loop variable.
    ForVar,
    /// A `@define` parameter.
    DefineParam,
}

// ── AnalysisContext ────────────────────────────────────────────────────────────

/// Pre-sized fact set built from a single AST traversal.
///
/// All fields are populated by `collect_facts`; rule functions read them
/// but never mutate them.
#[derive(Debug, Default)]
pub struct AnalysisContext {
    /// Whether the module has any explicit `@export` directives (affecting
    /// unused-function semantics — default-public modules export everything).
    pub has_explicit_exports: bool,

    /// Whether the module is a partial or @extends child (suppresses unused-* rules).
    pub is_partial_or_extends: bool,

    // ── Import/Export facts ───────────────────────────────────────────────────
    /// All import directives found in module body (top-level order).
    pub imports: Vec<ImportFact>,

    /// All export directives found in module body.
    pub exports: Vec<ExportFact>,

    // ── Define facts ─────────────────────────────────────────────────────────
    /// All `@define` blocks found in module body.
    pub defines: Vec<DefineFact>,

    // ── Frontmatter variable facts ────────────────────────────────────────────
    /// Frontmatter variable keys (excludes reserved keys: imports, type, extends, prompt).
    pub frontmatter_vars: Vec<FmVarFact>,

    // ── Usage tracking ────────────────────────────────────────────────────────
    /// Variable names referenced in body expressions
    /// (`Expr::Var`, `Expr::MemberAccess::object`, `Arg::Var`, `Arg::MemberAccess::object`).
    pub used_vars: HashSet<String>,

    /// Function names directly called
    /// (`Expr::Call::name`, `Arg::Call::name`).
    pub used_calls: HashSet<String>,

    /// Qualified call namespaces (`Expr::QualifiedCall::namespace`) — for alias usage.
    pub used_namespaces: HashSet<String>,

    /// @include alias names (`IncludeDirective::alias`) — for alias usage.
    pub used_include_aliases: HashSet<String>,

    // ── Shadow pairs ─────────────────────────────────────────────────────────
    /// Enumerated shadow variable pairs (for `shadow-variable` rule).
    pub shadow_pairs: Vec<ShadowPair>,
}

// ── Walk scope (private) ───────────────────────────────────────────────────────

/// Threading state for shadow-variable pair detection during the facts walk.
struct WalkScope {
    /// Frontmatter variable names (available at all times).
    fm_keys: HashSet<String>,
    /// Import alias names (available at module level).
    import_aliases: HashSet<String>,
    /// Stack of active @for loop variable names (innermost last).
    for_var_stack: Vec<String>,
    /// Stack of key variables (key, _) in @for loops.
    for_key_stack: Vec<String>,
    /// Active @define parameter names (only while inside a @define body).
    define_params: HashSet<String>,
}

impl WalkScope {
    fn new() -> Self {
        WalkScope {
            fm_keys: HashSet::new(),
            import_aliases: HashSet::new(),
            for_var_stack: Vec::new(),
            for_key_stack: Vec::new(),
            define_params: HashSet::new(),
        }
    }
}

// ── Facts walk ────────────────────────────────────────────────────────────────

/// Perform a single pre-sized traversal of `module.body`, collecting all facts
/// into an `AnalysisContext`.
///
/// `source` is the raw source string, used for frontmatter key span approximation.
///
/// Returns `Err(MdsError::ResourceLimit)` when the AST nesting exceeds
/// `MAX_NESTING_DEPTH` (defense against adversarially crafted inputs that would
/// exhaust the call stack in recursive rule implementations).
pub(super) fn collect_facts(
    module: &Module,
    is_partial_or_extends: bool,
    source: &str,
) -> Result<AnalysisContext, MdsError> {
    let mut ctx = AnalysisContext {
        is_partial_or_extends,
        ..Default::default()
    };

    // ── 1. Pre-collect frontmatter vars ─────────────────────────────────────
    if let Some(fm) = &module.frontmatter {
        collect_frontmatter_vars(fm, source, &mut ctx);
    }

    // ── 2. Build walk scope for shadow detection ─────────────────────────────
    let mut scope = WalkScope::new();
    for fv in &ctx.frontmatter_vars {
        scope.fm_keys.insert(fv.name.clone());
    }

    // ── 3. Pre-scan top-level imports into scope (for shadow detection of ForVar over ImportAlias)
    for node in &module.body {
        if let Node::Import(imp) = node {
            match imp {
                ImportDirective::Alias { alias, .. } => {
                    scope.import_aliases.insert(alias.clone());
                }
                ImportDirective::Merge { .. } | ImportDirective::Selective { .. } => {}
            }
        }
    }

    // ── 4. Main walk ─────────────────────────────────────────────────────────
    walk_nodes(&module.body, &mut ctx, &mut scope, 0)?;

    Ok(ctx)
}

/// Collect frontmatter variable keys from the raw YAML string.
///
/// Reserved keys (imports, type, extends, prompt) are excluded.
/// Approximate source offsets are computed via substring search in `source`.
fn collect_frontmatter_vars(fm: &crate::ast::Frontmatter, source: &str, ctx: &mut AnalysisContext) {
    // Reserved keys per Appendix A (unused-variable skip-set).
    const RESERVED: &[&str] = &["imports", "type", "extends", "prompt"];

    let yaml_result = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&fm.raw);
    let yaml = match yaml_result {
        Ok(v) => v,
        Err(_) => return, // malformed YAML — skip; resolver would have caught this
    };

    let mapping = match &yaml {
        serde_yaml_ng::Value::Mapping(m) => m,
        _ => return,
    };

    // Find the byte offset of the frontmatter content in the source.
    // Frontmatter starts after the first `---\n` line.
    let fm_content_start = find_frontmatter_content_start(source);

    for (key, _) in mapping {
        let serde_yaml_ng::Value::String(key_str) = key else {
            continue;
        };
        if RESERVED.contains(&key_str.as_str()) {
            continue;
        }

        // Approximate offset: search for `key_str:` at start of a line in fm content.
        let approx_offset =
            fm_content_start.and_then(|start| find_yaml_key_in_source(source, start, key_str));

        ctx.frontmatter_vars.push(FmVarFact {
            name: key_str.clone(),
            approx_offset,
        });
    }
}

/// Find the byte offset where frontmatter YAML content starts in `source`.
///
/// Returns `Some(offset)` pointing to the first byte after the opening `---\n`,
/// or `None` if no frontmatter fence is found.
fn find_frontmatter_content_start(source: &str) -> Option<usize> {
    // The frontmatter opens with `---\n` (or `---\r\n`; handle both).
    let fence_end = source
        .find("---\n")
        .map(|p| p + 4)
        .or_else(|| source.find("---\r\n").map(|p| p + 5))?;
    Some(fence_end)
}

/// Search for a YAML key name at the start of a line in the frontmatter region.
///
/// Returns the byte offset in `source` of the key's first character, or `None`
/// if not found.
fn find_yaml_key_in_source(source: &str, fm_start: usize, key: &str) -> Option<usize> {
    if fm_start >= source.len() {
        return None;
    }
    let fm_region = &source[fm_start..];
    let search = format!("{key}:");
    // Look for `key:` at start of a line (either at the very start or after `\n`).
    let mut search_from = 0;
    loop {
        let rel = fm_region[search_from..].find(search.as_str())?;
        let abs_rel = search_from + rel;
        // Check it's at a line boundary.
        let at_line_start = abs_rel == 0 || fm_region.as_bytes().get(abs_rel - 1) == Some(&b'\n');
        if at_line_start {
            return Some(fm_start + abs_rel);
        }
        // Advance past the whole matched substring. `search` ends with an ASCII `:`,
        // so `abs_rel + search.len()` always lands on a UTF-8 char boundary — advancing
        // by a single byte would slice mid-character and panic when `key` (or the byte
        // before the next match) is multi-byte (e.g. a non-ASCII frontmatter key that
        // also appears inside an earlier quoted value).
        search_from = abs_rel + search.len();
        if search_from >= fm_region.len() {
            return None;
        }
    }
}

/// Recursive body walker — bounded by `MAX_NESTING_DEPTH`.
///
/// Populates `ctx` with import/export/define/usage facts and shadow pairs.
fn walk_nodes(
    nodes: &[Node],
    ctx: &mut AnalysisContext,
    scope: &mut WalkScope,
    depth: usize,
) -> Result<(), MdsError> {
    if depth > MAX_NESTING_DEPTH {
        return Err(MdsError::resource_limit(format!(
            "template nesting exceeds maximum depth of {MAX_NESTING_DEPTH}"
        )));
    }

    for node in nodes {
        match node {
            // ── Import directive ─────────────────────────────────────────────
            Node::Import(imp) => {
                collect_import_fact(imp, ctx);
            }

            // ── Export directive ─────────────────────────────────────────────
            Node::Export(exp) => {
                ctx.has_explicit_exports = true;
                collect_export_fact(exp, ctx);
            }

            // ── Define block ─────────────────────────────────────────────────
            Node::Define(b) => {
                collect_define_fact(b, ctx, scope, depth)?;
            }

            // ── If block (recurse into all branches) ─────────────────────────
            Node::If(b) => {
                walk_if_block(b, ctx, scope, depth)?;
            }

            // ── For block ────────────────────────────────────────────────────
            Node::For(b) => {
                walk_for_block(b, ctx, scope, depth)?;
            }

            // ── Message block ─────────────────────────────────────────────────
            Node::Message(b) => {
                walk_nodes(&b.body, ctx, scope, depth + 1)?;
            }

            // ── Block (template inheritance placeholder) ──────────────────────
            Node::Block(b) => {
                walk_nodes(&b.body, ctx, scope, depth + 1)?;
            }

            // ── Interpolation ─────────────────────────────────────────────────
            Node::Interpolation(i) => {
                extract_expr_refs(&i.expr, ctx);
            }

            // ── Include directive ─────────────────────────────────────────────
            Node::Include(i) => {
                collect_include_ref(i, ctx);
            }

            // ── Leaf nodes (no children, no refs to collect here) ─────────────
            Node::Text(_) | Node::EscapedBrace => {}
        }
    }

    Ok(())
}

fn collect_import_fact(imp: &ImportDirective, ctx: &mut AnalysisContext) {
    match imp {
        ImportDirective::Alias {
            path,
            alias,
            offset,
        } => {
            ctx.imports.push(ImportFact {
                path: path.clone(),
                kind: ImportKind::Alias,
                alias: Some(alias.clone()),
                names: vec![],
                offset: *offset,
            });
        }
        ImportDirective::Merge { path, offset } => {
            ctx.imports.push(ImportFact {
                path: path.clone(),
                kind: ImportKind::Merge,
                alias: None,
                names: vec![],
                offset: *offset,
            });
        }
        ImportDirective::Selective {
            names,
            path,
            offset,
        } => {
            ctx.imports.push(ImportFact {
                path: path.clone(),
                kind: ImportKind::Selective,
                alias: None,
                names: names.clone(),
                offset: *offset,
            });
        }
    }
}

fn collect_export_fact(exp: &crate::ast::ExportDirective, ctx: &mut AnalysisContext) {
    use crate::ast::ExportDirective;
    match exp {
        ExportDirective::Named { name, offset } => {
            ctx.exports.push(ExportFact {
                kind: ExportKind::Named,
                name: Some(name.clone()),
                path: None,
                offset: *offset,
            });
        }
        ExportDirective::ReExport { name, path, offset } => {
            ctx.exports.push(ExportFact {
                kind: ExportKind::ReExport,
                name: Some(name.clone()),
                path: Some(path.clone()),
                offset: *offset,
            });
        }
        ExportDirective::Wildcard { path, offset } => {
            ctx.exports.push(ExportFact {
                kind: ExportKind::Wildcard,
                name: None,
                path: Some(path.clone()),
                offset: *offset,
            });
        }
    }
}

fn collect_define_fact(
    b: &DefineBlock,
    ctx: &mut AnalysisContext,
    scope: &mut WalkScope,
    depth: usize,
) -> Result<(), MdsError> {
    let param_names: Vec<String> = b.params.iter().map(|p| p.name.clone()).collect();

    ctx.defines.push(DefineFact {
        name: b.name.clone(),
        offset: b.offset,
    });

    // Shadow detection: @define params over FM keys or @for vars.
    for pname in &param_names {
        if scope.fm_keys.contains(pname) {
            ctx.shadow_pairs.push(ShadowPair {
                name: pname.clone(),
                inner_kind: ShadowKind::DefineParam,
                outer_kind: ShadowKind::FmVar,
                offset: b.offset,
            });
        }
        if scope.for_var_stack.contains(pname) || scope.for_key_stack.contains(pname) {
            ctx.shadow_pairs.push(ShadowPair {
                name: pname.clone(),
                inner_kind: ShadowKind::DefineParam,
                outer_kind: ShadowKind::ForVar,
                offset: b.offset,
            });
        }
    }

    // Recurse into @define body with params in scope.
    let old_params = std::mem::replace(&mut scope.define_params, param_names.into_iter().collect());
    walk_nodes(&b.body, ctx, scope, depth + 1)?;
    scope.define_params = old_params;

    Ok(())
}

fn walk_if_block(
    b: &IfBlock,
    ctx: &mut AnalysisContext,
    scope: &mut WalkScope,
    depth: usize,
) -> Result<(), MdsError> {
    extract_condition_refs(&b.condition, ctx);
    walk_nodes(&b.then_body, ctx, scope, depth + 1)?;
    for (cond, branch_body) in &b.elseif_branches {
        extract_condition_refs(cond, ctx);
        walk_nodes(branch_body, ctx, scope, depth + 1)?;
    }
    if let Some(else_body) = &b.else_body {
        walk_nodes(else_body, ctx, scope, depth + 1)?;
    }
    Ok(())
}

fn walk_for_block(
    b: &ForBlock,
    ctx: &mut AnalysisContext,
    scope: &mut WalkScope,
    depth: usize,
) -> Result<(), MdsError> {
    // Extract iterable expression refs.
    extract_expr_refs(&b.iterable, ctx);

    // Shadow detection: @for var over FM key, import alias, or outer @for var.
    check_for_var_shadow(&b.var, b.offset, scope, ctx);
    if let Some(kv) = &b.key_var {
        check_for_var_shadow(kv, b.offset, scope, ctx);
    }

    // Push @for vars onto scope stack for nested shadow detection.
    scope.for_var_stack.push(b.var.clone());
    if let Some(kv) = &b.key_var {
        scope.for_key_stack.push(kv.clone());
    }

    walk_nodes(&b.body, ctx, scope, depth + 1)?;

    scope.for_var_stack.pop();
    if b.key_var.is_some() {
        scope.for_key_stack.pop();
    }

    Ok(())
}

/// Check whether a @for loop variable shadows something in scope, and record the pair.
fn check_for_var_shadow(var: &str, offset: usize, scope: &WalkScope, ctx: &mut AnalysisContext) {
    if scope.fm_keys.contains(var) {
        ctx.shadow_pairs.push(ShadowPair {
            name: var.to_string(),
            inner_kind: ShadowKind::ForVar,
            outer_kind: ShadowKind::FmVar,
            offset,
        });
    } else if scope.import_aliases.contains(var) {
        ctx.shadow_pairs.push(ShadowPair {
            name: var.to_string(),
            inner_kind: ShadowKind::ForVar,
            outer_kind: ShadowKind::ImportAlias,
            offset,
        });
    } else if scope.for_var_stack.iter().any(|v| v == var)
        || scope.for_key_stack.iter().any(|v| v == var)
    {
        ctx.shadow_pairs.push(ShadowPair {
            name: var.to_string(),
            inner_kind: ShadowKind::ForVar,
            outer_kind: ShadowKind::ForVar,
            offset,
        });
    }
}

fn collect_include_ref(i: &IncludeDirective, ctx: &mut AnalysisContext) {
    ctx.used_include_aliases.insert(i.alias.clone());
}

// ── Expression reference extraction ──────────────────────────────────────────

/// Extract variable and function call references from an expression.
fn extract_expr_refs(expr: &Expr, ctx: &mut AnalysisContext) {
    match expr {
        Expr::Var(name) => {
            ctx.used_vars.insert(name.clone());
        }
        Expr::MemberAccess { object, .. } => {
            ctx.used_vars.insert(object.clone());
        }
        Expr::Call { name, args } => {
            ctx.used_calls.insert(name.clone());
            for arg in args {
                extract_arg_refs(arg, ctx);
            }
        }
        Expr::QualifiedCall {
            namespace, args, ..
        } => {
            ctx.used_namespaces.insert(namespace.clone());
            for arg in args {
                extract_arg_refs(arg, ctx);
            }
        }
        Expr::StringLiteral(_)
        | Expr::NumberLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NullLiteral => {}
    }
}

/// Extract variable and function call references from a function argument.
fn extract_arg_refs(arg: &Arg, ctx: &mut AnalysisContext) {
    match arg {
        Arg::Var(name) => {
            ctx.used_vars.insert(name.clone());
        }
        Arg::MemberAccess { object, .. } => {
            ctx.used_vars.insert(object.clone());
        }
        Arg::Call { name, args } => {
            ctx.used_calls.insert(name.clone());
            for a in args {
                extract_arg_refs(a, ctx);
            }
        }
        Arg::StringLiteral(_)
        | Arg::NumberLiteral(_)
        | Arg::BooleanLiteral(_)
        | Arg::NullLiteral => {}
    }
}

/// Extract variable and function call references from a condition.
fn extract_condition_refs(cond: &Condition, ctx: &mut AnalysisContext) {
    match cond {
        Condition::Truthy(expr) | Condition::Not(expr) => extract_expr_refs(expr, ctx),
        Condition::Eq(l, r) | Condition::NotEq(l, r) => {
            extract_expr_refs(l, ctx);
            extract_expr_refs(r, ctx);
        }
        Condition::And(conds) | Condition::Or(conds) => {
            for c in conds {
                extract_condition_refs(c, ctx);
            }
        }
    }
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

    fn facts(src: &str) -> AnalysisContext {
        let module = parse(src);
        let is_partial = crate::lint::is_partial_by_name("test.mds");
        let is_extends = module.extends.is_some();
        collect_facts(&module, is_partial || is_extends, src).unwrap()
    }

    #[test]
    fn collect_facts_empty_module() {
        let module = parse("Hello!\n");
        let ctx = collect_facts(&module, false, "Hello!\n").unwrap();
        assert!(!ctx.has_explicit_exports);
        assert!(!ctx.is_partial_or_extends);
        assert!(ctx.imports.is_empty());
        assert!(ctx.exports.is_empty());
    }

    #[test]
    fn collect_facts_detects_explicit_exports() {
        let src = "@define greet():\nhello\n@end\n@export greet\n";
        let module = parse(src);
        let ctx = collect_facts(&module, false, src).unwrap();
        assert!(ctx.has_explicit_exports, "should detect @export directive");
        assert_eq!(ctx.exports.len(), 1);
        assert_eq!(ctx.exports[0].name.as_deref(), Some("greet"));
    }

    #[test]
    fn collect_facts_partial_flag_propagated() {
        let module = parse("Hello!\n");
        let ctx = collect_facts(&module, true, "Hello!\n").unwrap();
        assert!(ctx.is_partial_or_extends);
    }

    /// AC-PERF-04: Nesting deeper than MAX_NESTING_DEPTH (64) returns ResourceLimit.
    #[test]
    fn collect_facts_depth_limit_enforced() {
        use crate::ast::{Condition, Expr, IfBlock, Module, Node, TextNode};

        let depth = MAX_NESTING_DEPTH + 1;
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

        let result = collect_facts(&module, false, "");
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

    // ── Import/export collection ──────────────────────────────────────────────

    #[test]
    fn collects_alias_import() {
        let src = "@import \"./utils.mds\" as utils\n";
        let ctx = facts(src);
        assert_eq!(ctx.imports.len(), 1);
        assert_eq!(ctx.imports[0].kind, ImportKind::Alias);
        assert_eq!(ctx.imports[0].alias.as_deref(), Some("utils"));
        assert_eq!(ctx.imports[0].path, "./utils.mds");
    }

    #[test]
    fn collects_merge_import() {
        let src = "@import \"./utils.mds\"\n";
        let ctx = facts(src);
        assert_eq!(ctx.imports.len(), 1);
        assert_eq!(ctx.imports[0].kind, ImportKind::Merge);
        assert!(ctx.imports[0].alias.is_none());
    }

    #[test]
    fn collects_selective_import() {
        let src = "@import { greet, farewell } from \"./utils.mds\"\n";
        let ctx = facts(src);
        assert_eq!(ctx.imports.len(), 1);
        assert_eq!(ctx.imports[0].kind, ImportKind::Selective);
        assert_eq!(ctx.imports[0].names, vec!["greet", "farewell"]);
    }

    #[test]
    fn collects_define_facts() {
        let src = "@define greet(name, suffix):\nhello {name}{suffix}\n@end\n";
        let ctx = facts(src);
        assert_eq!(ctx.defines.len(), 1);
        assert_eq!(ctx.defines[0].name, "greet");
    }

    #[test]
    fn collects_var_uses_from_interpolation() {
        let src = "{name}\n";
        let ctx = facts(src);
        assert!(
            ctx.used_vars.contains("name"),
            "should collect Expr::Var ref"
        );
    }

    #[test]
    fn collects_call_from_interpolation() {
        let src = "{greet(\"world\")}\n";
        let ctx = facts(src);
        assert!(ctx.used_calls.contains("greet"), "should collect Call ref");
    }

    #[test]
    fn collects_namespace_from_qualified_call() {
        let src = "@import \"./lib.mds\" as lib\n{lib.greet(\"world\")}\n";
        let ctx = facts(src);
        assert!(
            ctx.used_namespaces.contains("lib"),
            "should collect QualifiedCall namespace"
        );
    }

    #[test]
    fn collects_var_uses_from_condition() {
        let src = "@if role == \"admin\":\nhello\n@end\n";
        let ctx = facts(src);
        assert!(
            ctx.used_vars.contains("role"),
            "should collect var ref from condition"
        );
    }

    #[test]
    fn collects_var_uses_from_for_iterable() {
        let src = "@define greet(items):\n@for item in items:\n{item}\n@end\n@end\n";
        let ctx = facts(src);
        // `items` is referenced as @for iterable
        assert!(
            ctx.used_vars.contains("items"),
            "should collect @for iterable as var use"
        );
    }

    #[test]
    fn collects_include_aliases() {
        let src = "@import \"./lib.mds\" as lib\n@include lib\n";
        let ctx = facts(src);
        assert!(
            ctx.used_include_aliases.contains("lib"),
            "should collect @include alias"
        );
    }

    // ── Shadow pair detection ─────────────────────────────────────────────────

    #[test]
    fn detects_for_var_shadowing_fm_key() {
        let src = "---\nname: World\n---\n@for name in items:\n{name}\n@end\n";
        let ctx = facts(src);
        let shadow = ctx.shadow_pairs.iter().find(|p| {
            p.name == "name"
                && p.inner_kind == ShadowKind::ForVar
                && p.outer_kind == ShadowKind::FmVar
        });
        assert!(shadow.is_some(), "should detect @for var shadowing FM key");
    }

    #[test]
    fn detects_define_param_shadowing_fm_key() {
        let src = "---\nuser: Alice\n---\n@define greet(user):\nhello {user}\n@end\n";
        let ctx = facts(src);
        let shadow = ctx.shadow_pairs.iter().find(|p| {
            p.name == "user"
                && p.inner_kind == ShadowKind::DefineParam
                && p.outer_kind == ShadowKind::FmVar
        });
        assert!(
            shadow.is_some(),
            "should detect @define param shadowing FM key"
        );
    }

    // ── Frontmatter var collection ────────────────────────────────────────────

    #[test]
    fn collects_frontmatter_vars_excludes_reserved() {
        let src =
            "---\nname: World\ntype: mds\nimports: []\nextends: base\nprompt: hi\n---\nHello!\n";
        let ctx = facts(src);
        assert!(
            ctx.frontmatter_vars.iter().any(|v| v.name == "name"),
            "should collect 'name' var"
        );
        for reserved in &["type", "imports", "extends", "prompt"] {
            assert!(
                !ctx.frontmatter_vars.iter().any(|v| &v.name == reserved),
                "should exclude reserved key '{reserved}'"
            );
        }
    }

    /// Regression: a non-ASCII frontmatter key that ALSO appears inside an earlier
    /// quoted value must not panic the key-offset search. The old `abs_rel + 1`
    /// advance sliced mid-character (`byte index N is not a char boundary`).
    #[test]
    fn non_ascii_frontmatter_key_does_not_panic() {
        let src = "---\nintro: \"say évar: now\"\névar: 1\n---\nHello\n";
        // Must not panic during the facts walk.
        let ctx = facts(src);
        assert!(
            ctx.frontmatter_vars.iter().any(|v| v.name == "évar"),
            "should collect the non-ASCII frontmatter key without panicking"
        );
    }
}
