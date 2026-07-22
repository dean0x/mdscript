use std::sync::Arc;

use crate::arity::check_arity;
use crate::ast::{
    render_signature, required_param_count, Arg, BlockNode, Condition, Expr, ForBlock, IfBlock,
    IncludeDirective, MessageBlock, Node,
};
use crate::error::MdsError;
use crate::limits::{
    MAX_DOT_SEGMENTS, MAX_ELSEIF_BRANCHES, MAX_MESSAGES_TOTAL_SIZE, MAX_MESSAGE_COUNT,
    MAX_OUTPUT_SIZE,
};
use crate::scope::{FunctionDef, Scope};
use crate::value::Value;

/// Maximum call depth to prevent stack overflow from deeply nested calls.
const MAX_CALL_DEPTH: usize = 128;

/// Maximum number of iterations allowed in a single @for loop.
const MAX_LOOP_ITERATIONS: usize = 100_000;

/// Maximum total iterations across all loops in a single compilation.
/// This prevents nested loops from multiplying iterations into the billions.
const MAX_TOTAL_ITERATIONS: usize = 1_000_000;

/// Maximum number of accumulated warnings before further warnings are silently dropped.
const MAX_WARNINGS: usize = 1_000;

/// Transient state threaded through the evaluator for a single compilation.
///
/// Bundles the mutable parameters that were previously threaded individually
/// through every function signature, reducing arity and making call sites clearer.
pub(crate) struct EvalContext<'a> {
    /// LIFO stack of active function call keys, used to detect direct recursion.
    /// Vec is used for O(n) contains at MAX_CALL_DEPTH=128 — acceptable.
    call_stack: Vec<String>,
    /// Cumulative loop iterations across all @for loops in one compilation.
    total_iterations: usize,
    /// Running byte total of all message content pushed in messages mode.
    /// Checked incrementally so runaway growth is caught before building a giant Vec.
    total_message_bytes: usize,
    /// Accumulated warnings (e.g. empty @include).
    warnings: &'a mut Vec<String>,
    /// Optional source-map builder.  Present when `CompileOptions::source_map` is
    /// `true`.  `None` when the caller did not request a source map — zero-cost path
    /// (AC-PERF-01: no allocation when off).
    pub(crate) map: Option<crate::sourcemap::MapBuilder>,
    /// Per-include-site remap cache for source-map splice (S6 / AC-PERF-05).
    ///
    /// Maps `Arc<FragmentMap>` pointer identity (as `usize`) to the
    /// local-source-index → global-`MapBuilder`-source-index remap `Vec`.
    ///
    /// When `@include` appears inside a `@for` loop, the remap is built on the
    /// first iteration and retrieved from this cache on every subsequent
    /// iteration — O(1) map lookup instead of re-running `source_index` scans.
    ///
    /// The cache is scoped to one `evaluate_with_map` invocation and is empty at
    /// the start of each top-level compile; no cross-compilation state leaks.
    pub(crate) fragment_remap_cache: std::collections::HashMap<usize, Vec<u32>>,
    /// S8 signal: set to `true` by `invoke_function` when it takes the fine-grained
    /// body-descent path (S8), so the calling `Node::Interpolation` arm knows the body
    /// already recorded its own segments and the call-site point should be skipped.
    ///
    /// Reset to `false` at the start of each `invoke_function` call (prevents
    /// arg-evaluation side-effects from leaking into the outer Interpolation arm)
    /// and at the start of each `Node::Interpolation` arm.
    ///
    /// Irrelevant (always `false`) when `map` is `None` — zero-cost path.
    fn_body_owned: bool,
    /// Display path of the current module, used for `type_mismatch_at` spans.
    ///
    /// Matches the `file_str` field in `ModuleCtx` (resolver.rs).  Set to `""`
    /// on the simple `evaluate()` path where no file context is available —
    /// the `build_type_mismatch` helper degrades to spanless when this is empty
    /// (ADR-005 / DECISIONS_CONTEXT: degrade rather than mis-attribute).
    pub(crate) file: &'a str,
    /// Raw source of the current module, used for span byte-offset computation.
    ///
    /// Set to `""` when no source context is threaded in (e.g. unit-test paths
    /// that call `evaluate()` directly).  The `build_type_mismatch` helper checks
    /// `source.is_empty()` before attempting span construction.
    pub(crate) source: &'a str,
    /// Provenance of the *currently evaluating* function body (B1 — applies PF-012).
    ///
    /// Set to `func.origin.clone()` by `invoke_function` when entering a body and
    /// restored via `std::mem::replace` on ALL exit paths (LIFO-correct for nested
    /// cross-file define chains: A calls B calls C → C's origin is innermost).
    ///
    /// `build_type_mismatch` selects `(file, source)` from this when `Some`, so
    /// a type mismatch raised inside an imported `@define` body names the helper
    /// file rather than the caller's file (avoids the PF-012 mis-attribution class).
    ///
    /// `None` when not currently evaluating any function body.
    body_origin: Option<crate::sourcemap::Origin>,
}

/// Evaluate a module body into a final rendered string.
///
/// Warnings (e.g. empty `@include`) are appended to `warnings`.
/// `file` and `source` are the display path and raw source of the module being
/// evaluated; they are threaded into `EvalContext` so that diagnostics raised
/// during condition evaluation (e.g. `mds::type_mismatch`) can carry source spans.
/// Pass `""` for both when no source context is available (e.g. in unit tests).
pub fn evaluate(
    nodes: &[Node],
    scope: &mut Scope,
    warnings: &mut Vec<String>,
    file: &str,
    source: &str,
) -> Result<String, MdsError> {
    let mut ctx = EvalContext {
        call_stack: Vec::new(),
        total_iterations: 0,
        total_message_bytes: 0,
        warnings,
        map: None,
        fragment_remap_cache: std::collections::HashMap::new(),
        fn_body_owned: false,
        file,
        source,
        body_origin: None,
    };
    evaluate_nodes(nodes, scope, &mut ctx)
}

/// Evaluate a module body into a rendered string while recording source-map
/// segments via `builder`.
///
/// Returns `(output, builder)` so the caller can pass the builder on to the
/// finalization stage.  The builder's `cursor` is guaranteed to equal
/// `output.len() as u32` when this function returns.
///
/// `file` and `source` for `EvalContext` diagnostic spans are derived from
/// `builder.current_src` — single source of truth, no redundant parameter
/// (avoids the dual-channel mis-attribution class fixed in c5a4d65; issue #58).
///
/// Delegates to [`evaluate_with_map_seeded`] with seed counters of 0.
/// Use [`evaluate_with_map_seeded`] directly when you need to carry cumulative
/// resource budgets across multiple invocations (e.g. `@extends` regions).
pub(crate) fn evaluate_with_map(
    nodes: &[Node],
    scope: &mut Scope,
    warnings: &mut Vec<String>,
    builder: crate::sourcemap::MapBuilder,
) -> Result<(String, crate::sourcemap::MapBuilder), MdsError> {
    let (output, map, _, _) = evaluate_with_map_seeded(nodes, scope, warnings, builder, 0, 0)?;
    Ok((output, map))
}

/// Evaluate nodes with source-map recording and pre-seeded cumulative resource budgets.
///
/// Like [`evaluate_with_map`] but additionally accepts `seed_iterations` and
/// `seed_msg_bytes` to continue cumulative budgets from prior work (e.g. earlier
/// `@extends` regions).  Returns `(output, builder, total_iterations, total_msg_bytes)`
/// so the caller can thread the running totals into the next invocation.
///
/// `file` and `source` for `EvalContext` diagnostic spans are derived from
/// `builder.current_src` — the single source of truth.  Callers must update
/// `builder.current_src` before each call (as `evaluate_regions_with_map` does)
/// so that the derived values reflect the correct region origin (issue #58;
/// avoids the dual-channel mis-attribution class fixed in c5a4d65).
///
/// Used by `evaluate_regions_with_map` (resolver.rs) to enforce a single cumulative
/// iteration cap across all spliced `@extends` regions (REL-1, applies PF-004).
///
/// The `MapBuilder` is returned as a structured error rather than a panic if it
/// disappears — aligns with PF-005 (don't rely on panic for invariants).
pub(crate) fn evaluate_with_map_seeded(
    nodes: &[Node],
    scope: &mut Scope,
    warnings: &mut Vec<String>,
    builder: crate::sourcemap::MapBuilder,
    seed_iterations: usize,
    seed_msg_bytes: usize,
) -> Result<(String, crate::sourcemap::MapBuilder, usize, usize), MdsError> {
    // Clone file/source from the builder's current source entry before moving
    // builder into EvalContext.map.  This is the single source of truth for
    // diagnostic span attribution — eliminates the redundant explicit params
    // that caused mis-attributed spans before c5a4d65 (issue #58).
    let file_owned = builder
        .sources
        .get(builder.current_src as usize)
        .cloned()
        .unwrap_or_default();
    let source_owned = builder
        .sources_content
        .get(builder.current_src as usize)
        .cloned()
        .unwrap_or_default();
    let mut ctx = EvalContext {
        call_stack: Vec::new(),
        total_iterations: seed_iterations,
        total_message_bytes: seed_msg_bytes,
        warnings,
        map: Some(builder),
        fragment_remap_cache: std::collections::HashMap::new(),
        fn_body_owned: false,
        file: &file_owned,
        source: &source_owned,
        body_origin: None,
    };
    let output = evaluate_nodes(nodes, scope, &mut ctx)?;
    let map = ctx.map.take().ok_or_else(|| {
        MdsError::syntax("internal: MapBuilder disappeared during evaluate_with_map — compiler bug")
    })?;
    Ok((output, map, ctx.total_iterations, ctx.total_message_bytes))
}

fn evaluate_nodes(
    nodes: &[Node],
    scope: &mut Scope,
    ctx: &mut EvalContext,
) -> Result<String, MdsError> {
    let mut output = String::new();

    // `saved_cursor` anchors the absolute output position when this invocation
    // of evaluate_nodes started.  All segment `out` values are computed as
    // `saved_cursor + output.len()` so that segments in recursive calls (e.g.
    // inside @if/@for arms) carry the correct absolute byte offset in the final
    // compiled output even though each call has its own local `output` string.
    //
    // We read cursor once here (O(1)) rather than on every node to avoid
    // repeated borrows of ctx.map inside the loop body.
    let saved_cursor: u32 = ctx.map.as_ref().map_or(0, |m| m.cursor);

    for node in nodes {
        match node {
            Node::Text(t) => {
                if let Some(ref mut map) = ctx.map {
                    if map.suppress == 0 {
                        let abs_out = saved_cursor + output.len() as u32;
                        debug_assert_eq!(
                            map.cursor, abs_out,
                            "cursor invariant violated at Text offset={}",
                            t.offset
                        );
                        map.push_segment(abs_out, t.offset as u32, t.text.len() as u32);
                    }
                }
                output.push_str(&t.text);
                if let Some(ref mut map) = ctx.map {
                    map.cursor = saved_cursor + output.len() as u32;
                }
            }
            Node::EscapedBrace { offset } => {
                if let Some(ref mut map) = ctx.map {
                    if map.suppress == 0 {
                        let abs_out = saved_cursor + output.len() as u32;
                        debug_assert_eq!(
                            map.cursor, abs_out,
                            "cursor invariant violated at EscapedBrace offset={offset}"
                        );
                        // Source span: `\{{` is 3 source bytes (backslash + two braces).
                        map.push_segment(abs_out, *offset as u32, 3);
                    }
                }
                output.push_str("{{");
                if let Some(ref mut map) = ctx.map {
                    map.cursor = saved_cursor + output.len() as u32;
                }
            }
            Node::Interpolation(interp) => {
                // S8: reset ownership flag before each call so arg-evaluation
                // side-effects from nested S8 calls don't corrupt the outer arm.
                ctx.fn_body_owned = false;
                if let Some(ref mut map) = ctx.map {
                    if map.suppress == 0 {
                        // Assert cursor invariant before anchoring.  On the S8 path,
                        // invoke_function will read this anchor as the body's base
                        // output position.
                        let abs_out = saved_cursor + output.len() as u32;
                        debug_assert_eq!(
                            map.cursor, abs_out,
                            "cursor invariant violated at Interpolation offset={}",
                            interp.offset
                        );
                    }
                    // Always anchor cursor so inner evaluate_nodes invocations
                    // (invoke_function / S8 body descent) start from the correct
                    // absolute position.
                    map.cursor = saved_cursor + output.len() as u32;
                }
                let rendered = render_expr(&interp.expr, scope, ctx)?;
                if let Some(ref mut map) = ctx.map {
                    if map.suppress == 0 && !ctx.fn_body_owned {
                        // S3 path: body was suppressed; record the call-site span.
                        // On the S8 path, invoke_function already pushed fine-grained
                        // body segments attributed to the definition file — skip here.
                        let abs_out = saved_cursor + output.len() as u32;
                        map.push_segment(abs_out, interp.offset as u32, interp.len as u32);
                    }
                }
                output.push_str(&rendered);
                // Correct cursor after render_expr.  For S8, invoke_function already
                // set cursor to start_cursor + trimmed_len; this final assignment
                // re-anchors it to the outer output length (same value after push_str).
                if let Some(ref mut map) = ctx.map {
                    map.cursor = saved_cursor + output.len() as u32;
                }
            }
            Node::If(block) => {
                if let Some(ref mut map) = ctx.map {
                    map.cursor = saved_cursor + output.len() as u32;
                }
                output.push_str(&evaluate_if(block, scope, ctx)?);
                if let Some(ref mut map) = ctx.map {
                    map.cursor = saved_cursor + output.len() as u32;
                }
            }
            Node::For(block) => {
                if let Some(ref mut map) = ctx.map {
                    map.cursor = saved_cursor + output.len() as u32;
                }
                output.push_str(&evaluate_for(block, scope, ctx)?);
                if let Some(ref mut map) = ctx.map {
                    map.cursor = saved_cursor + output.len() as u32;
                }
            }
            Node::Define(_) => {
                // Handled by resolver with full lexical capture; no output.
            }
            Node::Import(_) | Node::Export(_) => {
                // Handled by resolver, skip during evaluation.
            }
            Node::Include(inc) => {
                if let Some(ref mut map) = ctx.map {
                    map.cursor = saved_cursor + output.len() as u32;
                }
                output.push_str(&evaluate_include(inc, scope, ctx)?);
                if let Some(ref mut map) = ctx.map {
                    map.cursor = saved_cursor + output.len() as u32;
                }
            }
            Node::Message(block) => {
                // Text mode: render the body inline, ignoring the role marker.
                // This maintains full backward compatibility — @message blocks are
                // transparent when compiling to plain text.
                if let Some(ref mut map) = ctx.map {
                    map.cursor = saved_cursor + output.len() as u32;
                }
                output.push_str(&evaluate_nodes(&block.body, scope, ctx)?);
                if let Some(ref mut map) = ctx.map {
                    map.cursor = saved_cursor + output.len() as u32;
                }
            }
            Node::Block(block) => {
                // Standalone mode: render the block's default body inline.
                // The block markers are invisible in text output — @block is a
                // transparent wrapper in non-extending files.
                // The resolver splices child overrides before evaluation, so this arm
                // handles both base defaults and child-override bodies.
                if let Some(ref mut map) = ctx.map {
                    map.cursor = saved_cursor + output.len() as u32;
                }
                output.push_str(&evaluate_block(block, scope, ctx)?);
                if let Some(ref mut map) = ctx.map {
                    map.cursor = saved_cursor + output.len() as u32;
                }
            }
        }
        if output.len() > MAX_OUTPUT_SIZE {
            return Err(MdsError::resource_limit(format!(
                "output exceeds maximum size of {} bytes",
                MAX_OUTPUT_SIZE
            )));
        }
    }

    Ok(output)
}

/// Render a `@block` node's body inline.
///
/// In standalone mode (no `@extends`) the block body is the default content and is
/// rendered as plain text — the `@block` markers are invisible.
fn evaluate_block(
    block: &BlockNode,
    scope: &mut Scope,
    ctx: &mut EvalContext,
) -> Result<String, MdsError> {
    evaluate_nodes(&block.body, scope, ctx)
}

/// Resolve a dot-separated path against the current scope.
///
/// `root` is the root variable name; `fields` are the field names to traverse into the
/// resolved root value. Passing an empty `fields` slice returns the root variable itself.
/// Returns `Ok(Value)` with the resolved value, or an error if the path is invalid.
fn resolve_dot_path(root: &str, fields: &[String], scope: &Scope) -> Result<Value, MdsError> {
    if fields.len() > MAX_DOT_SEGMENTS {
        return Err(MdsError::syntax(format!(
            "dot path exceeds maximum segment count of {MAX_DOT_SEGMENTS}"
        )));
    }
    let mut current = scope
        .get_var(root)
        .cloned()
        .ok_or_else(|| MdsError::undefined_var(root))?;
    for (i, field) in fields.iter().enumerate() {
        // Build the traversed path for error messages (e.g. "a.b" when failing at "c").
        let traversed_path = || {
            std::iter::once(root)
                .chain(fields[..i].iter().map(|s| s.as_str()))
                .collect::<Vec<_>>()
                .join(".")
        };
        match current {
            Value::Object(ref map) => {
                current = map.get(field).cloned().ok_or_else(|| {
                    MdsError::syntax(format!(
                        "field '{field}' not found on '{}'",
                        traversed_path()
                    ))
                })?;
            }
            _ => {
                return Err(MdsError::syntax(format!(
                    "cannot access field '{field}' on {} '{}'",
                    current.type_name(),
                    traversed_path()
                )));
            }
        }
    }
    Ok(current)
}

/// Evaluate an expression to its runtime `Value`.
///
/// This is the core expression evaluator used by both interpolation (via `render_expr`)
/// and directive conditions/iterables (via `evaluate_condition` and `evaluate_for`).
fn evaluate_expr(expr: &Expr, scope: &mut Scope, ctx: &mut EvalContext) -> Result<Value, MdsError> {
    match expr {
        Expr::Var(name) => scope
            .get_var(name)
            .cloned()
            .ok_or_else(|| MdsError::undefined_var(name)),
        Expr::Call { name, args } => {
            let resolved_args = resolve_args(args, scope, ctx, 0)?;
            call_function(name, &resolved_args, scope, ctx)
        }
        Expr::QualifiedCall {
            namespace,
            name,
            args,
        } => {
            let resolved_args = resolve_args(args, scope, ctx, 0)?;
            call_qualified_function(namespace, name, &resolved_args, scope, ctx)
        }
        Expr::MemberAccess { object, fields } => {
            // Give a targeted error when the name refers to an imported namespace rather
            // than a variable — before delegating to resolve_dot_path which only looks up vars.
            if scope.get_var(object).is_none() && scope.get_namespace(object).is_some() {
                return Err(MdsError::syntax(format!(
                    "'{object}' is an imported module, not a variable — to call a function use {{{object}.func()}}"
                )));
            }
            resolve_dot_path(object, fields, scope)
        }
        Expr::StringLiteral(s) => Ok(Value::String(s.clone())),
        Expr::NumberLiteral(n) => Ok(Value::Number(*n)),
        Expr::BooleanLiteral(b) => Ok(Value::Boolean(*b)),
        Expr::NullLiteral => Ok(Value::Null),
    }
}

/// Render an expression to a string for interpolation output.
///
/// Thin wrapper around `evaluate_expr` that enforces the rule that objects
/// cannot be directly interpolated — the user must access a specific field.
fn render_expr(expr: &Expr, scope: &mut Scope, ctx: &mut EvalContext) -> Result<String, MdsError> {
    let value = evaluate_expr(expr, scope, ctx)?;
    match &value {
        Value::Object(_) => {
            // Build a descriptive error depending on the expression type
            let hint = match expr {
                Expr::Var(name) => format!(
                    "cannot interpolate object '{name}' directly, access a specific field with dot notation (e.g. {{{{{name}.field}}}})"
                ),
                Expr::MemberAccess { object, fields } => format!(
                    "cannot interpolate object directly, access a specific field with dot notation (e.g. {{{{{object}.{field}}}}})",
                    field = fields.last().map_or("field", |f| f.as_str())
                ),
                _ => "cannot interpolate an object value directly".to_string(),
            };
            Err(MdsError::syntax(hint))
        }
        _ => Ok(value.to_string()),
    }
}

fn resolve_args(
    args: &[Arg],
    scope: &mut Scope,
    ctx: &mut EvalContext,
    depth: usize,
) -> Result<Vec<Value>, MdsError> {
    if depth > MAX_CALL_DEPTH {
        return Err(MdsError::recursion(format!(
            "argument expression depth exceeds {MAX_CALL_DEPTH}"
        )));
    }
    args.iter()
        .map(|arg| match arg {
            Arg::StringLiteral(s) => Ok(Value::String(s.clone())),
            Arg::NumberLiteral(n) => Ok(Value::Number(*n)),
            Arg::BooleanLiteral(b) => Ok(Value::Boolean(*b)),
            Arg::NullLiteral => Ok(Value::Null),
            Arg::Var(name) => scope
                .get_var(name)
                .cloned()
                .ok_or_else(|| MdsError::undefined_var(name)),
            Arg::Call {
                name,
                args: inner_args,
            } => {
                let resolved = resolve_args(inner_args, scope, ctx, depth + 1)?;
                call_function(name, &resolved, scope, ctx)
            }
            Arg::MemberAccess { object, fields } => resolve_dot_path(object, fields, scope),
        })
        .collect()
}

/// On double-fault, prefer the first error over the second.
///
/// Used when a render error AND an internal pop/LIFO error occur simultaneously:
/// the render error carries the actionable source-span diagnostic for the user,
/// while pop/LIFO failures are compiler bugs that surface as secondary errors.
fn prefer_first_error<T>(
    first: Result<T, MdsError>,
    second: Result<(), MdsError>,
) -> Result<T, MdsError> {
    match (first, second) {
        (Err(first_err), _) => Err(first_err),
        (Ok(_), Err(second_err)) => Err(second_err),
        (Ok(val), Ok(())) => Ok(val),
    }
}

/// Convert a literal `Expr` (a compile-time `@define` parameter default) to a
/// runtime `Value`.
///
/// `Param.default` is always one of the four literal `Expr` variants —
/// `parse_define_params` rejects non-literal defaults at parse time (see
/// `parse_cond_value`). The non-literal arms therefore encode that parser
/// invariant as a defensive boundary assertion: rather than evaluating an
/// arbitrary expression (which would need a `Scope`/`EvalContext` and could
/// resolve a variable or call a function — semantics defaults must never have),
/// a non-literal default is reported as an internal error. This keeps the
/// produced `Value`s byte-identical to the prior dedicated literal-to-value
/// conversion for every well-formed AST.
pub(crate) fn literal_expr_to_value(expr: &Expr) -> Result<Value, MdsError> {
    match expr {
        Expr::StringLiteral(s) => Ok(Value::String(s.clone())),
        Expr::NumberLiteral(n) => Ok(Value::Number(*n)),
        Expr::BooleanLiteral(b) => Ok(Value::Boolean(*b)),
        Expr::NullLiteral => Ok(Value::Null),
        Expr::Var(_)
        | Expr::Call { .. }
        | Expr::QualifiedCall { .. }
        | Expr::MemberAccess { .. } => Err(MdsError::syntax(
            "internal error: non-literal expression in parameter default position",
        )),
    }
}

/// Restore the closure captures recorded at function-definition time into the
/// current scope frame.
///
/// Namespaces, sibling functions, and variables captured from the definition
/// site are written into `scope` before parameter binding so that params shadow
/// captured vars correctly (params take precedence over closure variables).
fn restore_captured_scope(func: &FunctionDef, scope: &mut Scope) {
    for (alias, ns) in &func.captured.namespaces {
        scope.set_namespace(alias, ns.clone());
    }
    // captured.functions are owned FunctionDef (not Arc) — wrap in Arc for scope insertion.
    for (name, f) in &func.captured.functions {
        scope.set_function(name, Arc::new(f.clone()));
    }
    for (name, val) in &func.captured.vars {
        scope.set_var(name, val.clone());
    }
}

fn invoke_function(
    func: &FunctionDef,
    call_key: &str,
    args: &[Value],
    scope: &mut Scope,
    ctx: &mut EvalContext,
) -> Result<String, MdsError> {
    // Reset fn_body_owned so arg-evaluation side-effects (from inner S8 calls
    // in resolve_args) don't corrupt the outer Interpolation arm's decision.
    ctx.fn_body_owned = false;

    if ctx.call_stack.iter().any(|s| s == call_key) {
        return Err(MdsError::recursion(call_key));
    }
    if ctx.call_stack.len() >= MAX_CALL_DEPTH {
        return Err(MdsError::recursion(format!(
            "{call_key} (call depth exceeds {MAX_CALL_DEPTH})"
        )));
    }
    let required = required_param_count(&func.params);
    let total = func.params.len();
    if !check_arity(args.len(), required, total) {
        return Err(MdsError::arity(
            call_key,
            required,
            total,
            args.len(),
            Some(render_signature(call_key, &func.params)),
        ));
    }
    scope.push();
    // Restore captured lexical scope from definition site so the function body
    // can resolve alias imports, sibling functions, and frontmatter variables
    // from its defining module.
    restore_captured_scope(func, scope);
    // Bind params index-by-index; fill missing optional params with their defaults.
    for (i, param) in func.params.iter().enumerate() {
        let value = if i < args.len() {
            args[i].clone()
        } else {
            // Required params were already checked above; this arm only fires for
            // optional params not supplied by the caller.
            let default = param.default.as_ref().ok_or_else(|| {
                MdsError::syntax(format!(
                    "internal error: non-optional param '{}' missing but arity check passed",
                    param.name
                ))
            })?;
            literal_expr_to_value(default)?
        };
        scope.set_var(&param.name, value);
    }

    // B1 (applies PF-012): swap body_origin to the function's defining file so
    // build_type_mismatch inside the body attributes spans to the correct source.
    // Restore on ALL exit paths via explicit restore in both S8 and S3 arms
    // (LIFO-correct for nested cross-file define chains: A calls B calls C →
    // C's origin is innermost, A's is restored when C's call returns, etc.).
    let prev_body_origin = std::mem::replace(&mut ctx.body_origin, func.origin.clone());

    // S8 path: fine-grained function body source attribution.
    //
    // Conditions for S8 (all must hold):
    //  1. A map is present (source-map mode active, AC-PERF-01).
    //  2. suppress == 0 — we are not already inside a suppressed body.
    //  3. func.origin is Some — the function has provenance metadata (populated
    //     unconditionally since B1 dropped the source_map_mode gate, S7).
    //
    // On this path, instead of suppressing the body, we:
    //  - Switch MapBuilder::current_src to the definition file.
    //  - Evaluate the body without suppression so each leaf node pushes its own
    //    segment attributed to the definition file.
    //  - Run rebase_trim to drop segments in the trimmed-away leading/trailing
    //    whitespace regions and shift surviving out-values by `lead`.
    //  - Advance cursor to reflect the trimmed output length.
    //  - Set fn_body_owned=true so the calling Interpolation arm skips its
    //    call-site segment.
    let use_s8 = ctx
        .map
        .as_ref()
        .is_some_and(|m| m.suppress == 0 && func.origin.is_some());

    if use_s8 {
        // use_s8 := ctx.map.is_some() && func.origin.is_some() — bind both once so the
        // body avoids six scattered .expect()/.unwrap() re-derivations (RUST-1 / PF-005).
        // The else branch is unreachable by invariant but returns a structured error
        // rather than panicking, aligning with PF-005.
        let (start_cursor, seg_start, saved_src) =
            if let (Some(map), Some(origin)) = (ctx.map.as_mut(), func.origin.as_ref()) {
                // Snapshot pre-body state and register the definition file in one binding.
                let sc = map.cursor;
                let ss = map.segments.len();
                let sr = map.current_src;
                let def_src = map.source_index(origin.file.as_ref(), origin.source.as_ref());
                map.current_src = def_src;
                (sc, ss, sr)
                // map and origin borrows released here; ctx is usable below.
            } else {
                // Unreachable: use_s8 verified both are Some above.
                // Restore body_origin before early return to maintain LIFO invariant.
                ctx.body_origin = prev_body_origin;
                return Err(MdsError::syntax(
                    "internal: S8 invariant violated — map or origin unexpectedly None",
                ));
            };

        ctx.call_stack.push(call_key.to_string());
        let body_result = evaluate_nodes(&func.body, scope, ctx);
        let popped = ctx.call_stack.pop();

        // Restore source attribution for the outer scope.
        if let Some(ref mut map) = ctx.map {
            map.current_src = saved_src;
        }
        // B1: restore body_origin before `?` so LIFO is correct even on error paths.
        ctx.body_origin = prev_body_origin;

        let lifo_result = if popped.as_deref() == Some(call_key) {
            Ok(())
        } else {
            Err(MdsError::syntax(format!(
                "internal error: call_stack LIFO violated: \
                 expected '{call_key}', got {popped:?}"
            )))
        };
        let pop_result = scope.pop();
        let raw_body = prefer_first_error(body_result, lifo_result.and(pop_result))?;

        // Compute lead/trail byte counts for rebase_trim.
        // Use try_from + unwrap_or(u32::MAX) for safety: body is bounded by
        // MAX_OUTPUT_SIZE (50 MB) so try_from always succeeds in practice;
        // u32::MAX fallback causes rebase_trim to drop all segments (safe degradation).
        let lead = u32::try_from(raw_body.len() - raw_body.trim_start().len()).unwrap_or(u32::MAX);
        let trail = u32::try_from(raw_body.len() - raw_body.trim_end().len()).unwrap_or(u32::MAX);
        let untrimmed_len = u32::try_from(raw_body.len()).unwrap_or(u32::MAX);

        let trimmed = raw_body.trim();
        let trimmed_len = u32::try_from(trimmed.len()).unwrap_or(u32::MAX);

        // Drop segments in trimmed-away regions; shift surviving out-values by -lead.
        if let Some(ref mut map) = ctx.map {
            crate::sourcemap::rebase_trim(
                &mut map.segments,
                seg_start,
                start_cursor,
                lead,
                untrimmed_len,
                trail,
            );
            // Advance cursor to the trimmed body end so the outer Interpolation
            // arm's final cursor correction (`saved_cursor + output.len()`) agrees.
            map.cursor = start_cursor.saturating_add(trimmed_len);
        }

        // Signal to the Interpolation arm: body owns the segments.
        ctx.fn_body_owned = true;

        Ok(trimmed.to_string())
    } else {
        // S3 path: suppress source-map recording inside function bodies.
        // The call site (the Interpolation node) is recorded as a single point
        // by the Interpolation arm when fn_body_owned stays false.
        if let Some(ref mut map) = ctx.map {
            map.suppress += 1;
        }
        ctx.call_stack.push(call_key.to_string());
        let result = evaluate_nodes(&func.body, scope, ctx).map(|s| s.trim().to_string());
        // Safety-critical LIFO invariant: call_stack tracks recursion detection.
        // A mismatched pop would silently corrupt recursion state and allow
        // stack overflows. Return a structured error rather than panicking so
        // callers get a proper diagnostic instead of an opaque abort.
        let popped = ctx.call_stack.pop();
        if let Some(ref mut map) = ctx.map {
            map.suppress = map.suppress.saturating_sub(1);
        }
        let lifo_result = if popped.as_deref() == Some(call_key) {
            Ok(())
        } else {
            Err(MdsError::syntax(format!(
                "internal error: call_stack LIFO violated: \
                 expected '{call_key}', got {popped:?}"
            )))
        };
        let pop_result = scope.pop();
        // B1: restore body_origin before returning so LIFO is correct even on error
        // paths (prefer_first_error returns the render error, not a panic).
        ctx.body_origin = prev_body_origin;
        // On double-fault, preserve the render error — it carries the actionable
        // source-span diagnostic for the user. LIFO/pop failures are compiler
        // bugs and surface as secondary errors.
        prefer_first_error(result, lifo_result.and(pop_result))
    }
}

fn call_function(
    name: &str,
    args: &[Value],
    scope: &mut Scope,
    ctx: &mut EvalContext,
) -> Result<Value, MdsError> {
    // User-defined functions take priority (shadowing built-ins).
    if let Some(func) = scope.get_function(name).cloned() {
        return invoke_function(&func, name, args, scope, ctx).map(Value::String);
    }
    // Fall back to built-ins.
    if let Some(meta) = crate::builtins::get_builtin(name) {
        // Defense-in-depth arity check: the validator enforces this at compile
        // time, but we guard here too so the evaluator is safe when called
        // without prior validation (e.g. in unit tests or future API consumers).
        if !check_arity(args.len(), meta.min_args, meta.max_args) {
            return Err(MdsError::arity(
                name,
                meta.min_args,
                meta.max_args,
                args.len(),
                None, // built-in signatures live in docs, not the error message
            ));
        }
        // Call the handler directly using the meta reference we already hold,
        // avoiding a second linear scan through BUILTINS that call_builtin
        // would perform internally.
        return (meta.handler)(args);
    }
    Err(MdsError::undefined_fn(name))
}

fn call_qualified_function(
    namespace: &str,
    name: &str,
    args: &[Value],
    scope: &mut Scope,
    ctx: &mut EvalContext,
) -> Result<Value, MdsError> {
    let qualified_name = format!("{namespace}.{name}");

    let ns = scope
        .get_namespace(namespace)
        .ok_or_else(|| MdsError::undefined_var(namespace))?;

    let func = ns
        .functions
        .get(name)
        .ok_or_else(|| MdsError::undefined_fn(&qualified_name))?
        .clone();

    invoke_function(&func, &qualified_name, args, scope, ctx).map(Value::String)
}

/// Compare two runtime `Value` instances for strict same-type equality.
///
/// Returns `Some(true)` / `Some(false)` for same-type comparisons, and `None`
/// for cross-type comparisons (caller must surface a `TypeMismatch` error).
/// `NaN == NaN` is `Some(false)` (IEEE 754 via Rust's `f64 ==`).
///
/// Arrays and objects use structural (deep) equality: `Value` derives `PartialEq`,
/// so this is a recursive element-wise comparison. Note that the condition grammar
/// currently cannot express container literals on the RHS, so same-type container
/// comparisons are only reachable at the unit-test level today — these arms guard
/// future grammar extensions and ensure the contract is correct: same-type always
/// compares structurally, only cross-type is an error.
fn values_equal_same_type(lhs: &Value, rhs: &Value) -> Option<bool> {
    match (lhs, rhs) {
        (Value::String(a), Value::String(b)) => Some(a == b),
        (Value::Number(a), Value::Number(b)) => Some(a == b),
        (Value::Boolean(a), Value::Boolean(b)) => Some(a == b),
        (Value::Null, Value::Null) => Some(true),
        (Value::Array(a), Value::Array(b)) => Some(a == b),
        (Value::Object(a), Value::Object(b)) => Some(a == b),
        // Cross-type: signal TypeMismatch to the caller.
        _ => None,
    }
}

/// Build a `TypeMismatch` error, with or without a source span.
///
/// Source selection (applies PF-012 — avoids cross-source offset mis-attribution):
/// - When inside an imported `@define` body (`ctx.body_origin` is `Some`), use
///   the body's defining file/source — the AST offsets index THAT source.
/// - Otherwise use `ctx.file`/`ctx.source` (the top-level module's context).
///
/// If `anchor` is `Some(offset)` and the offset falls within the selected source,
/// the error carries a span anchored to the entire enclosing `@if`/`@elseif`
/// directive line (from `offset` to the first `\n`).  If the offset is out of
/// bounds or the source is empty (e.g. a cross-source `@extends` splice where the
/// condition's AST offset is relative to the base template, not the current
/// module), the function degrades to a spanless error — never mis-attributes a
/// span from a different source (DECISIONS_CONTEXT ADR-005 / PF-012: degrade
/// rather than mis-attribute).
fn build_type_mismatch(
    ctx: &EvalContext<'_>,
    anchor: Option<usize>,
    lhs_type: &str,
    rhs_type: &str,
) -> MdsError {
    let off = match anchor {
        Some(off) => off,
        None => return MdsError::type_mismatch(lhs_type, rhs_type),
    };

    // Select (file, source) from body_origin when inside a function body,
    // else fall back to the top-level module context (applies PF-012).
    let (file, source) = if let Some(ref origin) = ctx.body_origin {
        (origin.file.as_ref(), origin.source.as_ref())
    } else {
        (ctx.file, ctx.source)
    };

    if !source.is_empty() && off <= source.len() && source.is_char_boundary(off) {
        // Span covers the full directive line (from `off` to just before `\n`).
        let line_len = source[off..].find('\n').unwrap_or(source[off..].len());
        return MdsError::type_mismatch_at(lhs_type, rhs_type, file, source, off, line_len);
    }
    MdsError::type_mismatch(lhs_type, rhs_type)
}

/// Evaluate a condition to a boolean, resolving expressions from scope.
///
/// `scope` is `&mut` because conditions can now contain arbitrary expressions
/// (e.g. function calls such as `func(a) == func(b)`), and `evaluate_expr`
/// requires mutable scope access for function dispatch. Short-circuit
/// evaluation (`&&`, `||`) limits which operands are evaluated — the
/// right-hand side of `&&` is skipped on a false left-hand side, and
/// vice-versa for `||`.
///
/// `anchor` is the byte offset of the enclosing `@if`/`@elseif` directive in
/// `ctx.source`.  It is passed through `And`/`Or` recursion unchanged so that
/// nested comparisons always report the enclosing directive line, not a nested
/// sub-expression location.  Pass `None` when no anchor is available (callers
/// that do not thread file/source context into `ctx`).
fn evaluate_condition(
    condition: &Condition,
    scope: &mut Scope,
    ctx: &mut EvalContext,
    anchor: Option<usize>,
) -> Result<bool, MdsError> {
    match condition {
        Condition::Truthy(expr) => Ok(evaluate_expr(expr, scope, ctx)?.is_truthy()),
        Condition::Not(expr) => Ok(!evaluate_expr(expr, scope, ctx)?.is_truthy()),
        Condition::Eq(lhs, rhs) => {
            let lhs_val = evaluate_expr(lhs, scope, ctx)?;
            let rhs_val = evaluate_expr(rhs, scope, ctx)?;
            values_equal_same_type(&lhs_val, &rhs_val).ok_or_else(|| {
                build_type_mismatch(ctx, anchor, lhs_val.type_name(), rhs_val.type_name())
            })
        }
        Condition::NotEq(lhs, rhs) => {
            let lhs_val = evaluate_expr(lhs, scope, ctx)?;
            let rhs_val = evaluate_expr(rhs, scope, ctx)?;
            values_equal_same_type(&lhs_val, &rhs_val)
                .map(|eq| !eq)
                .ok_or_else(|| {
                    build_type_mismatch(ctx, anchor, lhs_val.type_name(), rhs_val.type_name())
                })
        }
        // Short-circuit And: return false on first false operand.
        // Parser invariant: And operands are always leaf conditions (parse_and_level calls
        // parse_simple_condition for each part). The total operand count is bounded at
        // parse time by MAX_LOGICAL_OPERANDS (limits.rs), preventing adversarial trees.
        // The debug_assert below is a dev-only canary (elided in release builds): it fires
        // in tests if a future grammar change introduces And-in-And nesting without updating
        // this evaluator, surfacing the breakage before it reaches production.
        Condition::And(operands) => {
            for operand in operands {
                debug_assert!(
                    !matches!(operand, Condition::And(_) | Condition::Or(_)),
                    "And operand should be a leaf condition, not And/Or"
                );
                if !evaluate_condition(operand, scope, ctx, anchor)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        // Short-circuit Or: return true on first true operand.
        // Parser invariant: Or operands are always And-level results (And or leaf), never Or.
        // The total operand count is bounded at parse time by MAX_LOGICAL_OPERANDS (limits.rs).
        // The debug_assert is a dev-only canary that fires in tests if a future grammar
        // change introduces Or-in-Or nesting without updating this evaluator.
        Condition::Or(operands) => {
            for operand in operands {
                debug_assert!(
                    !matches!(operand, Condition::Or(_)),
                    "Or operand should not be Or (parser flattens same-level operators)"
                );
                if evaluate_condition(operand, scope, ctx, anchor)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
    }
}

fn evaluate_if(
    block: &IfBlock,
    scope: &mut Scope,
    ctx: &mut EvalContext,
) -> Result<String, MdsError> {
    // Evaluate the primary condition; anchor span on the @if directive line.
    if evaluate_condition(&block.condition, scope, ctx, Some(block.offset))? {
        return evaluate_nodes(&block.then_body, scope, ctx);
    }

    // Evaluate @elseif branches in order (short-circuit: first true branch wins).
    // Each branch's anchor is its own offset so type_mismatch points to the
    // @elseif line that triggered the error, not the @if line.
    // Parser enforces MAX_ELSEIF_BRANCHES at construction time; assert the invariant
    // holds so evaluator correctness cannot silently depend on the parser limit alone.
    debug_assert!(
        block.elseif_branches.len() <= MAX_ELSEIF_BRANCHES,
        "elseif_branches length {} exceeds MAX_ELSEIF_BRANCHES {}",
        block.elseif_branches.len(),
        MAX_ELSEIF_BRANCHES,
    );
    for branch in &block.elseif_branches {
        if evaluate_condition(&branch.condition, scope, ctx, Some(branch.offset))? {
            return evaluate_nodes(&branch.body, scope, ctx);
        }
    }

    // Fall through to @else body if present
    if let Some(else_body) = &block.else_body {
        evaluate_nodes(else_body, scope, ctx)
    } else {
        Ok(String::new())
    }
}

/// Generic `@for` driver shared by text mode and messages mode.
///
/// Owns: iterable dispatch (key-value vs array), per-loop and cumulative
/// iteration cap enforcement, and scope push/bind/pop for each iteration.
/// The caller supplies a closure that is called once per iteration with the
/// scope already bound; the closure accumulates results into whatever output
/// type each mode needs.
///
/// The closure signature is `FnMut(&mut Scope, &mut EvalContext) ->
/// Result<(), MdsError>`. Using `impl FnMut` (not `Box<dyn FnMut>`) keeps
/// both call sites monomorphized so there is no heap allocation or virtual
/// dispatch on the hot loop-body path.
fn drive_for<F>(
    block: &ForBlock,
    scope: &mut Scope,
    ctx: &mut EvalContext,
    mut body: F,
) -> Result<(), MdsError>
where
    F: FnMut(&mut Scope, &mut EvalContext) -> Result<(), MdsError>,
{
    let iterable = evaluate_expr(&block.iterable, scope, ctx)?;

    if let Some(ref key_var) = block.key_var {
        // Key-value iteration: @for key, value in obj:
        let map = match iterable {
            Value::Object(m) => m,
            _ => {
                return Err(MdsError::syntax(format!(
                    "key-value iteration requires an object, but got {}",
                    iterable.type_name()
                )));
            }
        };

        if map.len() > MAX_LOOP_ITERATIONS {
            return Err(MdsError::resource_limit(format!(
                "object has {} entries, exceeding maximum loop iteration limit of {}",
                map.len(),
                MAX_LOOP_ITERATIONS
            )));
        }

        // Sort keys alphabetically for deterministic output.
        let mut entries: Vec<(String, Value)> = map.into_iter().collect();
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));

        for (key, val) in entries {
            ctx.total_iterations += 1;
            if ctx.total_iterations > MAX_TOTAL_ITERATIONS {
                return Err(MdsError::resource_limit(format!(
                    "total loop iterations exceeded maximum of {} across all loops in this compilation",
                    MAX_TOTAL_ITERATIONS
                )));
            }
            scope.push();
            scope.set_var(key_var, Value::String(key));
            scope.set_var(&block.var, val);
            let result = body(scope, ctx);
            prefer_first_error(result, scope.pop())?;
        }
        return Ok(());
    }

    // Standard array iteration: @for item in iterable:
    // Objects are rejected here with a hint to use key-value syntax.
    if let Value::Object(_) = &iterable {
        return Err(MdsError::syntax(
            "to iterate over an object's entries, use `@for key, value in obj:` syntax",
        ));
    }

    let array = iterable
        .as_array()
        .ok_or_else(|| MdsError::type_error(iterable.type_name()))?;

    // Check length before cloning to avoid allocating the full Vec only to
    // immediately return a resource-limit error for oversized arrays.
    if array.len() > MAX_LOOP_ITERATIONS {
        return Err(MdsError::resource_limit(format!(
            "array has {} elements, exceeding maximum loop iteration limit of {}",
            array.len(),
            MAX_LOOP_ITERATIONS
        )));
    }

    // Clone items to release the borrow on scope before iterating.
    let items = array.to_vec();

    for item in items {
        ctx.total_iterations += 1;
        if ctx.total_iterations > MAX_TOTAL_ITERATIONS {
            return Err(MdsError::resource_limit(format!(
                "total loop iterations exceeded maximum of {} across all loops in this compilation",
                MAX_TOTAL_ITERATIONS
            )));
        }
        scope.push();
        scope.set_var(&block.var, item);
        let result = body(scope, ctx);
        prefer_first_error(result, scope.pop())?;
    }
    Ok(())
}

fn evaluate_for(
    block: &ForBlock,
    scope: &mut Scope,
    ctx: &mut EvalContext,
) -> Result<String, MdsError> {
    let mut output = String::new();
    drive_for(block, scope, ctx, |scope, ctx| {
        let rendered = evaluate_nodes(&block.body, scope, ctx)?;
        output.push_str(&rendered);
        Ok(())
    })?;
    Ok(output)
}

fn evaluate_include(
    inc: &IncludeDirective,
    scope: &Scope,
    ctx: &mut EvalContext,
) -> Result<String, MdsError> {
    let ns = scope
        .get_namespace(&inc.alias)
        .ok_or_else(|| MdsError::undefined_var(&inc.alias))?;

    let prompt_body = match &ns.prompt_body {
        Some(body) => body.clone(),
        None => {
            if ctx.warnings.len() < MAX_WARNINGS {
                ctx.warnings.push(format!(
                    "warning: @include of '{}' produced empty output — module has no body text",
                    inc.alias
                ));
            }
            return Ok(String::new());
        }
    };

    // S6: Splice source-map fragment segments when a builder is active.
    //
    // The local→global source-index remap is cached by Arc<FragmentMap> pointer
    // identity (AC-PERF-05): built once on the first `@include` call for a given
    // module and reused on all subsequent calls (e.g. inside a `@for` loop).
    //
    // The MapBuilder is temporarily taken out of ctx to allow concurrent mutable
    // access to ctx.fragment_remap_cache (avoiding Rust split-borrow issues).
    if let Some(fmap) = ns.prompt_map.as_ref() {
        if let Some(mut map) = ctx.map.take() {
            // Stable pointer identity of the Arc — used as the cache key.
            // The Arc lives as long as the NamespaceScope is in scope (at least
            // for the duration of this compilation), so the pointer is valid.
            let fmap_ptr = std::sync::Arc::as_ptr(fmap) as usize;

            // Build the remap if not already cached for this include site,
            // then borrow it directly from the entry return value.
            let remap: &Vec<u32> = ctx.fragment_remap_cache.entry(fmap_ptr).or_insert_with(|| {
                fmap.sources
                    .iter()
                    .map(|(path, content)| map.source_index(path.as_ref(), content.as_ref()))
                    .collect()
            });

            // Splice: rebase each fragment segment into the global output stream.
            // `map.cursor` is the absolute byte offset where this include's body
            // starts in the parent output (set by the Node::Include arm in
            // evaluate_nodes just before calling evaluate_include).
            let base = map.cursor;
            for seg in &fmap.segments {
                let remapped_src = remap[seg.src as usize];
                map.push_fragment_segment(base + seg.out, remapped_src, seg.src_off, seg.len);
            }

            ctx.map = Some(map);
        }
    }

    Ok(prompt_body)
}

/// A single structured message produced by `evaluate_messages_intrinsic`.
#[derive(Debug, Clone, PartialEq)]
pub struct EvalMessage {
    pub role: String,
    pub content: String,
}

/// Evaluate a module body in messages mode, collecting `@message` blocks as
/// structured `EvalMessage` values.
///
/// Output shape is intrinsic to the template: a template that contains any
/// `@message` block compiles to structured messages, so non-whitespace `Text`
/// or `Interpolation` nodes that appear *outside* a `@message` block are a hard
/// error ([`MdsError::MixedContent`]) rather than a silently-ignored warning.
/// Empty messages (after trimming) are silently skipped.
/// Returns an error when `MAX_MESSAGE_COUNT` would be exceeded.
///
/// `file`/`source` provide the diagnostic context for the [`MdsError::MixedContent`]
/// span: when orphan content is found, the offending node's byte offset (already
/// captured by the parser on every `TextNode`/`Interpolation`) is paired with
/// `source` so the error underlines the prose (ADR-022). For the `@extends` path the
/// offsets may originate in a base template rather than `source`; the shared `at()`
/// guard drops the source in that out-of-bounds case so no miette `OutOfBounds`
/// render can occur (the raw offset/length are still preserved in `serialize()`).
pub fn evaluate_messages_intrinsic(
    nodes: &[Node],
    scope: &mut Scope,
    warnings: &mut Vec<String>,
    file: &str,
    source: &str,
) -> Result<Vec<EvalMessage>, MdsError> {
    let mut ctx = EvalContext {
        call_stack: Vec::new(),
        total_iterations: 0,
        total_message_bytes: 0,
        warnings,
        map: None,
        fragment_remap_cache: std::collections::HashMap::new(),
        fn_body_owned: false,
        file,
        source,
        body_origin: None,
    };
    let mut messages = Vec::new();
    collect_messages_strict(nodes, scope, &mut ctx, &mut messages, file, source)?;
    Ok(messages)
}

/// Recursive collector for messages mode. Descends into control-flow nodes
/// (`@if`, `@for`) while collecting `@message` blocks into `out`.
///
/// Strict: non-whitespace `Text` or any `Interpolation` outside a `@message`
/// block returns [`MdsError::MixedContent`] — mixed content is not silently
/// dropped because the template's output shape (messages) is intrinsic.
fn collect_messages_strict(
    nodes: &[Node],
    scope: &mut Scope,
    ctx: &mut EvalContext,
    out: &mut Vec<EvalMessage>,
    file: &str,
    source: &str,
) -> Result<(), MdsError> {
    for node in nodes {
        match node {
            Node::Text(t) => {
                // Non-whitespace text outside any @message block is mixed content.
                // Whitespace-only text (blank lines between @message blocks) is fine.
                if !t.text.trim().is_empty() {
                    // Point the span at the first non-whitespace byte of the orphan
                    // text rather than at the node's leading blank lines, and span
                    // only the trimmed run so the underline hugs the actual prose.
                    let lead_ws = t.text.len() - t.text.trim_start().len();
                    let offset = t.offset.saturating_add(lead_ws);
                    let len = t.text.trim().len();
                    return Err(MdsError::mixed_content_at(file, source, offset, len));
                }
            }
            Node::EscapedBrace { .. } => {
                // Orphan escaped brace — treated as orphan whitespace; silently ignored
                // (mirrors the historical messages-mode treatment of escaped braces).
            }
            Node::Interpolation(interp) => {
                // Interpolation outside a @message block is mixed content. The
                // interpolation carries its own offset+len (the `{...}` span).
                return Err(MdsError::mixed_content_at(
                    file,
                    source,
                    interp.offset,
                    interp.len,
                ));
            }
            Node::Message(block) => {
                collect_single_message(block, scope, ctx, out)?;
            }
            Node::If(block) => {
                collect_messages_from_if(block, scope, ctx, out, file, source)?;
            }
            Node::For(block) => {
                collect_messages_from_for(block, scope, ctx, out, file, source)?;
            }
            Node::Define(_) | Node::Import(_) | Node::Export(_) => {
                // Already handled by the resolver before evaluation.
            }
            Node::Include(inc) => {
                // @include in messages mode is not meaningful — warn.
                if ctx.warnings.len() < MAX_WARNINGS {
                    ctx.warnings.push(format!(
                        "warning: @include '{}' inside messages mode is ignored",
                        inc.alias
                    ));
                }
            }
            Node::Block(block) => {
                // Descend into @block body to surface any @message blocks it contains.
                // In standalone mode the block body is the default content; in inheritance
                // mode the resolver has already spliced the final body before evaluation,
                // so this arm is reached for both cases.
                collect_messages_strict(&block.body, scope, ctx, out, file, source)?;
            }
        }
    }
    Ok(())
}

fn collect_single_message(
    block: &MessageBlock,
    scope: &mut Scope,
    ctx: &mut EvalContext,
    out: &mut Vec<EvalMessage>,
) -> Result<(), MdsError> {
    // Evaluate the role expression to a string.
    let role_value = evaluate_expr(&block.role, scope, ctx)?;
    let role = match role_value {
        Value::String(s) => s,
        other => {
            return Err(MdsError::type_error(format!(
                "@message role must evaluate to a string, but got {}",
                other.type_name()
            )));
        }
    };
    // Mirror the parse-time empty-role rejection on the dynamic path: a variable
    // or expression that evaluates to "" or whitespace-only must also be rejected.
    if role.trim().is_empty() {
        return Err(MdsError::type_error(
            "@message role must evaluate to a non-empty string",
        ));
    }

    // Evaluate the body as plain text and trim surrounding whitespace.
    // NOTE: this path uses only `.trim()`, NOT `clean_output` — `\r` characters
    // and blank-line runs inside a @message body survive verbatim into the
    // compiled JSON output. `mds fmt` relies on this: `raw_content_spans` in
    // `formatter.rs` marks @message and @define bodies as raw content to prevent
    // R1 (`\r` strip) and R3 (blank-line-run cap) from silently changing compiled
    // output. Adding a `clean_output` call here would break the formatter's
    // safety model without any corresponding formatter change.
    let content = evaluate_nodes(&block.body, scope, ctx)?.trim().to_string();

    // Skip empty messages.
    if content.is_empty() {
        return Ok(());
    }

    if out.len() >= MAX_MESSAGE_COUNT {
        return Err(MdsError::resource_limit(format!(
            "message count exceeded maximum of {}",
            MAX_MESSAGE_COUNT
        )));
    }

    // Incremental cumulative-size check (AC-6.3): cap the aggregate content bytes
    // across all messages at MAX_MESSAGES_TOTAL_SIZE (50 MB) — the same ceiling a
    // single text-mode output has.  Checked before pushing so a runaway is caught early.
    ctx.total_message_bytes = ctx.total_message_bytes.saturating_add(content.len());
    if ctx.total_message_bytes > MAX_MESSAGES_TOTAL_SIZE {
        return Err(MdsError::resource_limit(format!(
            "total message content exceeds maximum cumulative size of {} bytes",
            MAX_MESSAGES_TOTAL_SIZE
        )));
    }

    out.push(EvalMessage { role, content });
    Ok(())
}

fn collect_messages_from_if(
    block: &crate::ast::IfBlock,
    scope: &mut Scope,
    ctx: &mut EvalContext,
    out: &mut Vec<EvalMessage>,
    file: &str,
    source: &str,
) -> Result<(), MdsError> {
    if evaluate_condition(&block.condition, scope, ctx, Some(block.offset))? {
        return collect_messages_strict(&block.then_body, scope, ctx, out, file, source);
    }
    // Parser enforces MAX_ELSEIF_BRANCHES at construction time; assert the invariant
    // holds so messages-mode correctness cannot silently depend on the parser limit alone.
    // Mirrors the identical assert in text-mode `evaluate_if`.
    debug_assert!(
        block.elseif_branches.len() <= MAX_ELSEIF_BRANCHES,
        "elseif_branches length {} exceeds MAX_ELSEIF_BRANCHES {}",
        block.elseif_branches.len(),
        MAX_ELSEIF_BRANCHES,
    );
    for branch in &block.elseif_branches {
        if evaluate_condition(&branch.condition, scope, ctx, Some(branch.offset))? {
            return collect_messages_strict(&branch.body, scope, ctx, out, file, source);
        }
    }
    if let Some(else_body) = &block.else_body {
        return collect_messages_strict(else_body, scope, ctx, out, file, source);
    }
    Ok(())
}

fn collect_messages_from_for(
    block: &ForBlock,
    scope: &mut Scope,
    ctx: &mut EvalContext,
    out: &mut Vec<EvalMessage>,
    file: &str,
    source: &str,
) -> Result<(), MdsError> {
    drive_for(block, scope, ctx, |scope, ctx| {
        collect_messages_strict(&block.body, scope, ctx, out, file, source)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Condition, DefineBlock, Interpolation, Param, TextNode};
    use std::sync::Arc;

    fn text(s: &str) -> Node {
        Node::Text(TextNode {
            text: s.to_string(),
            offset: 0,
        })
    }

    #[test]
    fn evaluate_text() {
        let nodes = vec![text("Hello world!")];
        let mut scope = Scope::new();
        let mut warnings = vec![];
        assert_eq!(
            evaluate(&nodes, &mut scope, &mut warnings, "", "").unwrap(),
            "Hello world!"
        );
    }

    #[test]
    fn evaluate_variable_interpolation() {
        let nodes = vec![
            text("Hello "),
            Node::Interpolation(Interpolation {
                expr: Expr::Var("name".to_string()),
                offset: 6,
                len: 4,
            }),
            text("!"),
        ];
        let mut scope = Scope::new();
        let mut warnings = vec![];
        scope.set_var("name", Value::String("Alice".to_string()));
        assert_eq!(
            evaluate(&nodes, &mut scope, &mut warnings, "", "").unwrap(),
            "Hello Alice!"
        );
    }

    #[test]
    fn evaluate_undefined_var_error() {
        let nodes = vec![Node::Interpolation(Interpolation {
            expr: Expr::Var("unknown".to_string()),
            offset: 0,
            len: 7,
        })];
        let mut scope = Scope::new();
        let mut warnings = vec![];
        let err = evaluate(&nodes, &mut scope, &mut warnings, "", "").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("unknown"),
            "error message should reference the undefined variable name; got: {msg}"
        );
    }

    #[test]
    fn evaluate_if_truthy() {
        let nodes = vec![Node::If(IfBlock {
            condition: Condition::Truthy(Expr::Var("flag".to_string())),
            elseif_branches: vec![],
            then_body: vec![text("yes")],
            else_body: Some(vec![text("no")]),
            offset: 0,
            else_offset: None,
            end_offset: 0,
        })];
        let mut scope = Scope::new();
        let mut warnings = vec![];
        scope.set_var("flag", Value::Boolean(true));
        assert_eq!(
            evaluate(&nodes, &mut scope, &mut warnings, "", "").unwrap(),
            "yes"
        );
    }

    #[test]
    fn evaluate_if_falsy() {
        let nodes = vec![Node::If(IfBlock {
            condition: Condition::Truthy(Expr::Var("flag".to_string())),
            elseif_branches: vec![],
            then_body: vec![text("yes")],
            else_body: Some(vec![text("no")]),
            offset: 0,
            else_offset: None,
            end_offset: 0,
        })];
        let mut scope = Scope::new();
        let mut warnings = vec![];
        scope.set_var("flag", Value::Boolean(false));
        assert_eq!(
            evaluate(&nodes, &mut scope, &mut warnings, "", "").unwrap(),
            "no"
        );
    }

    #[test]
    fn evaluate_for_loop() {
        let nodes = vec![Node::For(ForBlock {
            var: "item".to_string(),
            key_var: None,
            iterable: Expr::Var("items".to_string()),
            body: vec![
                text("- "),
                Node::Interpolation(Interpolation {
                    expr: Expr::Var("item".to_string()),
                    offset: 2,
                    len: 4,
                }),
                text("\n"),
            ],
            offset: 0,
            end_offset: 0,
        })];
        let mut scope = Scope::new();
        let mut warnings = vec![];
        scope.set_var(
            "items",
            Value::Array(vec![
                Value::String("apple".into()),
                Value::String("banana".into()),
            ]),
        );
        assert_eq!(
            evaluate(&nodes, &mut scope, &mut warnings, "", "").unwrap(),
            "- apple\n- banana\n"
        );
    }

    #[test]
    fn evaluate_function_call() {
        // The resolver handles @define and populates scope before evaluate() is called.
        // Simulate that here by pre-registering the function in scope.
        let define = DefineBlock {
            name: "greet".to_string(),
            params: vec![Param::required("name")],
            body: vec![
                text("Hello "),
                Node::Interpolation(Interpolation {
                    expr: Expr::Var("name".to_string()),
                    offset: 6,
                    len: 4,
                }),
                text("!"),
            ],
            offset: 0,
            end_offset: 0,
        };
        let mut scope = Scope::new();
        scope.set_function("greet", Arc::new(FunctionDef::from(&define)));

        let nodes = vec![Node::Interpolation(Interpolation {
            expr: Expr::Call {
                name: "greet".to_string(),
                args: vec![Arg::StringLiteral("Bob".to_string())],
            },
            offset: 20,
            len: 12,
        })];
        let mut warnings = vec![];
        assert_eq!(
            evaluate(&nodes, &mut scope, &mut warnings, "", "").unwrap(),
            "Hello Bob!"
        );
    }

    #[test]
    fn evaluate_escaped_brace() {
        let nodes = vec![
            text("Use "),
            Node::EscapedBrace { offset: 4 },
            text("name}} for interpolation"),
        ];
        let mut scope = Scope::new();
        let mut warnings = vec![];
        assert_eq!(
            evaluate(&nodes, &mut scope, &mut warnings, "", "").unwrap(),
            "Use {{name}} for interpolation"
        );
    }

    // ── prefer_first_error: double-fault error-preservation behaviour ─────────

    fn make_err(msg: &str) -> MdsError {
        MdsError::syntax(msg)
    }

    #[test]
    fn prefer_first_error_both_ok_returns_value() {
        let result: Result<&str, MdsError> = prefer_first_error(Ok("hello"), Ok(()));
        assert_eq!(result.unwrap(), "hello");
    }

    #[test]
    fn prefer_first_error_first_err_wins_over_ok_second() {
        // When the render (first) fails and secondary is Ok, the render error surfaces.
        let result: Result<&str, MdsError> =
            prefer_first_error(Err(make_err("render error")), Ok(()));
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("render error"),
            "first error should be returned; got: {msg}"
        );
    }

    #[test]
    fn prefer_first_error_first_err_wins_over_second_err() {
        // Double-fault: both render and secondary fail. Render error takes precedence
        // because it carries the actionable source-span diagnostic for the user.
        let result: Result<&str, MdsError> =
            prefer_first_error(Err(make_err("render error")), Err(make_err("lifo error")));
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("render error"),
            "render (first) error should win over lifo error in double-fault; got: {msg}"
        );
    }

    #[test]
    fn prefer_first_error_ok_first_surfaces_second_err() {
        // When render succeeds but the secondary (LIFO/pop) fails, the secondary error surfaces.
        let result: Result<&str, MdsError> =
            prefer_first_error(Ok("value"), Err(make_err("lifo error")));
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("lifo error"),
            "secondary error should be returned when first is Ok; got: {msg}"
        );
    }

    // ── Resource limit: MAX_CALL_DEPTH ───────────────────────────────────────

    #[test]
    fn call_depth_limit_rejects_deep_call_stack() {
        // Build a call chain of MAX_CALL_DEPTH + 2 functions (f0 calls f1, f1 calls f2, ...).
        // Uses direct AST construction to avoid the resolver's O(n^3) closure-capture overhead.
        //
        // Each FunctionDef has a simple body: [Interpolation(Call { "f{i+1}", [] })].
        // The last function in the chain calls f0 to close the cycle (preventing
        // the direct-recursion short-circuit which only catches same-key re-entry).
        let n = MAX_CALL_DEPTH + 2;
        let mut scope = Scope::new();

        // Register n functions: fi calls f(i+1 % n)
        for i in 0..n {
            let next = (i + 1) % n;
            let next_name = format!("f{next}");
            let body = vec![Node::Interpolation(Interpolation {
                expr: Expr::Call {
                    name: next_name,
                    args: vec![],
                },
                offset: 0,
                len: 3,
            })];
            let func = FunctionDef {
                params: vec![],
                body,
                captured: crate::scope::CapturedScope::default(),
                origin: None,
            };
            scope.set_function(&format!("f{i}"), Arc::new(func));
        }

        // Invoke f0 — should fail with a call depth error
        let call_node = vec![Node::Interpolation(Interpolation {
            expr: Expr::Call {
                name: "f0".to_string(),
                args: vec![],
            },
            offset: 0,
            len: 4,
        })];
        let mut warnings = vec![];
        let result = evaluate(&call_node, &mut scope, &mut warnings, "", "");
        assert!(result.is_err(), "call chain of {n} must be rejected");
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("call depth"),
            "error should mention call depth (got: {err})"
        );
    }

    // ── values_equal_same_type: NaN semantics ───────────────────────────────

    #[test]
    fn values_equal_nan_is_not_equal_to_itself() {
        // IEEE 754 defines NaN != NaN. values_equal_same_type must follow this — even
        // though the parser rejects NaN literals in condition expressions, the runtime
        // Value type holds f64 and could theoretically carry a NaN produced by arithmetic.
        // Verify that values_equal_same_type returns Some(false) for NaN == NaN.
        let nan_value = Value::Number(f64::NAN);
        let nan_value2 = Value::Number(f64::NAN);
        assert_eq!(
            values_equal_same_type(&nan_value, &nan_value2),
            Some(false),
            "NaN must not equal NaN (IEEE 754)"
        );
    }

    #[test]
    fn values_equal_same_type_returns_none_for_cross_type() {
        // Cross-type comparisons must return None, not a bool.
        let s = Value::String("3".to_string());
        let n = Value::Number(3.0);
        let b = Value::Boolean(true);
        let null = Value::Null;

        assert_eq!(values_equal_same_type(&s, &n), None, "string vs number");
        assert_eq!(values_equal_same_type(&n, &b), None, "number vs boolean");
        assert_eq!(values_equal_same_type(&b, &null), None, "boolean vs null");
        assert_eq!(values_equal_same_type(&s, &null), None, "string vs null");
    }

    // ── values_equal_same_type: container structural equality ────────────────

    #[test]
    fn values_equal_same_type_arrays_equal() {
        // [1, 2] == [1, 2] → Some(true)
        let a = Value::Array(vec![Value::Number(1.0), Value::Number(2.0)]);
        let b = Value::Array(vec![Value::Number(1.0), Value::Number(2.0)]);
        assert_eq!(
            values_equal_same_type(&a, &b),
            Some(true),
            "[1,2] == [1,2] must be Some(true)"
        );
    }

    #[test]
    fn values_equal_same_type_arrays_order_matters() {
        // [1, 2] != [2, 1] → Some(false)
        let a = Value::Array(vec![Value::Number(1.0), Value::Number(2.0)]);
        let b = Value::Array(vec![Value::Number(2.0), Value::Number(1.0)]);
        assert_eq!(
            values_equal_same_type(&a, &b),
            Some(false),
            "[1,2] vs [2,1] must be Some(false) — order is significant"
        );
    }

    #[test]
    fn values_equal_same_type_objects_equal() {
        // {a: 1} == {a: 1} → Some(true)
        let mut m1 = std::collections::HashMap::new();
        m1.insert("a".to_string(), Value::Number(1.0));
        let mut m2 = std::collections::HashMap::new();
        m2.insert("a".to_string(), Value::Number(1.0));
        assert_eq!(
            values_equal_same_type(&Value::Object(m1), &Value::Object(m2)),
            Some(true),
            "{{a:1}} == {{a:1}} must be Some(true)"
        );
    }

    #[test]
    fn values_equal_same_type_objects_unequal() {
        // {a: 1} != {a: 2} → Some(false)
        let mut m1 = std::collections::HashMap::new();
        m1.insert("a".to_string(), Value::Number(1.0));
        let mut m2 = std::collections::HashMap::new();
        m2.insert("a".to_string(), Value::Number(2.0));
        assert_eq!(
            values_equal_same_type(&Value::Object(m1), &Value::Object(m2)),
            Some(false),
            "{{a:1}} vs {{a:2}} must be Some(false)"
        );
    }

    #[test]
    fn values_equal_same_type_nested_array_equal() {
        // [[1], [2]] == [[1], [2]] → Some(true)
        let a = Value::Array(vec![
            Value::Array(vec![Value::Number(1.0)]),
            Value::Array(vec![Value::Number(2.0)]),
        ]);
        let b = Value::Array(vec![
            Value::Array(vec![Value::Number(1.0)]),
            Value::Array(vec![Value::Number(2.0)]),
        ]);
        assert_eq!(
            values_equal_same_type(&a, &b),
            Some(true),
            "nested [[1],[2]] == [[1],[2]] must be Some(true)"
        );
    }

    #[test]
    fn values_equal_same_type_empty_arrays_equal() {
        // [] == [] → Some(true)
        let a = Value::Array(vec![]);
        let b = Value::Array(vec![]);
        assert_eq!(
            values_equal_same_type(&a, &b),
            Some(true),
            "[] == [] must be Some(true)"
        );
    }

    #[test]
    fn values_equal_same_type_array_nan_element() {
        // [NaN] == [NaN] → Some(false) because NaN != NaN per IEEE 754
        let a = Value::Array(vec![Value::Number(f64::NAN)]);
        let b = Value::Array(vec![Value::Number(f64::NAN)]);
        assert_eq!(
            values_equal_same_type(&a, &b),
            Some(false),
            "[NaN] == [NaN] must be Some(false) — NaN ≠ NaN propagates through arrays"
        );
    }

    #[test]
    fn values_equal_same_type_array_element_type_mismatch_returns_false() {
        // [1] vs ["1"] is a same-type (both Array) comparison but with a differing
        // element type — must return Some(false) (not None / not an error).
        let a = Value::Array(vec![Value::Number(1.0)]);
        let b = Value::Array(vec![Value::String("1".to_string())]);
        assert_eq!(
            values_equal_same_type(&a, &b),
            Some(false),
            "[1] vs [\"1\"] must be Some(false) — element type differs but outer type matches"
        );
    }

    // ── Resource limit: MAX_OUTPUT_SIZE ──────────────────────────────────────

    #[test]
    fn output_size_limit_rejects_oversized_output() {
        // Build a node list that accumulates past MAX_OUTPUT_SIZE (50 MB) across two
        // nodes, rather than allocating a single 50 MB+ string in one shot.
        // Each node is ~26 MB; after the second node the accumulated output exceeds
        // the limit and evaluate_nodes returns a ResourceLimit error.
        //
        // Using two nodes of ~26 MB each keeps peak allocation at ~52 MB total
        // (26 MB node + 26 MB node + accumulating output buffer), which is safer
        // for CI environments than a single 50 MB+ pre-allocated string.
        let half = MAX_OUTPUT_SIZE / 2 + 1;
        let chunk = "x".repeat(half);
        let nodes = vec![text(&chunk), text(&chunk)];
        let mut scope = Scope::new();
        let mut warnings = vec![];
        let result = evaluate(&nodes, &mut scope, &mut warnings, "", "");
        assert!(
            result.is_err(),
            "output exceeding MAX_OUTPUT_SIZE must be rejected"
        );
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("output") || err.contains("maximum size") || err.contains("50"),
            "error should mention output size limit, got: {err}"
        );
    }

    // ── Default parameter filling ─────────────────────────────────────────────

    #[test]
    fn evaluate_default_param_used_when_not_provided() {
        let result = crate::compile_str_md(
            "@define greet(name = \"World\"):\nHello {{name}}!\n@end\n{{greet()}}\n",
        )
        .unwrap();
        assert_eq!(result, "Hello World!\n");
    }

    #[test]
    fn evaluate_default_param_overridden_when_provided() {
        let result = crate::compile_str_md(
            "@define greet(name = \"World\"):\nHello {{name}}!\n@end\n{{greet(\"Alice\")}}\n",
        )
        .unwrap();
        assert_eq!(result, "Hello Alice!\n");
    }

    #[test]
    fn evaluate_all_defaults_provided() {
        let result = crate::compile_str_md(
            "@define add(a = 1, b = 2):\n{{a}} {{b}}\n@end\n{{add(10, 20)}}\n",
        )
        .unwrap();
        assert_eq!(result, "10 20\n");
    }

    #[test]
    fn evaluate_mixed_required_and_default() {
        let result = crate::compile_str_md(
            "@define greet(name, greeting = \"Hello\"):\n{{greeting}} {{name}}!\n@end\n{{greet(\"Bob\")}}\n",
        )
        .unwrap();
        assert_eq!(result, "Hello Bob!\n");
    }

    #[test]
    fn evaluate_default_param_number() {
        // literal_expr_to_value: Expr::NumberLiteral → Value::Number
        let result =
            crate::compile_str_md("@define show(x = 42):\n{{x}}\n@end\n{{show()}}\n").unwrap();
        assert!(
            result.contains("42"),
            "Number default should produce its numeric value, got: {result}"
        );
    }

    #[test]
    fn evaluate_default_param_boolean_true() {
        // literal_expr_to_value: Expr::BooleanLiteral(true) → Value::Boolean(true)
        let result = crate::compile_str_md(
            "@define show(flag = true):\n@if flag:\nyes\n@else:\nno\n@end\n@end\n{{show()}}\n",
        )
        .unwrap();
        assert!(
            result.contains("yes"),
            "Boolean true default should be truthy, got: {result}"
        );
    }

    #[test]
    fn evaluate_default_param_boolean_false() {
        // literal_expr_to_value: Expr::BooleanLiteral(false) → Value::Boolean(false)
        let result = crate::compile_str_md(
            "@define show(flag = false):\n@if flag:\nyes\n@else:\nno\n@end\n@end\n{{show()}}\n",
        )
        .unwrap();
        assert!(
            result.contains("no"),
            "Boolean false default should be falsy, got: {result}"
        );
    }

    #[test]
    fn evaluate_default_param_null() {
        // literal_expr_to_value: Expr::NullLiteral → Value::Null (falsy)
        let result = crate::compile_str_md(
            "@define show(x = null):\n@if x:\nset\n@else:\nnull_branch\n@end\n@end\n{{show()}}\n",
        )
        .unwrap();
        assert!(
            result.contains("null_branch"),
            "Null default should be falsy, got: {result}"
        );
    }

    #[test]
    fn evaluate_arity_error_on_too_few_required_args() {
        let result = crate::compile_str_md("@define greet(name):\n{{name}}\n@end\n{{greet()}}\n");
        assert!(result.is_err(), "too few required args should fail");
    }

    // ── @define body edge-trim contract (spec §4.11) ──────────────────────────
    //
    // invoke_function trims the body output at its EDGES (leading/trailing whitespace)
    // while preserving interior blank runs verbatim. This matches the @message body
    // contract (symmetric per spec §4.11). Locked here so a refactor of invoke_function
    // cannot silently change the trimming semantics.

    #[test]
    fn define_edge_trim_removes_leading_and_trailing_blank_lines() {
        // Body has a leading blank line and a trailing blank line around the content.
        // After edge-trim the blank lines are removed; only the content (and its
        // trailing newline from the template output) survives.
        let src = "@define body():\n\nHello.\n\n@end\n{{body()}}\n";
        let result = crate::compile_str_md(src).unwrap();
        assert_eq!(
            result, "Hello.\n",
            "edge-trim must strip leading and trailing blank lines from @define body"
        );
    }

    #[test]
    fn define_edge_trim_preserves_interior_blank_run() {
        // Body has leading blank lines, an interior blank run, and trailing blank lines.
        // Edge-trim removes the outer blank lines; the interior blank run is preserved.
        let src = "@define body():\n\nLine one.\n\nLine two.\n\n@end\n{{body()}}\n";
        let result = crate::compile_str_md(src).unwrap();
        assert_eq!(
            result, "Line one.\n\nLine two.\n",
            "edge-trim must preserve interior blank run while removing leading/trailing blank lines"
        );
    }

    #[test]
    fn define_edge_trim_leading_indentation_stripped() {
        // A leading newline before indented content: edge-trim removes the leading
        // whitespace (newline) while preserving the body content.
        let src = "@define body():\n\nContent here.\n@end\n{{body()}}\n";
        let result = crate::compile_str_md(src).unwrap();
        assert_eq!(
            result, "Content here.\n",
            "edge-trim must strip leading blank line even when body has no trailing blank line"
        );
    }

    // ── Built-in functions integration ────────────────────────────────────────

    #[test]
    fn builtin_upper_in_interpolation() {
        let result = crate::compile_str_md("---\nword: hello\n---\n{{upper(word)}}\n").unwrap();
        assert!(
            result.contains("HELLO"),
            "upper() should uppercase, got: {result}"
        );
    }

    #[test]
    fn builtin_lower_in_interpolation() {
        let result = crate::compile_str_md("---\nword: HELLO\n---\n{{lower(word)}}\n").unwrap();
        assert!(
            result.contains("hello"),
            "lower() should lowercase, got: {result}"
        );
    }

    #[test]
    fn builtin_length_string_in_interpolation() {
        let result = crate::compile_str_md("---\nword: hello\n---\n{{length(word)}}\n").unwrap();
        assert!(
            result.contains('5'),
            "length of 'hello' should be 5, got: {result}"
        );
    }

    #[test]
    fn builtin_shadowed_by_user_function() {
        // A user-defined function named 'upper' should shadow the built-in.
        let src = "@define upper(x):\ncustom\n@end\n{{upper(\"anything\")}}\n";
        let result = crate::compile_str_md(src).unwrap();
        assert_eq!(
            result.trim(),
            "custom",
            "user-defined upper() should shadow built-in"
        );
    }

    #[test]
    fn builtin_compose_join_split() {
        let result =
            crate::compile_str_md("---\ncsv: a,b,c\n---\n{{join(split(csv, \",\"), \" | \")}}\n")
                .unwrap();
        assert!(
            result.contains("a | b | c"),
            "join(split()) should work, got: {result}"
        );
    }

    #[test]
    fn builtin_string_with_number_literal_arg() {
        let result = crate::compile_str_md("{{string(42)}}\n").unwrap();
        assert_eq!(result.trim(), "42");
    }

    // ── Phase 1: standalone @block rendering tests ────────────────────────────

    #[test]
    fn block_standalone_renders_default_inline_text_mode() {
        // A standalone @block renders its default body inline in text mode.
        let src = "Before.\n@block content:\nDefault body.\n@end\nAfter.\n";
        let result = crate::compile_str_md(src).unwrap();
        assert!(
            result.contains("Default body."),
            "standalone @block default should render inline in text mode, got: {result}"
        );
        assert!(
            result.contains("Before."),
            "text before @block should appear: {result}"
        );
        assert!(
            result.contains("After."),
            "text after @block should appear: {result}"
        );
    }

    #[test]
    fn block_standalone_renders_empty_body_in_text_mode() {
        // An empty @block renders nothing (no crash, no marker text).
        let src = "A\n@block empty:\n@end\nB\n";
        let result = crate::compile_str_md(src).unwrap();
        // Should produce "A\nB\n" — empty block body contributes nothing.
        assert!(
            !result.contains("@block") && !result.contains("@end"),
            "block markers must not appear in output: {result}"
        );
        assert!(
            result.contains('A') && result.contains('B'),
            "surrounding text should remain: {result}"
        );
    }

    #[test]
    fn block_with_interpolation_renders_inline() {
        // A @block body that contains interpolation resolves variables from scope.
        let src = "---\nname: Alice\n---\n@block greeting:\nHello {{name}}!\n@end\n";
        let result = crate::compile_str_md(src).unwrap();
        assert!(
            result.contains("Hello Alice!"),
            "block body interpolation should resolve, got: {result}"
        );
    }

    #[test]
    fn block_messages_mode_descends_into_block_body() {
        // In messages mode, @message inside a @block body should surface as a message.
        let src = "@block wrapper:\n@message system:\nSystem prompt.\n@end\n@end\n";
        let result = crate::compile_str(src);
        assert!(
            result.is_ok(),
            "compile should succeed for @block containing @message: {result:?}"
        );
        let messages = result.unwrap().into_messages().unwrap();
        assert_eq!(
            messages.len(),
            1,
            "@message inside @block should surface in messages mode"
        );
        assert_eq!(messages[0].role, "system");
        assert!(messages[0].content.contains("System prompt."));
    }

    // ── Arity span-divergence tests (#71) ─────────────────────────────────────
    //
    // Pin the deliberate divergence: evaluator errors have NO span (no source
    // context available at runtime); validator errors HAVE a span (source text
    // is present at compile time).

    #[test]
    fn arity_error_evaluator_has_no_span() {
        // Bypass the validator so only the evaluator's arity check fires.
        // compile_str runs validator then evaluator; we call evaluate directly
        // to isolate the evaluator path.
        use crate::ast::{Interpolation, Param};
        use crate::scope::FunctionDef;

        let define = crate::ast::DefineBlock {
            name: "requires_one".to_string(),
            params: vec![Param::required("x")],
            body: vec![text("ok")],
            offset: 0,
            end_offset: 0,
        };
        let mut scope = Scope::new();
        scope.set_function("requires_one", Arc::new(FunctionDef::from(&define)));

        // Call with 0 args → arity error from invoke_function (evaluator path).
        let nodes = vec![Node::Interpolation(Interpolation {
            expr: Expr::Call {
                name: "requires_one".to_string(),
                args: vec![],
            },
            offset: 0,
            len: 12,
        })];
        let mut warnings = vec![];
        let err = evaluate(&nodes, &mut scope, &mut warnings, "", "")
            .expect_err("should fail with arity mismatch");

        // Assert the span is None on the evaluator path (no source text available).
        match err {
            crate::error::MdsError::ArityMismatch { span, .. } => {
                assert!(
                    span.is_none(),
                    "evaluator arity error must carry no span, got {span:?}"
                );
            }
            other => panic!("expected ArityMismatch, got {other:?}"),
        }
    }

    #[test]
    fn arity_error_evaluator_builtin_has_no_span() {
        // Evaluator builtin arity check (defense-in-depth path): call_function
        // for a builtin with wrong arg count also uses MdsError::arity (no span).
        // We call evaluate directly with a pre-built AST node to bypass the validator.
        use crate::ast::{Arg, Interpolation};

        // upper() requires exactly 1 arg; call it with 2.
        let nodes = vec![Node::Interpolation(Interpolation {
            expr: Expr::Call {
                name: "upper".to_string(),
                args: vec![
                    Arg::StringLiteral("a".to_string()),
                    Arg::StringLiteral("b".to_string()),
                ],
            },
            offset: 0,
            len: 5,
        })];
        let mut scope = Scope::new();
        let mut warnings = vec![];
        let err = evaluate(&nodes, &mut scope, &mut warnings, "", "")
            .expect_err("upper() with 2 args should fail");

        match err {
            crate::error::MdsError::ArityMismatch { span, .. } => {
                assert!(
                    span.is_none(),
                    "evaluator builtin arity error must carry no span, got {span:?}"
                );
            }
            other => panic!("expected ArityMismatch, got {other:?}"),
        }
    }

    // ── @for parity tests (#88) ────────────────────────────────────────────────
    //
    // Each invariant is verified in BOTH markdown mode (a plain template) and
    // messages mode (a template with @message blocks, extracted via
    // into_messages()) to confirm the shared driver preserves identical
    // semantics in both paths.

    #[test]
    fn for_array_loop_text_mode() {
        let src = "---\nitems:\n  - alpha\n  - beta\n---\n@for item in items:\n{{item}}\n@end\n";
        let result = crate::compile_str_md(src).unwrap();
        assert!(result.contains("alpha"), "text: alpha missing");
        assert!(result.contains("beta"), "text: beta missing");
    }

    #[test]
    fn for_array_loop_messages_mode() {
        // Each @message is emitted once per iteration.
        let src = "---\nitems:\n  - alpha\n  - beta\n---\n@for item in items:\n@message user:\n{{item}}\n@end\n@end\n";
        let messages = crate::compile_str(src).unwrap().into_messages().unwrap();
        assert_eq!(
            messages.len(),
            2,
            "messages mode: expected 2 messages, got {}",
            messages.len()
        );
        assert_eq!(messages[0].content, "alpha");
        assert_eq!(messages[1].content, "beta");
    }

    #[test]
    fn for_key_value_alpha_sort_text_mode() {
        // Object keys must appear in alphabetical order regardless of insertion order.
        // Use a run-time vars injection so keys don't appear in the source/frontmatter
        // and we only find them in the loop body output.
        let src = "@for k, v in obj:\n{{k}}\n@end\n";
        use std::collections::HashMap;
        let obj = crate::value::Value::Object(HashMap::from([
            ("zebra".to_string(), crate::value::Value::Number(1.0)),
            ("apple".to_string(), crate::value::Value::Number(2.0)),
            ("mango".to_string(), crate::value::Value::Number(3.0)),
        ]));
        let vars: HashMap<String, crate::value::Value> = HashMap::from([("obj".to_string(), obj)]);
        let result = crate::compile_str_with_md(src, None, Some(vars)).unwrap();
        let positions = ["apple", "mango", "zebra"].map(|k| {
            result
                .find(k)
                .unwrap_or_else(|| panic!("text: key '{k}' missing from output; got: {result:?}"))
        });
        assert!(
            positions[0] < positions[1] && positions[1] < positions[2],
            "text: expected alphabetical order apple < mango < zebra, got positions {positions:?}"
        );
    }

    #[test]
    fn for_key_value_alpha_sort_messages_mode() {
        // Same alpha-sort requirement applies in messages mode.
        let src = "---\nobj:\n  zebra: 1\n  apple: 2\n  mango: 3\n---\n@for k, v in obj:\n@message user:\n{{k}}\n@end\n@end\n";
        let messages = crate::compile_str(src).unwrap().into_messages().unwrap();
        assert_eq!(messages.len(), 3, "messages mode: expected 3 messages");
        let keys: Vec<&str> = messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(
            keys,
            vec!["apple", "mango", "zebra"],
            "messages mode: keys not in alphabetical order"
        );
    }

    #[test]
    fn for_max_loop_iterations_text_mode() {
        // An array larger than MAX_LOOP_ITERATIONS must error in text mode.
        let items: Vec<String> = (0..=MAX_LOOP_ITERATIONS)
            .map(|i| format!("  - {i}"))
            .collect();
        let src = format!(
            "---\nitems:\n{}\n---\n@for item in items:\n{{item}}\n@end\n",
            items.join("\n")
        );
        let err = crate::compile_str_md(&src).expect_err("should error on oversized array");
        assert!(
            format!("{err}").contains("exceeding maximum loop iteration limit"),
            "text: expected resource_limit error, got: {err}"
        );
    }

    #[test]
    fn for_max_loop_iterations_messages_mode() {
        // Same limit fires identically in messages mode.
        let items: Vec<String> = (0..=MAX_LOOP_ITERATIONS)
            .map(|i| format!("  - {i}"))
            .collect();
        let src = format!(
            "---\nitems:\n{}\n---\n@for item in items:\n@message user:\n{{item}}\n@end\n@end\n",
            items.join("\n")
        );
        let err = crate::compile_str_md(&src).expect_err("should error on oversized array");
        assert!(
            format!("{err}").contains("exceeding maximum loop iteration limit"),
            "messages: expected resource_limit error, got: {err}"
        );
    }

    #[test]
    fn for_max_total_iterations_nested_text_mode() {
        // Nested loops that multiply iterations past MAX_TOTAL_ITERATIONS must error.
        // Use a moderately large outer × inner that stays below per-loop cap but
        // exceeds cumulative cap.  MAX_TOTAL_ITERATIONS = 1_000_000, MAX_LOOP_ITERATIONS = 100_000.
        // outer=1001, inner=1000 → 1_001_000 total iterations > 1_000_000.
        let outer: Vec<String> = (0..1001usize).map(|i| format!("  - {i}")).collect();
        let inner: Vec<String> = (0..1000usize).map(|i| format!("  - {i}")).collect();
        let src = format!(
            "---\nouter:\n{}\ninner:\n{}\n---\n@for o in outer:\n@for i in inner:\n{{o}}{{i}}\n@end\n@end\n",
            outer.join("\n"),
            inner.join("\n")
        );
        let err = crate::compile_str_md(&src).expect_err("should error on cumulative limit");
        assert!(
            format!("{err}").contains("total loop iterations exceeded maximum"),
            "text: expected total-iterations error, got: {err}"
        );
    }

    #[test]
    fn for_max_total_iterations_nested_messages_mode() {
        // Identical nested-loop scenario fires the same cumulative cap in messages mode.
        // The inner loop body is whitespace-only (no @message, no mixed content), so the
        // iteration counter drives the failure: MAX_TOTAL_ITERATIONS (1_000_000) trips
        // before MAX_MESSAGE_COUNT (10_000) — zero messages are produced inside the loops.
        // outer=1001, inner=1000 → 1_001_000 total iterations > MAX_TOTAL_ITERATIONS.
        // A messages template needs at least one @message, so we put one before the loops.
        let outer: Vec<String> = (0..1001usize).map(|i| format!("  - {i}")).collect();
        let inner: Vec<String> = (0..1000usize).map(|i| format!("  - {i}")).collect();
        let src = format!(
            "---\nouter:\n{outer}\ninner:\n{inner}\n---\n@message system:\nsetup\n@end\n@for o in outer:\n@for i in inner:\n@end\n@end\n",
            outer = outer.join("\n"),
            inner = inner.join("\n")
        );
        let err = crate::compile_str_md(&src).expect_err("should error on cumulative limit");
        assert!(
            format!("{err}").contains("total loop iterations exceeded maximum"),
            "messages: expected total-iterations error, got: {err}"
        );
    }

    #[test]
    fn for_empty_iterable_text_mode() {
        // An empty array produces empty output (just frontmatter passthrough), no error.
        // Use a sentinel body string that cannot appear in the frontmatter.
        let src = "---\nitems: []\n---\n@for item in items:\nSENTINEL_BODY\n@end\n";
        let result = crate::compile_str_md(src).unwrap();
        assert!(
            !result.contains("SENTINEL_BODY"),
            "text: empty iterable should produce no loop-body output, got: {result}"
        );
    }

    #[test]
    fn for_empty_iterable_messages_mode() {
        // An empty array in messages mode produces zero messages, no error.
        let src =
            "---\nitems: []\n---\n@for item in items:\n@message user:\n{{item}}\n@end\n@end\n";
        let messages = crate::compile_str(src).unwrap().into_messages().unwrap();
        assert_eq!(
            messages.len(),
            0,
            "messages: empty iterable should produce 0 messages"
        );
    }

    #[test]
    fn for_loop_var_shadows_outer_var_text_mode() {
        // A loop variable with the same name as an outer variable shadows it
        // for the duration of the loop body, then the outer value is restored.
        let src = "---\nitem: outer\nitems:\n  - inner\n---\nbefore:{{item}}\n@for item in items:\nduring:{{item}}\n@end\nafter:{{item}}\n";
        let result = crate::compile_str_md(src).unwrap();
        assert!(
            result.contains("before:outer"),
            "text: outer var should be 'outer' before loop, got: {result}"
        );
        assert!(
            result.contains("during:inner"),
            "text: loop var should shadow with 'inner' inside loop, got: {result}"
        );
        assert!(
            result.contains("after:outer"),
            "text: outer var should be restored after loop, got: {result}"
        );
    }

    #[test]
    fn for_loop_var_shadows_outer_var_messages_mode() {
        // Same shadowing behaviour in messages mode.
        let src = "---\nitem: outer\nitems:\n  - inner\n---\n@message system:\nbefore:{{item}}\n@end\n@for item in items:\n@message user:\nduring:{{item}}\n@end\n@end\n@message system:\nafter:{{item}}\n@end\n";
        let messages = crate::compile_str(src).unwrap().into_messages().unwrap();
        assert_eq!(messages.len(), 3, "expected 3 messages");
        assert_eq!(
            messages[0].content, "before:outer",
            "messages: outer var before loop"
        );
        assert_eq!(
            messages[1].content, "during:inner",
            "messages: shadow var inside loop"
        );
        assert_eq!(
            messages[2].content, "after:outer",
            "messages: outer var restored after loop"
        );
    }

    #[test]
    fn for_messages_mode_max_message_count_enforced() {
        // MAX_MESSAGE_COUNT fires in messages mode when a loop generates too many messages.
        // 10_001 messages > MAX_MESSAGE_COUNT (10_000).
        let items: Vec<String> = (0..10_001usize).map(|i| format!("  - msg{i}")).collect();
        let src = format!(
            "---\nitems:\n{}\n---\n@for item in items:\n@message user:\n{{item}}\n@end\n@end\n",
            items.join("\n")
        );
        let err = crate::compile_str_md(&src).expect_err("should error on message count");
        assert!(
            format!("{err}").contains("message count exceeded maximum"),
            "expected MAX_MESSAGE_COUNT error, got: {err}"
        );
    }

    #[test]
    fn for_messages_mode_total_size_enforced() {
        // MAX_MESSAGES_TOTAL_SIZE = MAX_OUTPUT_SIZE = 50 MiB = 52_428_800 bytes.
        // 1_000 messages × 52_429 bytes each = 52_429_000 > 52_428_800 → fires.
        // Each individual body (52_429 bytes) is well below the per-body cap (50 MiB).
        let body = "x".repeat(52_429);
        let items: Vec<String> = (0..1_000usize).map(|i| format!("  - item{i}")).collect();
        let src = format!(
            "---\nitems:\n{items}\n---\n@for item in items:\n@message user:\n{body}\n@end\n@end\n",
            items = items.join("\n"),
            body = body
        );
        let err = crate::compile_str_md(&src).expect_err("should error on total size");
        assert!(
            format!("{err}").contains("total message content exceeds maximum cumulative size"),
            "expected MAX_MESSAGES_TOTAL_SIZE error, got: {err}"
        );
    }

    #[test]
    fn for_object_in_array_mode_errors() {
        // Iterating an object with array syntax (@for x in obj:) must error in both modes.
        // The validator fires first with a static-analysis hint; the evaluator would fire
        // at runtime. Either way, the error message tells users to use key-value syntax.
        let src = "---\nobj:\n  a: 1\n---\n@for x in obj:\n{{x}}\n@end\n";
        let err = crate::compile_str_md(src).expect_err("object in array syntax should error");
        let msg = format!("{err}");
        assert!(
            msg.contains("key, value") || msg.contains("key-value"),
            "text: wrong error for object-in-array syntax: {err}"
        );
    }

    #[test]
    fn for_object_in_array_mode_errors_messages() {
        let src = "---\nobj:\n  a: 1\n---\n@for x in obj:\n@message user:\n{{x}}\n@end\n@end\n";
        let err = crate::compile_str_md(src).expect_err("object in array syntax should error");
        let msg = format!("{err}");
        assert!(
            msg.contains("key, value") || msg.contains("key-value"),
            "messages: wrong error for object-in-array syntax: {err}"
        );
    }
}
