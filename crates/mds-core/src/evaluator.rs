use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

pub(crate) mod budget;
use budget::{
    bytes, display_len, format_exact, format_len, render_value, table_bytes, value_bytes,
    MemBudget, Part, NODE, VALUE_ROOT,
};

use crate::arity::check_arity;
use crate::ast::{
    render_signature, required_param_count, Arg, BlockNode, Condition, Expr, ForBlock, IfBlock,
    IncludeDirective, MessageBlock, Node,
};
use crate::builtins::{Bound, BuiltinMeta};
use crate::error::MdsError;
use crate::limits::{
    MAX_ARRAY_ELEMENTS, MAX_DOT_SEGMENTS, MAX_ELSEIF_BRANCHES, MAX_MESSAGES_TOTAL_SIZE,
    MAX_MESSAGE_COUNT, MAX_OUTPUT_SIZE, MAX_TOTAL_FUNCTION_CALLS,
};
use crate::scope::{FunctionDef, PathStep, PromptText, Scope, Slot};
use crate::value::Value;

/// Maximum call depth to prevent stack overflow from deeply nested calls.
const MAX_CALL_DEPTH: usize = 128;

/// Maximum number of iterations allowed in a single @for loop.
const MAX_LOOP_ITERATIONS: usize = 100_000;

/// Maximum total iterations across all loops in a single module evaluation.
/// This prevents nested loops from multiplying iterations into the billions.
const MAX_TOTAL_ITERATIONS: usize = 1_000_000;

/// Maximum number of accumulated warnings before further warnings are silently dropped.
const MAX_WARNINGS: usize = 1_000;
const FRAME_ENTRY: usize = 2 * size_of::<(String, Slot)>() + 2;
const _: () = assert!(FRAME_ENTRY >= 2 * size_of::<(String, Slot)>() + 2);

/// The bytes of one frame entry: its name, the slot pair and the bound value.
fn entry_bytes(name: &str, bound_value: Option<usize>) -> Option<usize> {
    name.len()
        .checked_add(FRAME_ENTRY)?
        .checked_add(bound_value?)
}

fn charge_literal(mem: &mut MemBudget, len: usize) -> Result<usize, MdsError> {
    mem.charge(Part::Temp, VALUE_ROOT.checked_add(len))
}

/// Cumulative resource budgets for evaluation.
///
/// Each module starts a new loop and message budget. All regions of a module share
/// those budgets. All modules and regions in a compile share the function call budget.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct EvalBudget {
    /// Loop iterations across all `@for` loops (capped at `MAX_TOTAL_ITERATIONS`).
    iterations: usize,
    /// Message content bytes collected in messages mode (capped at
    /// `MAX_MESSAGES_TOTAL_SIZE`). Checked as each message is collected, so runaway
    /// growth is caught before a giant `Vec` is built.
    message_bytes: usize,
    function_calls: usize,
    pub(crate) mem: MemBudget,
}

impl EvalBudget {
    pub(crate) fn start_module(&mut self) -> &mut Self {
        self.iterations = 0;
        self.message_bytes = 0;
        self
    }

    #[cfg(test)]
    pub(crate) fn seed_function_calls(&mut self, spent: usize) {
        self.function_calls = spent;
    }

    fn charge_function_call(&mut self) -> Result<(), MdsError> {
        if self.function_calls >= MAX_TOTAL_FUNCTION_CALLS {
            return Err(MdsError::resource_limit(format!(
                "compile exceeds {MAX_TOTAL_FUNCTION_CALLS} function calls"
            )));
        }
        self.function_calls += 1;
        Ok(())
    }
}

/// Transient state threaded through the evaluator for a single compilation.
///
/// Bundles the mutable parameters that were previously threaded individually
/// through every function signature, reducing arity and making call sites clearer.
pub(crate) struct EvalContext<'a> {
    /// LIFO stack of active function call keys, used to detect direct recursion.
    /// Vec is used for O(n) contains at MAX_CALL_DEPTH=128 — acceptable.
    call_stack: Vec<String>,
    /// Cumulative budgets, seeded by the caller and handed back when evaluation ends.
    budget: EvalBudget,
    /// Accumulated warnings (e.g. empty @include).
    warnings: &'a mut Vec<String>,
    /// Optional source-map builder.  Present when `CompileOptions::source_map` is
    /// `true`.  `None` when the caller did not request a source map — zero-cost path
    /// (AC-PERF-01: no allocation when off).
    pub(crate) map: Option<crate::sourcemap::MapBuilder>,
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
#[cfg(test)]
pub fn evaluate(
    nodes: &[Node],
    scope: &mut Scope,
    warnings: &mut Vec<String>,
    file: &str,
    source: &str,
) -> Result<String, MdsError> {
    let mut out = String::new();
    evaluate_seeded(
        nodes,
        scope,
        warnings,
        file,
        source,
        &mut EvalBudget::default(),
        &mut out,
    )?;
    Ok(out)
}

/// Evaluate a module body with the caller's cumulative budget.
///
/// Used to evaluate several node slices that make up ONE module (the spliced regions
/// of an `@extends` chain) under one set of caps. `budget` holds the running totals
/// when this returns, on success and on error alike. Append to the caller's `out`.
pub(crate) fn evaluate_seeded(
    nodes: &[Node],
    scope: &mut Scope,
    warnings: &mut Vec<String>,
    file: &str,
    source: &str,
    budget: &mut EvalBudget,
    out: &mut String,
) -> Result<(), MdsError> {
    let mut ctx = EvalContext {
        call_stack: Vec::new(),
        budget: *budget,
        warnings,
        map: None,
        fn_body_owned: false,
        file,
        source,
        body_origin: None,
    };
    let result = evaluate_nodes(nodes, scope, &mut ctx, out);
    *budget = ctx.budget;
    result
}

/// Evaluate a module body into a rendered string while recording source-map
/// segments via `builder`.
///
/// Returns `(output, builder)` so the caller can pass the builder on to the
/// finalization stage.  The builder's `cursor` is guaranteed to equal
/// `output.len() as u32` when this function returns.
///
/// `file` and `source` for `EvalContext` diagnostic spans are derived from the
/// builder's current source — single source of truth, no redundant parameter
/// (avoids the dual-channel mis-attribution class fixed in c5a4d65; issue #58).
///
/// Delegates to [`evaluate_with_map_seeded`] with a fresh [`EvalBudget`].
/// Use [`evaluate_with_map_seeded`] directly when you need to carry cumulative
/// resource budgets across multiple invocations (e.g. `@extends` regions).
#[cfg(test)]
fn evaluate_with_map(
    nodes: &[Node],
    scope: &mut Scope,
    warnings: &mut Vec<String>,
    builder: crate::sourcemap::MapBuilder,
) -> Result<(String, crate::sourcemap::MapBuilder), MdsError> {
    let mut out = String::new();
    let map = evaluate_with_map_seeded(
        nodes,
        scope,
        warnings,
        builder,
        &mut EvalBudget::default(),
        &mut out,
    )?;
    Ok((out, map))
}

/// Evaluate nodes with source-map recording, spending from the caller's `budget`.
///
/// The budget includes prior work, such as earlier `@extends` regions.
/// It holds the running totals on success and on error alike.
///
/// `file` and `source` for `EvalContext` diagnostic spans are derived from the
/// builder's current source — the single source of truth.  Callers must switch
/// it to the region's origin before each call (as `evaluate_regions_with_map` does)
/// so that the derived values reflect the correct region origin (issue #58;
/// avoids the dual-channel mis-attribution class fixed in c5a4d65).
///
/// Used by `evaluate_regions_with_map` (resolver.rs) to enforce a single cumulative
/// iteration cap across all spliced `@extends` regions (REL-1, applies PF-004).
///
/// Append to `out` and return the builder. Its cursor must equal the absolute
/// output offset on entry. A missing builder or unregistered source returns an error.
pub(crate) fn evaluate_with_map_seeded(
    nodes: &[Node],
    scope: &mut Scope,
    warnings: &mut Vec<String>,
    builder: crate::sourcemap::MapBuilder,
    budget: &mut EvalBudget,
    out: &mut String,
) -> Result<crate::sourcemap::MapBuilder, MdsError> {
    // Take the current source's Origin before moving builder into EvalContext.map.
    // This is the single source of truth for diagnostic span attribution —
    // eliminates the redundant explicit params that caused mis-attributed spans
    // before c5a4d65 (issue #58). Cloning it bumps refcounts; it copies no text, so
    // evaluating an @extends chain region by region stays linear (#416).
    //
    // R3 (CWE-209): `display` (root-relative, never absolute), not `file` (the
    // canonical key, may be absolute), so that file-backed compile error messages
    // show "src/a.mds" instead of "/proj/src/a.mds".
    let origin = builder.current_origin().cloned().ok_or_else(|| {
        MdsError::syntax(
            "internal: MapBuilder current_src names no registered source — compiler bug",
        )
    })?;
    let mut ctx = EvalContext {
        call_stack: Vec::new(),
        budget: *budget,
        warnings,
        map: Some(builder),
        fn_body_owned: false,
        file: &origin.display,
        source: &origin.source,
        body_origin: None,
    };
    let result = evaluate_nodes(nodes, scope, &mut ctx, out);
    *budget = ctx.budget;
    result?;
    ctx.map.take().ok_or_else(|| {
        MdsError::syntax("internal: MapBuilder disappeared during evaluate_with_map — compiler bug")
    })
}

fn evaluate_nodes(
    nodes: &[Node],
    scope: &mut Scope,
    ctx: &mut EvalContext,
    out: &mut String,
) -> Result<(), MdsError> {
    let start_len = out.len();
    // The cursor is absolute even when a function body uses a separate buffer.
    let saved_cursor = ctx.map.as_ref().map_or(0, |map| map.cursor);

    for node in nodes {
        let mark = ctx.budget.mem.mark();
        let result = (|| -> Result<(), MdsError> {
            let abs_out = saved_cursor + (out.len() - start_len) as u32;
            let slot;
            let rendered;
            let included;
            // Child bodies append to `out` directly. Leaf text uses one capped append.
            let piece = match node {
                Node::Text(t) => {
                    if let Some(ref mut map) = ctx.map {
                        if map.suppress == 0 {
                            // Enforced in release builds too (#220): `cursor` is the absolute
                            // output position every following segment is computed from, so a
                            // desynchronised cursor mis-attributes the whole rest of the map
                            // in silence — a source map that points at the wrong bytes is
                            // worse than no source map. Only a defect in the arms that
                            // re-anchor the cursor can trip it; no template input can.
                            // Cost: one u32 compare per node, only while a source map is
                            // being recorded.
                            assert_eq!(
                                map.cursor, abs_out,
                                "source-map cursor desynchronised from the output length at \
                             a Text node: every following segment would map to the wrong \
                             output offset"
                            );
                            map.push_segment(abs_out, t.offset as u32, t.text.len() as u32);
                        }
                    }
                    Some(t.text.as_str())
                }
                Node::EscapedBrace { offset } => {
                    if let Some(ref mut map) = ctx.map {
                        if map.suppress == 0 {
                            // Enforced in release too (#220); see the Text arm above.
                            assert_eq!(
                                map.cursor, abs_out,
                                "source-map cursor desynchronised from the output length at \
                             an EscapedBrace node: every following segment would map to \
                             the wrong output offset"
                            );
                            // Source span: `\{{` is 3 source bytes (backslash + two braces).
                            map.push_segment(abs_out, *offset as u32, 3);
                        }
                    }
                    Some("{{")
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
                            // Enforced in release too (#220); see the Text arm above.
                            assert_eq!(
                                map.cursor, abs_out,
                                "source-map cursor desynchronised from the output length at \
                             an Interpolation node: every following segment would map to \
                             the wrong output offset"
                            );
                        }
                        // Always anchor cursor so inner evaluate_nodes invocations
                        // (invoke_function / S8 body descent) start from the correct
                        // absolute position.
                        map.cursor = abs_out;
                    }
                    slot = evaluate_expr(&interp.expr, scope, ctx)?;
                    rendered = render_expr(&interp.expr, &slot, &mut ctx.budget.mem)?;
                    if let Some(ref mut map) = ctx.map {
                        if map.suppress == 0 && !ctx.fn_body_owned {
                            // S3 path: body was suppressed; record the call-site span.
                            // On the S8 path, invoke_function already pushed fine-grained
                            // body segments attributed to the definition file — skip here.
                            map.push_segment(abs_out, interp.offset as u32, interp.len as u32);
                        }
                    }
                    Some(rendered.as_ref())
                }
                Node::If(block) => {
                    if let Some(ref mut map) = ctx.map {
                        map.cursor = abs_out;
                    }
                    evaluate_if(block, scope, ctx, out)?;
                    None
                }
                Node::For(block) => {
                    if let Some(ref mut map) = ctx.map {
                        map.cursor = abs_out;
                    }
                    evaluate_for(block, scope, ctx, out)?;
                    None
                }
                Node::Define(_) | Node::Import(_) | Node::Export(_) => {
                    // The resolver handles these directives.
                    return Ok(());
                }
                Node::Include(inc) => {
                    if let Some(ref mut map) = ctx.map {
                        map.cursor = abs_out;
                    }
                    included = evaluate_include(inc, scope, ctx)?;
                    included.as_deref()
                }
                Node::Message(block) => {
                    // Text mode: render the body inline, ignoring the role marker.
                    // This maintains full backward compatibility — @message blocks are
                    // transparent when compiling to plain text.
                    if let Some(ref mut map) = ctx.map {
                        map.cursor = abs_out;
                    }
                    evaluate_nodes(&block.body, scope, ctx, out)?;
                    None
                }
                Node::Block(block) => {
                    // Standalone mode: render the block's default body inline.
                    // The block markers are invisible in text output — @block is a
                    // transparent wrapper in non-extending files. An `@extends` chain
                    // never reaches this arm: the resolver evaluates each block's
                    // effective body (base default or child override) as its own region.
                    if let Some(ref mut map) = ctx.map {
                        map.cursor = abs_out;
                    }
                    evaluate_block(block, scope, ctx, out)?;
                    None
                }
            };
            if let Some(piece) = piece {
                push_capped(out, piece, &mut ctx.budget.mem)?;
            }
            // Re-anchor after the node, including after an empty child body.
            if let Some(ref mut map) = ctx.map {
                map.cursor = saved_cursor + (out.len() - start_len) as u32;
            }
            Ok(())
        })();
        ctx.budget.mem.release_to(mark);
        result?;
    }

    Ok(())
}

/// Append `add` to `out` after the size and memory checks pass.
///
/// Every output accumulator appends through this (#415), so `MAX_OUTPUT_SIZE` is
/// checked before the bytes land: a loop crossing the cap stops on the crossing pass
/// instead of building its whole output first. Growth stays geometric, like
/// `push_str`, but never reserves past the cap, so capacity is bounded too — plain
/// doubling would take a buffer to about twice the cap.
///
/// The memory budget also counts the capacity of each live output buffer.
fn push_capped(out: &mut String, add: &str, mem: &mut MemBudget) -> Result<(), MdsError> {
    let needed = out
        .len()
        .checked_add(add.len())
        .filter(|&needed| needed <= MAX_OUTPUT_SIZE)
        .ok_or_else(MdsError::output_size_exceeded)?;
    if needed > out.capacity() {
        // `needed <= MAX_OUTPUT_SIZE`, so the target is at least `needed`.
        let target = out
            .capacity()
            .saturating_mul(2)
            .max(needed)
            .min(MAX_OUTPUT_SIZE);
        mem.charge(Part::Buffers, Some(target - out.capacity()))?;
        out.reserve_exact(target - out.len());
        if out.capacity() > target {
            mem.charge(Part::Buffers, Some(out.capacity() - target))?;
        }
    }
    out.push_str(add);
    Ok(())
}

/// Render a `@block` node's body inline.
///
/// In standalone mode (no `@extends`) the block body is the default content and is
/// rendered as plain text — the `@block` markers are invisible.
fn evaluate_block(
    block: &BlockNode,
    scope: &mut Scope,
    ctx: &mut EvalContext,
    out: &mut String,
) -> Result<(), MdsError> {
    evaluate_nodes(&block.body, scope, ctx, out)
}

/// Return the bound slot only if its projection still refers to a value.
fn resolve_var(name: &str, scope: &Scope) -> Result<Slot, MdsError> {
    scope
        .get_slot(name)
        .filter(|slot| slot.get().is_some())
        .cloned()
        .ok_or_else(|| MdsError::undefined_var(name))
}

fn invalid_projection() -> MdsError {
    MdsError::syntax("internal error: an evaluated value has an invalid projection")
}

/// Resolve each field against the root slot. An empty path returns the slot itself.
fn resolve_dot_path(
    root: &str,
    fields: &[String],
    scope: &Scope,
    mem: &mut MemBudget,
) -> Result<Slot, MdsError> {
    if fields.len() > MAX_DOT_SEGMENTS {
        return Err(MdsError::syntax(format!(
            "dot path exceeds maximum segment count of {MAX_DOT_SEGMENTS}"
        )));
    }
    let mut current = resolve_var(root, scope)?;
    for (i, field) in fields.iter().enumerate() {
        // Build the traversed path for error messages (e.g. "a.b" when failing at "c").
        let traversed_path = || {
            std::iter::once(root)
                .chain(fields[..i].iter().map(|s| s.as_str()))
                .collect::<Vec<_>>()
                .join(".")
        };
        match current.get().ok_or_else(invalid_projection)? {
            Value::Object(map) if map.contains_key(field) => {
                mem.charge(Part::Temp, Slot::PROJECTION_BYTES.checked_add(field.len()))?;
                current = current.project(PathStep::Key(field.clone()));
            }
            Value::Object(_) => {
                return Err(MdsError::syntax(format!(
                    "field '{field}' not found on '{}'",
                    traversed_path()
                )));
            }
            value => {
                return Err(MdsError::syntax(format!(
                    "cannot access field '{field}' on {} '{}'",
                    value.type_name(),
                    traversed_path()
                )));
            }
        }
    }
    Ok(current)
}

/// Evaluate an expression to a slot that holds or projects a runtime value.
///
/// This is the core expression evaluator used by interpolation and directive expressions.
fn evaluate_expr(expr: &Expr, scope: &mut Scope, ctx: &mut EvalContext) -> Result<Slot, MdsError> {
    match expr {
        Expr::Var(name) => resolve_var(name, scope),
        Expr::Call { name, args } => call_function(name, args, scope, ctx, 0),
        Expr::QualifiedCall {
            namespace,
            name,
            args,
        } => call_qualified_function(namespace, name, args, scope, ctx),
        Expr::MemberAccess { object, fields } => {
            // Give a targeted error when the name refers to an imported namespace rather
            // than a variable — before delegating to resolve_dot_path which only looks up vars.
            if scope.get_slot(object).is_none() && scope.get_namespace(object).is_some() {
                return Err(MdsError::syntax(format!(
                    "'{object}' is an imported module, not a variable — to call a function use {{{object}.func()}}"
                )));
            }
            resolve_dot_path(object, fields, scope, &mut ctx.budget.mem)
        }
        literal => {
            charge_literal(&mut ctx.budget.mem, literal_text_len(literal))?;
            literal_expr_to_value(literal).map(Slot::from)
        }
    }
}

/// Render an evaluated expression. Borrow strings and reject direct object interpolation.
fn render_expr<'a>(
    expr: &Expr,
    slot: &'a Slot,
    mem: &mut MemBudget,
) -> Result<Cow<'a, str>, MdsError> {
    let value = slot.get().ok_or_else(invalid_projection)?;
    match value {
        Value::String(text) => Ok(Cow::Borrowed(text)),
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
        _ => {
            let len = mem.charge(Part::Temp, display_len(value))?;
            render_value(value, len).map(Cow::Owned)
        }
    }
}

fn resolve_args(
    args: &[Arg],
    scope: &mut Scope,
    ctx: &mut EvalContext,
    depth: usize,
) -> Result<Vec<Slot>, MdsError> {
    if depth > MAX_CALL_DEPTH {
        return Err(MdsError::recursion(format!(
            "argument expression depth exceeds {MAX_CALL_DEPTH}"
        )));
    }
    ctx.budget
        .mem
        .charge(Part::Temp, bytes(args.len(), size_of::<Slot>()))?;
    args.iter()
        .map(|arg| {
            let literal_len = match arg {
                Arg::StringLiteral(s) => s.len(),
                Arg::NumberLiteral(_) | Arg::BooleanLiteral(_) | Arg::NullLiteral => 0,
                Arg::Var(name) => return resolve_var(name, scope),
                Arg::Call {
                    name,
                    args: inner_args,
                } => {
                    // The callee gets this value; its text is not output here.
                    let held = hold_map(ctx);
                    let slot = call_function(name, inner_args, scope, ctx, depth + 1);
                    release_map(ctx, held);
                    return slot;
                }
                Arg::MemberAccess { object, fields } => {
                    return resolve_dot_path(object, fields, scope, &mut ctx.budget.mem)
                }
            };
            // One charge for every literal, before its value is built.
            charge_literal(&mut ctx.budget.mem, literal_len)?;
            Ok(match arg {
                Arg::StringLiteral(s) => Value::String(s.clone()),
                Arg::NumberLiteral(n) => Value::Number(*n),
                Arg::BooleanLiteral(b) => Value::Boolean(*b),
                // Only literals reach this match; the other arms returned above.
                _ => Value::Null,
            }
            .into())
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

fn literal_text_len(expr: &Expr) -> usize {
    match expr {
        Expr::StringLiteral(s) => s.len(),
        _ => 0,
    }
}

fn check_function_arity(
    func: &FunctionDef,
    call_key: &str,
    arg_count: usize,
) -> Result<(), MdsError> {
    let required = required_param_count(&func.params);
    let total = func.params.len();
    if !check_arity(arg_count, required, total) {
        return Err(MdsError::arity(
            call_key,
            required,
            total,
            arg_count,
            Some(render_signature(call_key, &func.params)),
        ));
    }
    Ok(())
}

fn invoke_function(
    func: &Arc<FunctionDef>,
    call_key: &str,
    args: &[Slot],
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
    check_function_arity(func, call_key, args.len())?;
    ctx.budget.charge_function_call()?;
    let mark = ctx.budget.mem.mark();
    ctx.budget.mem.charge(Part::Temp, Some(call_key.len()))?;
    let mut body = String::new();
    scope.push_call(func);
    let result = evaluate_function_body(func, call_key, args, scope, ctx, &mut body);
    let result = prefer_first_error(result, scope.pop());
    ctx.budget.mem.release_to(mark);
    if result.is_ok() {
        ctx.budget
            .mem
            .transfer(Part::Buffers, Part::Temp, body.capacity());
    } else {
        ctx.budget.mem.release_buffers(body.capacity());
    }
    result.map(|()| body)
}

fn bind_call_params(
    params: &[crate::ast::Param],
    args: &[Slot],
    scope: &mut Scope,
    mem: &mut MemBudget,
) -> Result<(), MdsError> {
    // Bind params index-by-index; fill missing optional params with their defaults.
    for (i, param) in params.iter().enumerate() {
        let slot = if let Some(arg) = args.get(i) {
            // The slot shares the argument already live in the caller's mark.
            mem.charge(Part::Temp, entry_bytes(&param.name, Some(0)))?;
            arg.clone()
        } else {
            // Required params were already checked above; this arm only fires for
            // optional params not supplied by the caller.
            let default = param.default.as_ref().ok_or_else(|| {
                MdsError::syntax(format!(
                    "internal error: non-optional param '{}' missing but arity check passed",
                    param.name
                ))
            })?;
            mem.charge(
                Part::Temp,
                entry_bytes(
                    &param.name,
                    VALUE_ROOT.checked_add(literal_text_len(default)),
                ),
            )?;
            literal_expr_to_value(default)?.into()
        };
        scope.set_slot(&param.name, slot);
    }
    Ok(())
}

fn evaluate_function_body(
    func: &FunctionDef,
    call_key: &str,
    args: &[Slot],
    scope: &mut Scope,
    ctx: &mut EvalContext,
    raw_body: &mut String,
) -> Result<(), MdsError> {
    bind_call_params(&func.params, args, scope, &mut ctx.budget.mem)?;

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
    //  2. suppress == 0 — we are not inside a suppressed body or a held region
    //     (`hold_map`: a condition, an iterable or a call argument).
    //  3. func.origin is Some — the function has provenance metadata (populated
    //     unconditionally since B1 dropped the source_map_mode gate, S7).
    //
    // On this path, instead of suppressing the body, we:
    //  - Switch the MapBuilder's current source to the definition file.
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
                let sr = map.switch_to(origin);
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
        let body_result = evaluate_nodes(&func.body, scope, ctx, raw_body);
        let popped = ctx.call_stack.pop();

        // Restore source attribution for the outer scope.
        if let Some(ref mut map) = ctx.map {
            map.switch_to_index(saved_src);
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
        prefer_first_error(body_result, lifo_result)?;

        // Compute lead/trail byte counts for rebase_trim.
        // Use try_from + unwrap_or(u32::MAX) for safety: body is bounded by
        // MAX_OUTPUT_SIZE (50 MB) so try_from always succeeds in practice;
        // u32::MAX fallback causes rebase_trim to drop all segments (safe degradation).
        let untrimmed_len = u32::try_from(raw_body.len()).unwrap_or(u32::MAX);
        let (lead, trail) = trim_body(raw_body, &mut ctx.budget.mem);
        let lead = u32::try_from(lead).unwrap_or(u32::MAX);
        let trail = u32::try_from(trail).unwrap_or(u32::MAX);
        let trimmed_len = u32::try_from(raw_body.len()).unwrap_or(u32::MAX);

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
            // Advance to the trimmed body end, where the enclosing node also sets
            // the cursor after the append.
            map.cursor = start_cursor.saturating_add(trimmed_len);
        }

        // Signal to the Interpolation arm: body owns the segments.
        ctx.fn_body_owned = true;

        Ok(())
    } else {
        // S3 path: suppress source-map recording inside function bodies.
        // The call site (the Interpolation node) is recorded as a single point
        // by the Interpolation arm when fn_body_owned stays false.
        if let Some(ref mut map) = ctx.map {
            map.suppress += 1;
        }
        ctx.call_stack.push(call_key.to_string());
        let result = evaluate_nodes(&func.body, scope, ctx, raw_body).map(|()| {
            trim_body(raw_body, &mut ctx.budget.mem);
        });
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
        ctx.body_origin = prev_body_origin;
        prefer_first_error(result, lifo_result)
    }
}

/// Trim the buffer and return the original leading and trailing byte counts.
fn trim_body(body: &mut String, mem: &mut MemBudget) -> (usize, usize) {
    let lead = body.len() - body.trim_start().len();
    let end = body.trim_end().len();
    let trail = body.len() - end;
    body.truncate(end);
    // The two edges overlap if the entire body is whitespace.
    drop(body.drain(..lead.min(end)));
    // Do not keep a large allocation for a small result after the trim.
    let capacity = body.capacity();
    if capacity > 2 * body.len() {
        body.shrink_to_fit();
        mem.release_buffers(capacity - body.capacity());
    }
    (lead, trail)
}

/// Start a region whose text does not go to the output: a condition, an iterable or
/// a call argument. While the region is held, the source map records nothing: a call
/// takes the suppressed path, so it switches to no source and registers none. The
/// cursor stays one global output offset. `None` when no map is recorded.
#[inline(never)]
fn hold_map(ctx: &mut EvalContext) -> Option<(u32, usize)> {
    let map = ctx.map.as_mut()?;
    map.suppress += 1;
    Some((map.cursor, map.segments.len()))
}

/// End a region that [`hold_map`] started: put back the cursor and the segment count.
/// Call it on the success path and on the error path.
#[inline(never)]
fn release_map(ctx: &mut EvalContext, held: Option<(u32, usize)>) {
    if let (Some(map), Some((cursor, segments))) = (ctx.map.as_mut(), held) {
        map.suppress = map.suppress.saturating_sub(1);
        map.segments.truncate(segments);
        map.cursor = cursor;
    }
}

fn call_function(
    name: &str,
    args: &[Arg],
    scope: &mut Scope,
    ctx: &mut EvalContext,
    depth: usize,
) -> Result<Slot, MdsError> {
    // User-defined functions take priority (shadowing built-ins).
    if let Some(func) = scope.get_function(name).cloned() {
        check_function_arity(&func, name, args.len())?;
        let args = resolve_args(args, scope, ctx, depth)?;
        return invoke_with_args(&func, name, &args, scope, ctx);
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
        return invoke_builtin(meta, args, scope, ctx, depth);
    }
    // An argument error still takes priority when the callee does not exist.
    resolve_args(args, scope, ctx, depth)?;
    Err(MdsError::undefined_fn(name))
}

fn invoke_with_args(
    func: &Arc<FunctionDef>,
    call_key: &str,
    args: &[Slot],
    scope: &mut Scope,
    ctx: &mut EvalContext,
) -> Result<Slot, MdsError> {
    ctx.budget.mem.charge(Part::Temp, Some(VALUE_ROOT))?;
    invoke_function(func, call_key, args, scope, ctx).map(|s| Value::String(s).into())
}

fn invoke_builtin(
    meta: &BuiltinMeta,
    args: &[Arg],
    scope: &mut Scope,
    ctx: &mut EvalContext,
    depth: usize,
) -> Result<Slot, MdsError> {
    let mark = ctx.budget.mem.mark();
    let result = (|| {
        let slots = resolve_args(args, scope, ctx, depth)?;
        ctx.budget
            .mem
            .charge(Part::Temp, bytes(slots.len(), NODE))?;
        let mut values = Vec::with_capacity(slots.len());
        let mut input_bytes = 0;
        for slot in slots {
            let copies = slot.copies_on_take();
            if copies || (values.is_empty() && matches!(meta.bound, Bound::Input)) {
                let value = slot.get().ok_or_else(invalid_projection)?;
                let size = value_bytes(value).ok_or_else(MdsError::compile_memory_exceeded)?;
                if values.is_empty() {
                    input_bytes = size;
                }
                if copies {
                    ctx.budget.mem.charge(Part::Temp, Some(size))?;
                }
            }
            values.push(slot.into_value().ok_or_else(invalid_projection)?);
        }
        ctx.budget.charge_function_call()?;
        let bound = builtin_bound(meta, &values, input_bytes);
        if let Some(n) = bound {
            ctx.budget.mem.charge(Part::Temp, n)?;
        }
        (meta.handler)(&values).map(|value| (value, bound.flatten()))
    })();
    ctx.budget.mem.release_to(mark);
    let (value, bound) = result?;
    let size = if matches!(meta.bound, Bound::Input) {
        value_bytes(&value)
    } else {
        bound
    };
    ctx.budget.mem.charge(Part::Temp, size)?;
    Ok(value.into())
}

/// The charge before one built-in call. `None` charges nothing. `Some(None)`
/// is a count past the address space, which the charge refuses.
fn builtin_bound(
    meta: &BuiltinMeta,
    values: &[Value],
    input_bytes: usize,
) -> Option<Option<usize>> {
    Some(match meta.bound {
        Bound::Scalar => Some(VALUE_ROOT),
        // Capped counts return None above their cap. Invalid arguments keep the
        // handler's diagnostic; no result buffer is allocated on that path.
        Bound::Exact(count) => count(values)?.checked_add(VALUE_ROOT),
        Bound::Input => input_builtin_bound(meta.name, values.first(), input_bytes),
        Bound::Split => split_bound(values),
    })
}

fn input_builtin_bound(name: &str, value: Option<&Value>, input_bytes: usize) -> Option<usize> {
    let array = value.and_then(Value::as_array);
    let count = array.map_or(0, |items| items.len());
    let bound = bytes(input_bytes, 2)?.checked_add(bytes(count, 32)?)?;
    if name == "unique" {
        if let Some(items) = array {
            // Numeric formatting can be longer than the in-memory value. Count
            // each key's bytes plus the entire seen table before it is built.
            let keys = items.iter().try_fold(0usize, |sum, item| {
                let key_len = match item {
                    Value::Number(n) => crate::builtins::number_key_len(*n)?,
                    _ => display_len(item)?.checked_add(5)?,
                };
                sum.checked_add(key_len)
            })?;
            let table = table_bytes(count, size_of::<String>())?;
            return Some(bound.max(input_bytes.checked_add(keys)?.checked_add(table)?));
        }
    }
    Some(bound)
}

fn split_bound(values: &[Value]) -> Option<usize> {
    let [Value::String(s), Value::String(sep)] = values else {
        return Some(0);
    };
    let Some(parts) = s.len().checked_div(sep.len()) else {
        return Some(0);
    };
    let capacity = parts
        .saturating_add(1)
        .clamp(64, MAX_ARRAY_ELEMENTS)
        .next_power_of_two();
    VALUE_ROOT
        .checked_add(s.len())?
        .checked_add(bytes(capacity, NODE)?)
}

fn call_qualified_function(
    namespace: &str,
    name: &str,
    args: &[Arg],
    scope: &mut Scope,
    ctx: &mut EvalContext,
) -> Result<Slot, MdsError> {
    let len = ctx.budget.mem.charge(
        Part::Temp,
        namespace
            .len()
            .checked_add(1)
            .and_then(|n| n.checked_add(name.len())),
    )?;
    let mut qualified_name = String::with_capacity(len);
    qualified_name.push_str(namespace);
    qualified_name.push('.');
    qualified_name.push_str(name);
    let callee = scope
        .get_namespace(namespace)
        .ok_or_else(|| MdsError::undefined_var(namespace))
        .and_then(|ns| {
            ns.functions
                .get(name)
                .cloned()
                .ok_or_else(|| MdsError::undefined_fn(&qualified_name))
        });
    if let Ok(func) = &callee {
        check_function_arity(func, &qualified_name, args.len())?;
    }
    // Defer a lookup error until after the arguments have been evaluated.
    let args = resolve_args(args, scope, ctx, 0)?;
    invoke_with_args(&callee?, &qualified_name, &args, scope, ctx)
}

/// Compare two runtime `Value` instances for strict same-type equality.
///
/// Returns `Some(true)` / `Some(false)` for same-type comparisons, and `None`
/// for cross-type comparisons (caller must surface a `TypeMismatch` error).
/// `NaN == NaN` is `Some(false)` (IEEE 754 via Rust's `f64 ==`).
///
/// Same-type values compare through the derived `PartialEq` of `Value`, the one
/// equality path. Arrays and objects use structural (deep) equality: a recursive
/// element-wise comparison. Note that the condition grammar currently cannot
/// express container literals on the RHS, so same-type container comparisons are
/// only reachable at the unit-test level today. The contract still holds for
/// them: same-type always compares structurally, only cross-type is an error.
fn values_equal_same_type(lhs: &Value, rhs: &Value) -> Option<bool> {
    // Cross-type: `None` signals TypeMismatch to the caller.
    (std::mem::discriminant(lhs) == std::mem::discriminant(rhs)).then(|| lhs == rhs)
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

    // Select (display, source) from body_origin when inside a function body,
    // else fall back to the top-level module context (applies PF-012 / R3).
    // Use origin.display (root-relative) rather than origin.file (canonical key) so
    // type-mismatch spans in @define bodies show safe display paths (CWE-209).
    let (file, source) = if let Some(ref origin) = ctx.body_origin {
        (origin.display.as_ref(), origin.source.as_ref())
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
    let mark = ctx.budget.mem.mark();
    // The condition decides the branch; the text of a call in it is not output.
    let held = hold_map(ctx);
    let result = (|| match condition {
        Condition::Truthy(expr) | Condition::Not(expr) => {
            let slot = evaluate_expr(expr, scope, ctx)?;
            let truthy = slot.get().ok_or_else(invalid_projection)?.is_truthy();
            Ok(truthy != matches!(condition, Condition::Not(_)))
        }
        Condition::Eq(lhs, rhs) | Condition::NotEq(lhs, rhs) => {
            let lhs = evaluate_expr(lhs, scope, ctx)?;
            let rhs = evaluate_expr(rhs, scope, ctx)?;
            let lhs = lhs.get().ok_or_else(invalid_projection)?;
            let rhs = rhs.get().ok_or_else(invalid_projection)?;
            let eq = values_equal_same_type(lhs, rhs).ok_or_else(|| {
                build_type_mismatch(ctx, anchor, lhs.type_name(), rhs.type_name())
            })?;
            Ok(eq != matches!(condition, Condition::NotEq(..)))
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
    })();
    release_map(ctx, held);
    ctx.budget.mem.release_to(mark);
    result
}

fn evaluate_if(
    block: &IfBlock,
    scope: &mut Scope,
    ctx: &mut EvalContext,
    out: &mut String,
) -> Result<(), MdsError> {
    // Evaluate the primary condition; anchor span on the @if directive line.
    if evaluate_condition(&block.condition, scope, ctx, Some(block.offset))? {
        return evaluate_nodes(&block.then_body, scope, ctx, out);
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
            return evaluate_nodes(&branch.body, scope, ctx, out);
        }
    }

    // Fall through to @else body if present
    if let Some(else_body) = &block.else_body {
        evaluate_nodes(else_body, scope, ctx, out)
    } else {
        Ok(())
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
    let mark = ctx.budget.mem.mark();
    let result = (|| {
        // The iterable gives the items; the text of a call in it is not output.
        let held = hold_map(ctx);
        let slot = evaluate_expr(&block.iterable, scope, ctx);
        release_map(ctx, held);
        let slot = slot?;
        let iterable = slot.get().ok_or_else(invalid_projection)?;

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
            // SAFE to use unstable sort: `map` comes from `Value::Object` (a HashMap) —
            // keys are unique by construction so no two elements compare equal (tie-free).
            ctx.budget
                .mem
                .charge(Part::Temp, bytes(map.len(), size_of::<&String>()))?;
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort_unstable();

            for key in keys {
                ctx.budget.iterations += 1;
                if ctx.budget.iterations > MAX_TOTAL_ITERATIONS {
                    return Err(MdsError::resource_limit(format!(
                    "total loop iterations exceeded maximum of {} across all loops in this compilation",
                    MAX_TOTAL_ITERATIONS
                )));
                }
                let pass = ctx.budget.mem.mark();
                // One charge for the key entry and the value entry of the pass.
                let entries = entry_bytes(key_var, VALUE_ROOT.checked_add(key.len()))
                    .zip(entry_bytes(
                        &block.var,
                        Slot::PROJECTION_BYTES.checked_add(key.len()),
                    ))
                    .and_then(|(key_entry, value_entry)| key_entry.checked_add(value_entry));
                ctx.budget.mem.charge(Part::Temp, entries)?;
                scope.push();
                scope.set_var(key_var, Value::String(key.clone()));
                scope.set_slot(&block.var, slot.project(PathStep::Key(key.clone())));
                let result = body(scope, ctx);
                let result = prefer_first_error(result, scope.pop());
                ctx.budget.mem.release_to(pass);
                result?;
            }
            return Ok(());
        }

        // Standard array iteration: @for item in iterable:
        // Objects are rejected here with a hint to use key-value syntax.
        if let Value::Object(_) = iterable {
            return Err(MdsError::syntax(
                "to iterate over an object's entries, use `@for key, value in obj:` syntax",
            ));
        }

        let array = iterable
            .as_array()
            .ok_or_else(|| MdsError::type_error(iterable.type_name()))?;

        // Check length before any pass creates a projection.
        if array.len() > MAX_LOOP_ITERATIONS {
            return Err(MdsError::resource_limit(format!(
                "array has {} elements, exceeding maximum loop iteration limit of {}",
                array.len(),
                MAX_LOOP_ITERATIONS
            )));
        }

        for i in 0..array.len() {
            ctx.budget.iterations += 1;
            if ctx.budget.iterations > MAX_TOTAL_ITERATIONS {
                return Err(MdsError::resource_limit(format!(
                "total loop iterations exceeded maximum of {} across all loops in this compilation",
                MAX_TOTAL_ITERATIONS
            )));
            }
            let pass = ctx.budget.mem.mark();
            ctx.budget.mem.charge(
                Part::Temp,
                entry_bytes(&block.var, Some(Slot::PROJECTION_BYTES)),
            )?;
            scope.push();
            scope.set_slot(&block.var, slot.project(PathStep::Index(i)));
            let result = body(scope, ctx);
            let result = prefer_first_error(result, scope.pop());
            ctx.budget.mem.release_to(pass);
            result?;
        }
        Ok(())
    })();
    ctx.budget.mem.release_to(mark);
    result
}

fn evaluate_for(
    block: &ForBlock,
    scope: &mut Scope,
    ctx: &mut EvalContext,
    out: &mut String,
) -> Result<(), MdsError> {
    drive_for(block, scope, ctx, |scope, ctx| {
        evaluate_nodes(&block.body, scope, ctx, out)
    })
}

// Warning text is formatted first. Its capacity is at most one allocation
// proportional to the capped alias, plus the fixed diagnostic text.
fn push_warning(ctx: &mut EvalContext, args: fmt::Arguments<'_>) -> Result<(), MdsError> {
    let message = format_exact(format_len(args), args)?;
    ctx.budget
        .mem
        .charge(Part::Buffers, Some(message.capacity()))?;
    ctx.warnings.push(message);
    Ok(())
}

fn evaluate_include(
    inc: &IncludeDirective,
    scope: &Scope,
    ctx: &mut EvalContext,
) -> Result<Option<PromptText>, MdsError> {
    let ns = scope
        .get_namespace(&inc.alias)
        .ok_or_else(|| MdsError::undefined_var(&inc.alias))?;

    let prompt_body = match &ns.prompt_body {
        Some(body) => body,
        None => {
            if ctx.warnings.len() < MAX_WARNINGS {
                // Untrusted identifier from template source — WIRE (spec 7.5).
                let safe_alias = crate::lint::sanitize_control_chars_wire(&inc.alias);
                if ns.prompt_suppressed_by_exports {
                    // WARN-B: module has body text but @export list excludes "prompt".
                    push_warning(
                        ctx,
                        format_args!(
                            "warning: @include of '{safe_alias}' produced empty output \
                         — module has body text, but its explicit @export list does not export \
                         'prompt'; add `@export prompt` to the module, or call its functions \
                         directly: {{{{{safe_alias}.fn()}}}}"
                        ),
                    )?;
                } else {
                    // WARN-A: module has no body text at all.
                    push_warning(
                        ctx,
                        format_args!(
                            "warning: @include of '{safe_alias}' produced empty output \
                         — module has no body text; to use the module's functions, call them \
                         directly: {{{{{safe_alias}.fn()}}}}"
                        ),
                    )?;
                }
            }
            return Ok(None);
        }
    };

    // S6: splice the included module's source-map fragment when a builder is active.
    // `map.cursor` is the absolute byte offset where this include's body starts in
    // the parent output (set by the Node::Include arm in evaluate_nodes just before
    // calling evaluate_include). The builder caches the fragment's source remap, so
    // an @include inside a @for loop registers the module's sources once (AC-PERF-05).
    // While the map is held or suppressed, this text is not mapped here, so the
    // include splices no segment and registers no source; the body is returned as before.
    let map = ctx.map.as_mut().filter(|map| map.suppress == 0);
    if let (Some(fragment), Some(map)) = (ns.prompt_map.as_ref(), map) {
        let base = map.cursor;
        map.splice_fragment(fragment, base);
    }

    Ok(Some(prompt_body.clone()))
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
/// `source` so the error underlines the prose (origin rides along the data, not a
/// path→source lookup). Every node's offsets must therefore index into `source`.
///
/// Append to the caller's `out` and spend from the caller's `budget`.
///
/// Used to collect several node slices that make up ONE module (the spliced regions
/// of an `@extends` chain) under one set of caps. Sharing `out` shares the
/// message-count cap too, which counts `out.len()`. `budget` holds the running totals
/// when this returns, on success and on error alike.
pub(crate) fn evaluate_messages_seeded(
    nodes: &[Node],
    scope: &mut Scope,
    warnings: &mut Vec<String>,
    file: &str,
    source: &str,
    budget: &mut EvalBudget,
    out: &mut Vec<EvalMessage>,
) -> Result<(), MdsError> {
    let mut ctx = EvalContext {
        call_stack: Vec::new(),
        budget: *budget,
        warnings,
        map: None,
        fn_body_owned: false,
        file,
        source,
        body_origin: None,
    };
    let result = collect_messages_strict(nodes, scope, &mut ctx, out, file, source);
    *budget = ctx.budget;
    result
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
                    // Untrusted identifier from template source — WIRE (spec 7.5).
                    push_warning(
                        ctx,
                        format_args!(
                            "warning: @include '{}' inside messages mode is ignored",
                            crate::lint::sanitize_control_chars_wire(&inc.alias)
                        ),
                    )?;
                }
            }
            Node::Block(block) => {
                // Descend into @block body to surface any @message blocks it contains.
                // Reached in standalone mode, where the block body is the default
                // content; an `@extends` chain hands each block's effective body to
                // the collector as its own region instead.
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
    let mark = ctx.budget.mem.mark();
    let result = (|| {
        // Evaluate the role expression to a string.
        let role_slot = evaluate_expr(&block.role, scope, ctx)?;
        let role_value = role_slot.get().ok_or_else(invalid_projection)?;
        let Value::String(role) = role_value else {
            return Err(MdsError::type_error(format!(
                "@message role must evaluate to a string, but got {}",
                role_value.type_name()
            )));
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
        let mut content = String::new();
        if let Err(error) = evaluate_nodes(&block.body, scope, ctx, &mut content) {
            ctx.budget.mem.release_buffers(content.capacity());
            return Err(error);
        }
        trim_body(&mut content, &mut ctx.budget.mem);
        ctx.budget
            .mem
            .transfer(Part::Buffers, Part::Temp, content.capacity());

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
        ctx.budget.message_bytes = ctx.budget.message_bytes.saturating_add(content.len());
        if ctx.budget.message_bytes > MAX_MESSAGES_TOTAL_SIZE {
            return Err(MdsError::resource_limit(format!(
                "total message content exceeds maximum cumulative size of {} bytes",
                MAX_MESSAGES_TOTAL_SIZE
            )));
        }

        // Count the kept role before a copy or move. A computed role stays charged
        // as a temporary until this message mark ends.
        ctx.budget
            .mem
            .charge(Part::Buffers, Some(role.capacity()))?;
        let Some(Value::String(role)) = role_slot.into_value() else {
            return Err(invalid_projection());
        };
        ctx.budget
            .mem
            .transfer(Part::Temp, Part::Buffers, content.capacity());
        out.push(EvalMessage { role, content });
        Ok(())
    })();
    ctx.budget.mem.release_to(mark);
    result
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

    fn test_context(warnings: &mut Vec<String>) -> EvalContext<'_> {
        EvalContext {
            call_stack: Vec::new(),
            budget: EvalBudget::default(),
            warnings,
            map: None,
            fn_body_owned: false,
            file: "",
            source: "",
            body_origin: None,
        }
    }

    fn seed_headroom(ctx: &mut EvalContext<'_>, available: usize) {
        ctx.budget
            .mem
            .charge(
                Part::Buffers,
                Some(crate::limits::MAX_COMPILE_MEMORY - available),
            )
            .unwrap();
    }

    #[test]
    fn join_keeps_its_type_error_when_the_result_would_not_fit() {
        let mut scope = Scope::new();
        scope.set_var(
            "items",
            Value::Array(vec![Value::String("x".repeat(256)), Value::Number(1.0)]),
        );
        let args = [
            Arg::Var("items".into()),
            Arg::StringLiteral("/".repeat(2048)),
        ];
        let mut warnings = Vec::new();
        let mut ctx = test_context(&mut warnings);
        seed_headroom(&mut ctx, 4096);
        let before = ctx.budget.mem;
        let err = call_function("join", &args, &mut scope, &mut ctx, 0).unwrap_err();
        assert_eq!(err.serialize().code, "mds::builtin");
        assert!(err.to_string().contains("found number in array"));
        assert_eq!(ctx.budget.mem, before);
        scope.set_var(
            "items",
            Value::Array(vec![
                Value::String("x".repeat(256)),
                Value::String("ok".into()),
            ]),
        );
        let err = call_function("join", &args, &mut scope, &mut ctx, 0).unwrap_err();
        assert_eq!(
            err.serialize(),
            MdsError::compile_memory_exceeded().serialize()
        );
        assert_eq!(ctx.budget.mem, before);
    }

    #[test]
    fn loop_frames_release_each_key_copy() {
        let mut scope = Scope::new();
        scope.set_var(
            "entries",
            Value::Object(
                (0..MAX_LOOP_ITERATIONS)
                    .map(|i| (i.to_string(), Value::Null))
                    .collect(),
            ),
        );
        let mut block = test_loop(Expr::Var("entries".into()));
        block.key_var = Some("key".into());
        let mut warnings = Vec::new();
        let mut ctx = test_context(&mut warnings);
        seed_headroom(&mut ctx, MAX_LOOP_ITERATIONS * size_of::<&String>() + 512);
        let before = ctx.budget.mem;
        let mut passes = 0;
        drive_for(&block, &mut scope, &mut ctx, |_, _| {
            passes += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(passes, MAX_LOOP_ITERATIONS);
        assert_eq!(ctx.budget.mem, before);
    }

    #[test]
    fn warning_charge_keeps_only_text_that_fits() {
        let mut warnings = Vec::new();
        let mut ctx = test_context(&mut warnings);
        let long = "a".repeat(256);
        seed_headroom(&mut ctx, 128);
        let before = ctx.budget.mem;
        let err = push_warning(&mut ctx, format_args!("warning: {long}")).unwrap_err();
        assert_eq!(
            err.serialize(),
            MdsError::compile_memory_exceeded().serialize()
        );
        assert!(ctx.warnings.is_empty());
        assert_eq!(ctx.budget.mem, before);
        push_warning(&mut ctx, format_args!("warning: a")).unwrap();
        assert_eq!(*ctx.warnings, ["warning: a"]);
    }

    /// Run `run` with one byte less than `need` of headroom, then with exactly `need`.
    /// The short run must refuse with the budget error and leave the budget as it
    /// was, so every scope mark on its path is released on the error path. The exact
    /// run must not release more than it charged. Return the result of the exact run,
    /// and the temporaries and buffers it left charged.
    fn charge_boundary<T>(
        need: usize,
        mut run: impl FnMut(&mut EvalContext<'_>) -> Result<T, MdsError>,
    ) -> (Result<T, MdsError>, [usize; 2]) {
        let mut warnings = Vec::new();
        let mut ctx = test_context(&mut warnings);
        seed_headroom(&mut ctx, need - 1);
        let before = ctx.budget.mem;
        let short = run(&mut ctx).err().map(|error| error.serialize());
        assert_eq!(short, Some(MdsError::compile_memory_exceeded().serialize()));
        assert_eq!(ctx.budget.mem, before, "the refusal must release its marks");
        drop(ctx);
        let mut warnings = Vec::new();
        let mut ctx = test_context(&mut warnings);
        seed_headroom(&mut ctx, need);
        let before = ctx.budget.mem;
        let result = run(&mut ctx);
        // A release of more than the charge keeps the counters, so only the latch shows it.
        assert!(
            !ctx.budget.mem.underflow,
            "the exact run must not underflow a counter"
        );
        let held = [
            ctx.budget.mem.mark() - before.mark(),
            ctx.budget.mem.buffers - before.buffers,
        ];
        (result, held)
    }

    fn interpolation(expr: Expr) -> Node {
        Node::Interpolation(Interpolation {
            expr,
            offset: 0,
            len: 0,
        })
    }

    fn function(params: Vec<Param>, body: Vec<Node>) -> Arc<FunctionDef> {
        Arc::new(FunctionDef {
            params,
            body,
            captured: crate::scope::CapturedScope::default(),
            origin: None,
        })
    }

    #[test]
    fn a_call_frame_entry_charges_its_parameter_name() {
        let name = "p".repeat(4096);
        let func = function(vec![Param::required(name.as_str())], Vec::new());
        // The call key, then the entry: its name and the slot pair.
        let need = "f".len() + name.len() + FRAME_ENTRY;
        let (result, held) = charge_boundary(need, |ctx| {
            invoke_function(&func, "f", &[Value::Null.into()], &mut Scope::new(), ctx)
        });
        assert_eq!(result.unwrap(), "");
        assert_eq!(held, [0, 0]);
    }

    #[test]
    fn a_call_frame_entry_charges_its_default_literal() {
        let default = "d".repeat(4096);
        let param = Param {
            name: "p".into(),
            default: Some(Expr::StringLiteral(default.clone())),
        };
        let func = function(vec![param], Vec::new());
        // The call key, then the entry: its name, the slot pair and the default value.
        let need = "f".len() + "p".len() + FRAME_ENTRY + VALUE_ROOT + default.len();
        let (result, held) = charge_boundary(need, |ctx| {
            invoke_function(&func, "f", &[], &mut Scope::new(), ctx)
        });
        assert_eq!(result.unwrap(), "");
        assert_eq!(held, [0, 0]);
    }

    #[test]
    fn a_loop_frame_entry_charges_its_variable_name() {
        let mut block = test_loop(Expr::Var("items".into()));
        block.var = "v".repeat(4096);
        // One pass entry at a time: its name, the slot pair and the projection.
        let need = block.var.len() + FRAME_ENTRY + Slot::PROJECTION_BYTES;
        let mut passes = 0;
        let (result, held) = charge_boundary(need, |ctx| {
            let mut scope = Scope::new();
            scope.set_var("items", Value::Array(vec![Value::Null; 2]));
            drive_for(&block, &mut scope, ctx, |_, _| {
                passes += 1;
                Ok(())
            })
        });
        result.unwrap();
        assert_eq!(passes, 2);
        assert_eq!(held, [0, 0]);
    }

    #[test]
    fn a_key_value_frame_charges_the_key_and_both_names() {
        let key = "k".repeat(4096);
        let mut block = test_loop(Expr::Var("entries".into()));
        block.var = "v".repeat(1024);
        block.key_var = Some("n".repeat(2048));
        // The sorted key list, then the key entry (with its key copy) and the value
        // entry (with the key in its projection).
        let key_entry = 2048 + FRAME_ENTRY + VALUE_ROOT + key.len();
        let value_entry = block.var.len() + FRAME_ENTRY + Slot::PROJECTION_BYTES + key.len();
        let need = size_of::<&String>() + key_entry + value_entry;
        let (result, held) = charge_boundary(need, |ctx| {
            let mut scope = Scope::new();
            let entries = std::collections::HashMap::from([(key.clone(), Value::Null)]);
            scope.set_var("entries", Value::Object(entries));
            drive_for(&block, &mut scope, ctx, |_, _| Ok(()))
        });
        result.unwrap();
        assert_eq!(held, [0, 0]);
    }

    #[test]
    fn a_shared_builtin_argument_is_charged_the_size_of_its_copy() {
        let items = Value::Array(vec![Value::String("x".repeat(64)); 1000]);
        let copy = value_bytes(&items).unwrap();
        // The argument slot, the value list, the copy and the scalar bound.
        let need = size_of::<Slot>() + NODE + copy + VALUE_ROOT;
        let (result, held) = charge_boundary(need, |ctx| {
            let mut scope = Scope::new();
            scope.set_var("items", items.clone());
            call_function("length", &[Arg::Var("items".into())], &mut scope, ctx, 0)
        });
        assert_eq!(result.unwrap().get(), Some(&Value::Number(1000.0)));
        // Only the scalar result stays charged.
        assert_eq!(held, [VALUE_ROOT, 0]);
    }

    #[test]
    fn a_builtin_argument_held_once_moves_without_a_charge() {
        let body = "t".repeat(4096);
        let func = function(Vec::new(), vec![text(&body)]);
        let call = Arg::Call {
            name: "f".into(),
            args: Vec::new(),
        };
        // The argument slot, the result of `f()` (its value and its buffer), the value
        // list and the scalar bound. A copy of the result would add its bytes again.
        let need = size_of::<Slot>() + VALUE_ROOT + body.len() + NODE + VALUE_ROOT;
        let (result, held) = charge_boundary(need, |ctx| {
            let mut scope = Scope::new();
            scope.set_function("f", Arc::clone(&func));
            call_function("length", std::slice::from_ref(&call), &mut scope, ctx, 0)
        });
        assert_eq!(result.unwrap().get(), Some(&Value::Number(4096.0)));
        // The moved argument is released with the call; only the scalar result stays.
        assert_eq!(held, [VALUE_ROOT, 0]);
    }

    #[test]
    fn a_message_role_is_charged_before_it_is_kept() {
        let role = "r".repeat(4096);
        let block = MessageBlock {
            role: Expr::Var("role".into()),
            body: vec![text("b")],
            offset: 0,
        };
        // The one-byte content, then the kept role.
        let need = 1 + role.len();
        let (result, held) = charge_boundary(need, |ctx| {
            let mut scope = Scope::new();
            scope.set_var("role", Value::String(role.clone()));
            let mut out = Vec::new();
            collect_single_message(&block, &mut scope, ctx, &mut out).map(|()| out)
        });
        let message = EvalMessage {
            role: role.clone(),
            content: "b".into(),
        };
        assert_eq!(result.unwrap(), [message]);
        assert_eq!(held, [0, need]);
    }

    #[test]
    fn an_argument_list_is_charged_before_it_is_built() {
        let node = interpolation(Expr::Call {
            name: "missing".into(),
            args: vec![Arg::Var("x".into()); 64],
        });
        let need = 64 * size_of::<Slot>();
        let (result, held) = charge_boundary(need, |ctx| {
            let mut scope = Scope::new();
            scope.set_var("x", Value::Null);
            evaluate_nodes(
                std::slice::from_ref(&node),
                &mut scope,
                ctx,
                &mut String::new(),
            )
        });
        assert_eq!(
            result.unwrap_err().serialize(),
            MdsError::undefined_fn("missing").serialize()
        );
        assert_eq!(held, [0, 0]);
    }

    #[test]
    fn a_literal_is_charged_before_its_value_is_built() {
        let literal = "l".repeat(4096);
        // An argument literal: the argument slot, then the literal value.
        let node = interpolation(Expr::Call {
            name: "missing".into(),
            args: vec![Arg::StringLiteral(literal.clone())],
        });
        let need = size_of::<Slot>() + VALUE_ROOT + literal.len();
        let (result, held) = charge_boundary(need, |ctx| {
            evaluate_nodes(
                std::slice::from_ref(&node),
                &mut Scope::new(),
                ctx,
                &mut String::new(),
            )
        });
        assert_eq!(
            result.unwrap_err().serialize(),
            MdsError::undefined_fn("missing").serialize()
        );
        assert_eq!(held, [0, 0]);
        // An expression literal in a condition.
        let condition = Condition::Truthy(Expr::StringLiteral(literal.clone()));
        let (result, held) = charge_boundary(VALUE_ROOT + literal.len(), |ctx| {
            evaluate_condition(&condition, &mut Scope::new(), ctx, None)
        });
        assert!(result.unwrap());
        assert_eq!(held, [0, 0]);
    }

    #[test]
    fn a_call_key_is_charged_before_the_call() {
        let name = "f".repeat(4096);
        let func = function(Vec::new(), Vec::new());
        let node = interpolation(Expr::Call {
            name: name.clone(),
            args: Vec::new(),
        });
        // The result value, then the call key.
        let need = VALUE_ROOT + name.len();
        let (result, held) = charge_boundary(need, |ctx| {
            let mut scope = Scope::new();
            scope.set_function(&name, Arc::clone(&func));
            let mut out = String::new();
            evaluate_nodes(std::slice::from_ref(&node), &mut scope, ctx, &mut out).map(|()| out)
        });
        assert_eq!(result.unwrap(), "");
        assert_eq!(held, [0, 0]);
    }

    #[test]
    fn a_qualified_name_is_charged_before_it_is_built() {
        let namespace = "n".repeat(4096);
        let node = interpolation(Expr::QualifiedCall {
            namespace: namespace.clone(),
            name: "f".into(),
            args: Vec::new(),
        });
        let need = namespace.len() + ".f".len();
        let (result, held) = charge_boundary(need, |ctx| {
            evaluate_nodes(
                std::slice::from_ref(&node),
                &mut Scope::new(),
                ctx,
                &mut String::new(),
            )
        });
        assert_eq!(
            result.unwrap_err().serialize(),
            MdsError::undefined_var(&namespace).serialize()
        );
        assert_eq!(held, [0, 0]);
    }

    #[test]
    fn sorted_loop_keys_are_charged_before_they_are_collected() {
        const KEYS: usize = 1000;
        let mut block = test_loop(Expr::Var("entries".into()));
        block.key_var = Some("k".into());
        // The key list, then one pass: its key entry and its value entry, each with
        // a four-byte key.
        let pass = ("k".len() + FRAME_ENTRY + VALUE_ROOT + 4)
            + ("item".len() + FRAME_ENTRY + Slot::PROJECTION_BYTES + 4);
        let need = KEYS * size_of::<&String>() + pass;
        let mut passes = 0;
        let (result, held) = charge_boundary(need, |ctx| {
            let mut scope = Scope::new();
            let entries = (0..KEYS).map(|i| (format!("{i:04}"), Value::Null));
            scope.set_var("entries", Value::Object(entries.collect()));
            drive_for(&block, &mut scope, ctx, |_, _| {
                passes += 1;
                Ok(())
            })
        });
        result.unwrap();
        assert_eq!(passes, KEYS);
        assert_eq!(held, [0, 0]);
    }

    #[test]
    fn a_non_string_interpolation_is_charged_before_it_is_rendered() {
        let value = Value::Array(vec![Value::Number(0.5); 500]);
        let rendered = value.to_string();
        let node = interpolation(Expr::Var("value".into()));
        let (result, held) = charge_boundary(rendered.len(), |ctx| {
            let mut scope = Scope::new();
            scope.set_var("value", value.clone());
            // The output buffer already has room, so only the rendered text is charged.
            let mut out = String::with_capacity(rendered.len());
            evaluate_nodes(std::slice::from_ref(&node), &mut scope, ctx, &mut out).map(|()| out)
        });
        assert_eq!(result.unwrap(), rendered);
        assert_eq!(held, [0, 0]);
    }

    /// Evaluate `@include` of an alias with no prompt in a loop of three passes, with
    /// `available` bytes of headroom. Return the result, the warnings and the bytes
    /// the warnings left charged as buffers.
    fn include_warnings_in_a_loop(
        alias: &str,
        available: usize,
    ) -> (Result<(), MdsError>, Vec<String>, usize) {
        let mut scope = Scope::new();
        scope.set_var("items", Value::Array(vec![Value::Null; 3]));
        scope.set_namespace(
            alias,
            crate::scope::NamespaceScope {
                functions: std::collections::HashMap::new(),
                prompt_body: None,
                prompt_map: None,
                prompt_suppressed_by_exports: false,
            },
        );
        let mut block = test_loop(Expr::Var("items".into()));
        block.body = vec![Node::Include(IncludeDirective {
            alias: alias.into(),
            offset: 0,
        })];
        let mut warnings = Vec::new();
        let mut ctx = test_context(&mut warnings);
        seed_headroom(&mut ctx, available);
        let before = ctx.budget.mem;
        let result = evaluate_nodes(
            &[Node::For(block)],
            &mut scope,
            &mut ctx,
            &mut String::new(),
        );
        assert!(
            !ctx.budget.mem.underflow,
            "the loop must not underflow a counter"
        );
        assert_eq!(ctx.budget.mem.mark(), before.mark());
        let charged = ctx.budget.mem.buffers - before.buffers;
        drop(ctx);
        (result, warnings, charged)
    }

    #[test]
    fn include_warnings_in_a_loop_are_charged_as_they_are_kept() {
        let long = "a".repeat(8192);
        let (result, full, charged) =
            include_warnings_in_a_loop(&long, crate::limits::MAX_COMPILE_MEMORY);
        result.unwrap();
        assert_eq!(full.len(), 3);
        assert_eq!(charged, full.iter().map(String::capacity).sum::<usize>());
        let warning = full[0].capacity();
        assert!(warning > 2 * long.len());
        // Room for three long warnings and one pass entry, less one byte: the third
        // warning is refused, and the two before it stay.
        let pass = "item".len() + FRAME_ENTRY + Slot::PROJECTION_BYTES;
        let available = 3 * warning + pass - 1;
        let (result, kept, charged) = include_warnings_in_a_loop(&long, available);
        assert_eq!(
            result.unwrap_err().serialize(),
            MdsError::compile_memory_exceeded().serialize()
        );
        assert_eq!(kept, full[..2]);
        assert_eq!(charged, 2 * warning);
        // Control: short-alias warnings fit in the same room and keep their text.
        let (result, short, _) = include_warnings_in_a_loop("b", available);
        result.unwrap();
        let text = "warning: @include of 'b' produced empty output — module has no body \
                    text; to use the module's functions, call them directly: {{b.fn()}}";
        assert_eq!(short, [text; 3]);
    }

    fn test_loop(iterable: Expr) -> ForBlock {
        ForBlock {
            var: "item".into(),
            key_var: None,
            iterable,
            body: Vec::new(),
            offset: 0,
            end_offset: 0,
        }
    }

    fn member(object: &str, field: &str) -> Expr {
        Expr::MemberAccess {
            object: object.into(),
            fields: vec![field.into()],
        }
    }

    #[test]
    fn shared_reads_nested_loop_binds_the_original_elements() {
        let items = Arc::new(Value::Array(vec![Value::Array(vec![Value::Null])]));
        let frontmatter = [("items".into(), Arc::clone(&items))].into_iter().collect();
        let mut scope = Scope::with_layers(Arc::default(), Arc::new(frontmatter));
        let outer = test_loop(Expr::Var("items".into()));
        let mut inner = test_loop(Expr::Var("item".into()));
        inner.var = "leaf".into();
        let mut warnings = Vec::new();
        let mut ctx = test_context(&mut warnings);
        let mut shared = Vec::new();
        drive_for(&outer, &mut scope, &mut ctx, |scope, ctx| {
            let item = &scope.get_var("items").unwrap().as_array().unwrap()[0];
            let leaf = std::ptr::from_ref(&item.as_array().unwrap()[0]);
            let root = scope.get_slot("items").unwrap().root();
            let outer_slot = scope.get_slot("item").unwrap();
            assert!(Arc::ptr_eq(root, outer_slot.root()));
            shared.push(std::ptr::eq(outer_slot.get().unwrap(), item));
            drive_for(&inner, scope, ctx, |scope, _| {
                let root = scope.get_slot("items").unwrap().root();
                let leaf_slot = scope.get_slot("leaf").unwrap();
                assert!(Arc::ptr_eq(root, leaf_slot.root()));
                shared.push(std::ptr::eq(leaf_slot.get().unwrap(), leaf));
                Ok(())
            })
        })
        .unwrap();
        assert_eq!(shared, [true, true]);
    }

    #[test]
    fn shared_reads_dot_loop_binds_the_original_elements() {
        let items = Value::Array(vec![Value::Number(1.0), Value::Number(2.0)]);
        let config = Arc::new(Value::Object(std::collections::HashMap::from([(
            "items".into(),
            items,
        )])));
        let frontmatter = [("cfg".into(), Arc::clone(&config))].into_iter().collect();
        let mut scope = Scope::with_layers(Arc::default(), Arc::new(frontmatter));
        let block = test_loop(member("cfg", "items"));
        let Value::Object(fields) = config.as_ref() else {
            panic!("expected object");
        };
        let items = &fields["items"];
        let elements = items.as_array().unwrap();
        let mut warnings = Vec::new();
        let mut ctx = test_context(&mut warnings);
        let iterable = evaluate_expr(&block.iterable, &mut scope, &mut ctx).unwrap();
        assert!(Arc::ptr_eq(iterable.root(), &config));
        let mut shared = vec![std::ptr::eq(iterable.get().unwrap(), items)];
        drive_for(&block, &mut scope, &mut ctx, |scope, _| {
            let slot = scope.get_slot("item").unwrap();
            assert!(Arc::ptr_eq(slot.root(), &config));
            shared.push(std::ptr::eq(
                slot.get().unwrap(),
                &elements[shared.len() - 1],
            ));
            Ok(())
        })
        .unwrap();
        assert_eq!(shared, [true, true, true]);
    }

    #[test]
    fn shared_reads_function_arguments_refer_to_the_caller_values() {
        let mut scope = Scope::new();
        scope.set_var(
            "cfg",
            Value::Object(std::collections::HashMap::from([(
                "item".into(),
                Value::Null,
            )])),
        );
        let args = [
            Arg::Var("cfg".into()),
            Arg::MemberAccess {
                object: "cfg".into(),
                fields: vec!["item".into()],
            },
        ];
        let resolved =
            resolve_args(&args, &mut scope, &mut test_context(&mut Vec::new()), 0).unwrap();
        scope.push();
        bind_call_params(
            &[Param::required("whole"), Param::required("part")],
            &resolved,
            &mut scope,
            &mut MemBudget::default(),
        )
        .unwrap();
        let config = scope.get_slot("cfg").unwrap();
        let Value::Object(fields) = config.get().unwrap() else {
            panic!("expected object");
        };
        let whole = scope.get_slot("whole").unwrap();
        let part = scope.get_slot("part").unwrap();
        assert!(Arc::ptr_eq(whole.root(), config.root()));
        assert!(Arc::ptr_eq(part.root(), config.root()));
        assert!(std::ptr::eq(part.get().unwrap(), &fields["item"]));
    }

    #[test]
    fn shared_reads_key_value_loop_shares_values_in_key_order() {
        let entries = std::collections::HashMap::from([
            ("z".into(), Value::Number(2.0)),
            ("a".into(), Value::Number(1.0)),
        ]);
        let mut scope = Scope::new();
        scope.set_var("entries", Value::Object(entries));
        let mut block = test_loop(Expr::Var("entries".into()));
        block.key_var = Some("key".into());
        let mut seen = Vec::new();
        drive_for(
            &block,
            &mut scope,
            &mut test_context(&mut Vec::new()),
            |scope, _| {
                let key = scope.get_var("key").unwrap().to_string();
                let Value::Object(entries) = scope.get_var("entries").unwrap() else {
                    panic!("expected object");
                };
                let shared = std::ptr::eq(scope.get_var("item").unwrap(), &entries[&key]);
                seen.push((key, shared));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(seen, [(String::from("a"), true), (String::from("z"), true)]);
    }

    #[test]
    fn shared_reads_broken_projection_is_undefined_not_null() {
        let root = Slot::from(Value::Array(vec![Value::Null]));
        let mut scope = Scope::new();
        scope.set_slot("broken", root.project(PathStep::Index(1)));
        scope.set_slot("valid", root.project(PathStep::Index(0)));
        let mut warnings = Vec::new();
        let mut ctx = test_context(&mut warnings);
        assert_eq!(scope.get_var("broken"), None);
        let expected = MdsError::undefined_var("broken").to_string();
        for expr in [Expr::Var("broken".into()), member("broken", "field")] {
            assert_eq!(
                evaluate_expr(&expr, &mut scope, &mut ctx)
                    .and_then(|slot| render_expr(&expr, &slot, &mut ctx.budget.mem).map(drop))
                    .unwrap_err()
                    .to_string(),
                expected
            );
        }
        assert_eq!(
            resolve_args(&[Arg::Var("broken".into())], &mut scope, &mut ctx, 0)
                .unwrap_err()
                .to_string(),
            expected
        );
        assert_eq!(scope.get_var("valid"), Some(&Value::Null));
    }

    #[test]
    fn shared_reads_large_array_refused_before_first_pass() {
        let mut scope = Scope::new();
        scope.set_var(
            "items",
            Value::Array(vec![Value::Null; MAX_LOOP_ITERATIONS + 1]),
        );
        let block = test_loop(Expr::Var("items".into()));
        let mut passes = 0;
        let err = drive_for(
            &block,
            &mut scope,
            &mut test_context(&mut Vec::new()),
            |_, _| {
                passes += 1;
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(passes, 0);
        assert_eq!(
            err.to_string(),
            format!("resource limit exceeded: array has {} elements, exceeding maximum loop iteration limit of {MAX_LOOP_ITERATIONS}", MAX_LOOP_ITERATIONS + 1),
        );
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
        // Build a node list whose output crosses MAX_OUTPUT_SIZE (50 MiB) across two
        // nodes, rather than allocating a single 50 MiB+ string in one shot.
        // Each node is ~26 MB: the first is appended, and the second node's append
        // would pass the limit, so evaluate_nodes refuses it before it lands and
        // returns a ResourceLimit error.
        //
        // Two nodes of ~26 MB each keep every single string under the limit, which
        // is safer for CI environments than a single 50 MiB+ pre-allocated string.
        let half = MAX_OUTPUT_SIZE / 2 + 1;
        let chunk = "x".repeat(half);
        let nodes = vec![text(&chunk), text(&chunk)];
        let mut scope = Scope::new();
        let mut warnings = vec![];
        let result = evaluate(&nodes, &mut scope, &mut warnings, "", "");
        assert_output_cap_error(result, "two text nodes past MAX_OUTPUT_SIZE");
    }

    #[test]
    fn output_cap_precedes_a_later_error_in_an_if_body() {
        let nodes = vec![
            text(&"x".repeat(MAX_OUTPUT_SIZE - 1)),
            Node::If(IfBlock {
                condition: Condition::Truthy(Expr::Var("flag".into())),
                elseif_branches: vec![],
                then_body: vec![
                    text("yz"),
                    Node::Interpolation(Interpolation {
                        expr: Expr::Var("unknown".into()),
                        offset: 0,
                        len: 0,
                    }),
                ],
                else_body: None,
                offset: 0,
                else_offset: None,
                end_offset: 0,
            }),
        ];
        let mut scope = Scope::new();
        scope.set_var("flag", Value::Boolean(true));
        let result = evaluate(&nodes, &mut scope, &mut Vec::new(), "", "");
        assert_output_cap_error(result, "the first child append crosses the cap");
    }

    // ── #415: the output cap stops a loop at the pass that crosses it ─────────
    //
    // The error text is the same whether the cap is checked before each append or
    // once a whole node has finished, so these tests assert the axis the check moves
    // (applies PF-013): how many loop passes ran before evaluation stopped, read from
    // the budget the evaluator hands back on error.

    /// One loop pass renders `{{x}}\n`: `x` is 1 MiB, plus the newline.
    const CAP_PASS: usize = 1024 * 1024 + 1;

    /// The pass whose append would take a loop's buffer past `MAX_OUTPUT_SIZE`:
    /// 49 passes fit (51,380,273 bytes), the 50th does not (52,428,850).
    const CAP_CROSSING_PASS: usize = MAX_OUTPUT_SIZE / CAP_PASS + 1;

    /// Loop length: twice the crossing pass, so a check made only once the loop has
    /// finished lets all 100 passes run.
    const CAP_LOOP_LEN: usize = 2 * CAP_CROSSING_PASS;

    const _: () = assert!((CAP_CROSSING_PASS - 1) * CAP_PASS <= MAX_OUTPUT_SIZE);
    const _: () = assert!(CAP_CROSSING_PASS * CAP_PASS > MAX_OUTPUT_SIZE);

    /// The error of `result`. An `Ok` is reported by length only: the output runs to
    /// tens of megabytes.
    fn err_of(result: Result<String, MdsError>, context: &str) -> MdsError {
        match result {
            Ok(out) => panic!(
                "{context}: evaluated {} bytes, expected an error",
                out.len()
            ),
            Err(err) => err,
        }
    }

    /// The exact output-cap error, as `Display` renders it.
    fn output_cap_message() -> String {
        format!("resource limit exceeded: output exceeds maximum size of {MAX_OUTPUT_SIZE} bytes")
    }

    /// Assert `result` is exactly the output-cap `mds::resource_limit` error.
    fn assert_output_cap_error(result: Result<String, MdsError>, context: &str) {
        let err = err_of(result, context);
        assert!(
            matches!(err, MdsError::ResourceLimit { .. }),
            "{context}: expected mds::resource_limit, got: {err}"
        );
        assert_eq!(err.to_string(), output_cap_message(), "{context}");
    }

    /// Parse `src` and register its top-level `@define`s in `scope`, as the resolver
    /// does before it evaluates a module body.
    fn parse_into(src: &str, scope: &mut Scope) -> Vec<Node> {
        let tokens = crate::lexer::tokenize(src, "").expect("fixture must tokenize");
        let module = crate::parser::parse_with_ctx(&tokens, "", "").expect("fixture must parse");
        for node in &module.body {
            if let Node::Define(def) = node {
                scope.set_function(&def.name, Arc::new(FunctionDef::from(def)));
            }
        }
        module.body
    }

    /// Evaluate `src` from `seed` with `x` (1 MiB), `items` (`CAP_LOOP_LEN` numbers)
    /// and `one` (a single number) in scope; return the result and the budget spent.
    fn eval_capped(src: &str, seed: EvalBudget) -> (Result<String, MdsError>, EvalBudget) {
        let numbers = |n: usize| Value::Array((0..n).map(|i| Value::Number(i as f64)).collect());
        let mut scope = Scope::new();
        scope.set_var("x", Value::String("a".repeat(CAP_PASS - 1)));
        scope.set_var("items", numbers(CAP_LOOP_LEN));
        scope.set_var("one", numbers(1));
        let nodes = parse_into(src, &mut scope);
        let mut budget = seed;
        let mut warnings = vec![];
        let mut out = String::new();
        let result = evaluate_seeded(
            &nodes,
            &mut scope,
            &mut warnings,
            "",
            "",
            &mut budget,
            &mut out,
        );
        (result.map(|()| out), budget)
    }

    /// #415: a `@for` whose output crosses the cap stops on the crossing pass.
    /// Checked only once the loop had finished, all `CAP_LOOP_LEN` passes ran first.
    #[test]
    fn for_loop_output_cap_stops_iterating() {
        let (result, budget) =
            eval_capped("@for i in items:\n{{x}}\n@end\n", EvalBudget::default());
        assert_output_cap_error(result, "a loop crossing the output cap");
        assert_eq!(
            budget.iterations, CAP_CROSSING_PASS,
            "the loop must stop on the pass that crosses the cap, not run all {CAP_LOOP_LEN}"
        );
    }

    /// #415: every container that holds a crossing loop reports the same
    /// error, and the loop stops on the crossing pass in each of them.
    #[test]
    fn for_loop_output_cap_stops_iterating_in_every_container() {
        // (container, source, loop passes run when the crossing pass is reached)
        let cases = [
            (
                // The outer loop's buffer crosses: each outer pass runs one inner pass.
                "nested @for",
                "@for o in items:\n@for i in one:\n{{x}}\n@end\n@end\n",
                2 * CAP_CROSSING_PASS,
            ),
            (
                "@for in @define",
                "@define f():\n@for i in items:\n{{x}}\n@end\n@end\n{{f()}}\n",
                CAP_CROSSING_PASS,
            ),
            (
                "@for in @block",
                "@block b:\n@for i in items:\n{{x}}\n@end\n@end\n",
                CAP_CROSSING_PASS,
            ),
            (
                "@for in a text-mode @message",
                "@message user:\n@for i in items:\n{{x}}\n@end\n@end\n",
                CAP_CROSSING_PASS,
            ),
        ];
        // Every container runs before anything is asserted, so a failure reports them all.
        let expected: Vec<(&str, String, usize)> = cases
            .iter()
            .map(|&(container, _, passes)| (container, output_cap_message(), passes))
            .collect();
        let actual: Vec<(&str, String, usize)> = cases
            .iter()
            .map(|&(container, src, _)| {
                let (result, budget) = eval_capped(src, EvalBudget::default());
                let err = err_of(result, container);
                assert!(
                    matches!(err, MdsError::ResourceLimit { .. }),
                    "{container}: expected mds::resource_limit, got: {err}"
                );
                (container, err.to_string(), budget.iterations)
            })
            .collect();
        assert_eq!(
            actual, expected,
            "each container must report the output cap and stop its loop on the crossing pass \
             (container, error, loop passes run)"
        );
    }

    /// #415: with exactly `CAP_CROSSING_PASS` passes left in the budget, the
    /// output cap stops the loop on its last allowed pass; with one fewer, the
    /// iteration cap stops it first — on the same pass, before its body runs.
    #[test]
    fn for_loop_iteration_cap_still_wins_on_the_crossing_pass() {
        let src = "@for i in items:\n{{x}}\n@end\n";
        let seeded = |left: usize| EvalBudget {
            iterations: MAX_TOTAL_ITERATIONS - left,
            ..EvalBudget::default()
        };

        let (result, budget) = eval_capped(src, seeded(CAP_CROSSING_PASS));
        assert_output_cap_error(
            result,
            "the crossing pass is the last one the budget allows",
        );
        assert_eq!(budget.iterations, MAX_TOTAL_ITERATIONS);

        let (result, budget) = eval_capped(src, seeded(CAP_CROSSING_PASS - 1));
        let err = err_of(result, "the iteration cap must fire on the crossing pass");
        assert_eq!(
            err.to_string(),
            format!(
                "resource limit exceeded: total loop iterations exceeded maximum of \
                 {MAX_TOTAL_ITERATIONS} across all loops in this compilation"
            )
        );
        assert_eq!(budget.iterations, MAX_TOTAL_ITERATIONS + 1);
    }

    /// #415: a buffer grown through `push_capped` never reserves past the cap.
    /// Plain doubling from 32 MiB would reserve 64 MiB for a 33 MiB buffer.
    #[test]
    fn push_capped_growth_never_reserves_past_the_cap() {
        let mut out = String::new();
        let mut mem = MemBudget::default();
        let chunk = "a".repeat(1024 * 1024);
        for pass in 1..=MAX_OUTPUT_SIZE / chunk.len() {
            push_capped(&mut out, &chunk, &mut mem).expect("appends up to the cap must succeed");
            assert!(
                out.capacity() >= out.len() && out.capacity() <= MAX_OUTPUT_SIZE,
                "pass {pass}: capacity {} must lie between the length {} and the cap",
                out.capacity(),
                out.len()
            );
        }
        // Positive control: the buffer reached exactly the cap, so the bound above was
        // tested where doubling would have overshot it.
        assert_eq!(out.len(), MAX_OUTPUT_SIZE);
    }

    /// #415: at the cap, `push_capped` accepts an empty append and refuses one
    /// more byte with the output-cap error, leaving the buffer unchanged.
    #[test]
    fn push_capped_refuses_past_the_cap_and_leaves_the_buffer_unchanged() {
        let mut out = String::new();
        let mut mem = MemBudget::default();
        push_capped(&mut out, &"a".repeat(MAX_OUTPUT_SIZE - 1), &mut mem).expect("under the cap");
        push_capped(&mut out, "b", &mut mem).expect("exactly the cap is allowed");
        push_capped(&mut out, "", &mut mem).expect("an empty append at the cap is allowed");
        assert_eq!(out.len(), MAX_OUTPUT_SIZE);

        let err = push_capped(&mut out, "c", &mut mem).expect_err("one byte past the cap");
        assert_eq!(err.to_string(), output_cap_message());
        assert!(
            out.len() == MAX_OUTPUT_SIZE && out.ends_with('b'),
            "a refused append must leave the buffer unchanged"
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

    fn arity_scope() -> Scope {
        let mut scope = Scope::new();
        let function = Arc::new(FunctionDef {
            params: vec![Param::required("x")],
            body: vec![text("ok")],
            captured: crate::scope::CapturedScope::default(),
            origin: None,
        });
        scope.set_function("f", Arc::clone(&function));
        scope.set_namespace(
            "ns",
            Arc::new(crate::scope::NamespaceScope {
                functions: [("f".into(), function)].into_iter().collect(),
                prompt_body: None,
                prompt_map: None,
                prompt_suppressed_by_exports: false,
            }),
        );
        scope
    }

    fn direct_call(namespace: Option<&str>, name: &str, args: Vec<Arg>) -> Expr {
        match namespace {
            Some(namespace) => Expr::QualifiedCall {
                namespace: namespace.into(),
                name: name.into(),
                args,
            },
            None => Expr::Call {
                name: name.into(),
                args,
            },
        }
    }

    #[test]
    fn known_callee_reports_arity_before_an_undefined_argument() {
        let mut scope = arity_scope();
        let mut warnings = Vec::new();
        let mut ctx = test_context(&mut warnings);
        let missing = Arg::Var("missing_arg".into());
        let wrong = vec![missing.clone(), Arg::NullLiteral];
        let nested = direct_call(
            None,
            "upper",
            vec![Arg::Call {
                name: "f".into(),
                args: wrong.clone(),
            }],
        );
        let calls = [
            (direct_call(None, "f", wrong.clone()), "f", Some("f(x)")),
            (
                direct_call(Some("ns"), "f", wrong.clone()),
                "ns.f",
                Some("ns.f(x)"),
            ),
            (direct_call(None, "upper", wrong), "upper", None),
            (nested, "f", Some("f(x)")),
        ];
        let mismatches: Vec<_> = calls
            .into_iter()
            .filter_map(|(expr, name, signature)| {
                let actual = evaluate_expr(&expr, &mut scope, &mut ctx).unwrap_err();
                let expected = MdsError::arity(name, 1, 1, 2, signature.map(str::to_owned));
                let actual = format!("{actual:?}");
                let expected = format!("{expected:?}");
                (actual != expected).then_some((actual, expected))
            })
            .collect();
        assert!(
            mismatches.is_empty(),
            "actual and expected errors: {mismatches:?}"
        );
        for (namespace, name) in [(None, "f"), (Some("ns"), "f"), (None, "upper")] {
            let expr = direct_call(namespace, name, vec![missing.clone()]);
            let err = evaluate_expr(&expr, &mut scope, &mut ctx).unwrap_err();
            assert_eq!(
                err.to_string(),
                MdsError::undefined_var("missing_arg").to_string()
            );
        }
    }

    #[test]
    fn missing_callee_reports_argument_error_before_lookup_error() {
        let mut scope = arity_scope();
        let mut warnings = Vec::new();
        let mut ctx = test_context(&mut warnings);
        for (namespace, name, expected) in [
            (None, "gone", MdsError::undefined_fn("gone")),
            (Some("gone"), "f", MdsError::undefined_var("gone")),
            (Some("ns"), "gone", MdsError::undefined_fn("ns.gone")),
        ] {
            let expr = direct_call(namespace, name, vec![Arg::Var("missing_arg".into())]);
            let err = evaluate_expr(&expr, &mut scope, &mut ctx).unwrap_err();
            assert_eq!(
                err.to_string(),
                MdsError::undefined_var("missing_arg").to_string()
            );
            let expr = direct_call(namespace, name, vec![Arg::NullLiteral]);
            let err = evaluate_expr(&expr, &mut scope, &mut ctx).unwrap_err();
            assert_eq!(err.to_string(), expected.to_string());
        }
    }

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

        // Call with 0 args → arity error before the function body runs.
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
    fn call_frame_pops_when_a_parameter_default_fails() {
        use crate::ast::Param;
        use crate::scope::CapturedScope;

        let mut scope = Scope::new();
        let function = Arc::new(FunctionDef {
            params: vec![
                Param::required("local"),
                Param {
                    name: "bad_default".into(),
                    default: Some(Expr::Var("missing".into())),
                },
            ],
            body: vec![],
            captured: CapturedScope::default(),
            origin: None,
        });
        scope.set_function("f", function);
        let nodes = vec![Node::Interpolation(crate::ast::Interpolation {
            expr: Expr::Call {
                name: "f".into(),
                args: vec![Arg::NumberLiteral(1.0)],
            },
            offset: 0,
            len: 0,
        })];

        let error = evaluate(&nodes, &mut scope, &mut vec![], "", "").unwrap_err();
        assert!(error.to_string().contains("non-literal expression"));
        assert!(scope.get_var("local").is_none());
        assert!(scope.pop().is_err());
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

    // ── #220: the source-map cursor invariant, checked from outside ───────────

    /// The cursor invariant the record points assert internally is also an observable
    /// output property: when `evaluate_with_map` returns, `cursor` is the compiled
    /// output length, and every recorded segment points inside that output.
    ///
    /// Exercised across all three recording node kinds (Text, EscapedBrace,
    /// Interpolation) and through the recursive arms (`@if`, `@for`, `@message`,
    /// `@block`) that each run their own `evaluate_nodes` invocation with its own
    /// local output buffer.
    #[test]
    fn evaluate_with_map_cursor_tracks_output_length_across_node_kinds() {
        let source = concat!(
            "Hello {{name}}!\n",
            "Escaped: \\{{ raw\n",
            "@if flag:\n",
            "yes {{name}}\n",
            "@end\n",
            "@for i in items:\n",
            "- {{i}}\n",
            "@end\n",
            "@message user:\n",
            "ask {{name}}\n",
            "@end\n",
            "@block extra:\n",
            "inside {{name}}\n",
            "@end\n",
        );
        let tokens = crate::lexer::tokenize(source, "t.mds").expect("fixture must tokenize");
        let module =
            crate::parser::parse_with_ctx(&tokens, "t.mds", source).expect("fixture must parse");

        let mut scope = Scope::new();
        scope.set_var("name", Value::String("World".to_string()));
        scope.set_var("flag", Value::Boolean(true));
        scope.set_var(
            "items",
            Value::Array(vec![
                Value::String("a".to_string()),
                Value::String("b".to_string()),
            ]),
        );

        let builder = crate::sourcemap::MapBuilder::from_parts(
            "t.mds".to_string(),
            "t.mds".to_string(),
            source.to_string(),
        );
        let mut warnings = vec![];
        let (output, map) = evaluate_with_map(&module.body, &mut scope, &mut warnings, builder)
            .expect("fixture must evaluate");

        assert_eq!(
            map.cursor,
            output.len() as u32,
            "the cursor must equal the compiled output length when evaluation returns"
        );
        assert!(
            map.segments.len() >= 12,
            "non-vacuity: the fixture must reach many record points; got {} segments",
            map.segments.len()
        );

        let mut prev_out = 0u32;
        for (i, seg) in map.segments.iter().enumerate() {
            assert!(
                seg.out >= prev_out,
                "segment {i} output offset went backwards: {} < {prev_out}",
                seg.out
            );
            assert!(
                seg.out as usize <= output.len(),
                "segment {i} output offset {} is past the end of the compiled output ({})",
                seg.out,
                output.len()
            );
            prev_out = seg.out;
        }

        // The escaped brace is the record point with a fixed source width: three source
        // bytes (backslash + two braces) collapsing to a two-byte output.
        let escape_off = u32::try_from(
            source
                .find("\\{{")
                .expect("fixture must contain an escaped brace"),
        )
        .expect("fixture offset fits in u32");
        assert!(
            map.segments
                .iter()
                .any(|s| s.src_off == escape_off && s.len == 3),
            "expected an EscapedBrace segment at source offset {escape_off} with len 3; \
             got: {:?}",
            map.segments
        );
    }

    // ── #114: one budget threaded through seeded evaluations ──────────────────
    //
    // An `@extends` chain is evaluated one spliced region at a time, and every region
    // must spend from the same budget. Each test runs two seeded evaluations over one
    // `EvalBudget`: the first spends it to exactly the cap (proving the seed is honored
    // with no off-by-one), the second trips on its very first unit of work.

    const ITERATION_LIMIT_MSG: &str = "total loop iterations exceeded maximum of 1000000";
    const MESSAGE_BYTES_LIMIT_MSG: &str =
        "total message content exceeds maximum cumulative size of 52428800 bytes";

    /// Parse `source` into the module body the evaluator consumes.
    fn parse_body(source: &str) -> Vec<Node> {
        let tokens = crate::lexer::tokenize(source, "t.mds").expect("fixture must tokenize");
        crate::parser::parse_with_ctx(&tokens, "t.mds", source)
            .expect("fixture must parse")
            .body
    }

    /// A scope whose `items` array has one element: a loop over it is one iteration.
    fn one_item_scope() -> Scope {
        let mut scope = Scope::new();
        scope.set_var("items", Value::Array(vec![Value::Number(1.0)]));
        scope
    }

    fn assert_limit<T>(result: Result<T, MdsError>, expected: &str) {
        let Err(err) = result else {
            panic!("a spent budget must trip on the first unit of work, but it evaluated");
        };
        assert!(
            matches!(err, MdsError::ResourceLimit { .. }),
            "expected mds::resource_limit, got: {err}"
        );
        assert!(
            err.to_string().contains(expected),
            "expected {expected:?}, got: {err}"
        );
    }

    #[test]
    fn evaluate_seeded_threads_the_iteration_budget_across_calls() {
        let body = parse_body("@for i in items:\n.\n@end\n");
        let mut warnings = vec![];

        // Control: a zero seed runs the loop and records the one iteration it spent.
        let mut budget = EvalBudget::default();
        let mut out = String::new();
        evaluate_seeded(
            &body,
            &mut one_item_scope(),
            &mut warnings,
            "",
            "",
            &mut budget,
            &mut out,
        )
        .expect("a fresh budget must admit one iteration");
        assert_eq!(out, ".\n");
        assert_eq!(budget.iterations, 1);

        // One iteration short of the cap: the loop runs and spends the budget exactly.
        let mut budget = EvalBudget {
            iterations: MAX_TOTAL_ITERATIONS - 1,
            ..EvalBudget::default()
        };
        evaluate_seeded(
            &body,
            &mut one_item_scope(),
            &mut warnings,
            "",
            "",
            &mut budget,
            &mut String::new(),
        )
        .expect("the last iteration under the cap must run");
        assert_eq!(budget.iterations, MAX_TOTAL_ITERATIONS);

        // The same budget, carried into the next evaluation, trips on its first iteration.
        assert_limit(
            evaluate_seeded(
                &body,
                &mut one_item_scope(),
                &mut warnings,
                "",
                "",
                &mut budget,
                &mut String::new(),
            ),
            ITERATION_LIMIT_MSG,
        );
    }

    #[test]
    fn evaluate_with_map_seeded_threads_the_iteration_budget_across_calls() {
        let source = "@for i in items:\n.\n@end\n";
        let body = parse_body(source);
        let builder = || {
            crate::sourcemap::MapBuilder::from_parts(
                "t.mds".to_string(),
                "t.mds".to_string(),
                source.to_string(),
            )
        };
        let mut warnings = vec![];

        // Control: a zero seed runs the loop, records it, and counts one iteration.
        let mut budget = EvalBudget::default();
        let mut out = String::new();
        let map = evaluate_with_map_seeded(
            &body,
            &mut one_item_scope(),
            &mut warnings,
            builder(),
            &mut budget,
            &mut out,
        )
        .expect("a fresh budget must admit one iteration");
        assert_eq!(out, ".\n");
        assert_eq!(map.cursor, 2, "the cursor tracks the recorded output");
        assert_eq!(budget.iterations, 1);

        let mut budget = EvalBudget {
            iterations: MAX_TOTAL_ITERATIONS - 1,
            ..EvalBudget::default()
        };
        evaluate_with_map_seeded(
            &body,
            &mut one_item_scope(),
            &mut warnings,
            builder(),
            &mut budget,
            &mut String::new(),
        )
        .expect("the last iteration under the cap must run");
        assert_eq!(budget.iterations, MAX_TOTAL_ITERATIONS);

        assert_limit(
            evaluate_with_map_seeded(
                &body,
                &mut one_item_scope(),
                &mut warnings,
                builder(),
                &mut budget,
                &mut String::new(),
            ),
            ITERATION_LIMIT_MSG,
        );
    }

    // ── #416: the builder shares module sources and caches splice remaps ───────

    fn origin(file: &str, display: &str, source: &str) -> crate::sourcemap::Origin {
        crate::sourcemap::Origin {
            file: Arc::from(file),
            display: Arc::from(display),
            source: Arc::from(source),
        }
    }

    /// The namespace an importer sees for `module`, whose prompt was evaluated by its
    /// own builder: one segment of its own text, then one of `helper`'s — the file an
    /// S8 function call pulled in.
    fn included_namespace(
        module: &crate::sourcemap::Origin,
        helper: &crate::sourcemap::Origin,
    ) -> crate::scope::NamespaceScope {
        let mut b = crate::sourcemap::MapBuilder::new(module.clone());
        b.push_segment(0, 0, 4);
        b.switch_to(helper);
        b.push_segment(5, 0, 4);
        crate::scope::NamespaceScope {
            functions: std::collections::HashMap::new(),
            prompt_body: Some("mmmm hhhh\n".to_owned().into()),
            prompt_map: Some(Arc::new(b.into_fragment())),
            prompt_suppressed_by_exports: false,
        }
    }

    /// #416: K `@include`s of one module — separate sites and a loop —
    /// splice through one remap-cache entry in the builder, and every registered
    /// source is the `Origin` that registered it, sharing its `Arc`s.
    #[test]
    fn map_builder_remap_cache_holds_one_entry_for_k_includes_of_one_module() {
        let main_src = "@include m\n@for i in items:\n@include m\n@end\n@include m\n";
        let main = origin("/p/main.mds", "main.mds", main_src);
        let module = origin("/p/m.mds", "m.mds", "m text");
        let helper = origin("/p/h.mds", "h.mds", "h text");
        let mut scope = Scope::new();
        scope.set_var("items", Value::Array(vec![Value::Number(1.0); 3]));
        scope.set_namespace("m", included_namespace(&module, &helper));

        let (output, map) = evaluate_with_map(
            &parse_body(main_src),
            &mut scope,
            &mut vec![],
            crate::sourcemap::MapBuilder::new(main.clone()),
        )
        .expect("fixture must evaluate");

        assert_eq!(
            output.matches("mmmm hhhh").count(),
            5,
            "non-vacuity: two include sites plus three loop passes"
        );
        assert_eq!(
            map.remap_cache_len(),
            1,
            "one entry for one included module"
        );
        assert_eq!(
            map.segments.iter().filter(|s| s.src != 0).count(),
            10,
            "every include splices both fragment segments"
        );
        let expected = [&main, &module, &helper];
        assert_eq!(map.sources().len(), expected.len());
        for (idx, (entry, want)) in map.sources().iter().zip(expected).enumerate() {
            assert!(
                Arc::ptr_eq(&entry.source, &want.source),
                "entry {idx}: source must be the module's own Arc"
            );
            assert!(
                Arc::ptr_eq(&entry.display, &want.display),
                "entry {idx}: display must be the module's own Arc"
            );
        }
    }

    /// PF-005: a builder whose current source names no registered source is an internal
    /// error, not an evaluation with an empty file context. Only an index another
    /// builder handed out can do that: every index a builder hands out is its own.
    #[test]
    fn evaluate_with_map_seeded_rejects_an_unregistered_current_src() {
        let body = parse_body("text\n");
        let mut builder = crate::sourcemap::MapBuilder::new(origin("t.mds", "t.mds", "text\n"));
        // Control: the seed's index evaluates.
        let (out, returned) = evaluate_with_map(&body, &mut Scope::new(), &mut vec![], builder)
            .expect("the seed must evaluate");
        assert_eq!(out, "text\n");
        builder = returned;
        let mut other = crate::sourcemap::MapBuilder::new(origin("a.mds", "a.mds", "a"));
        let foreign = other.register(&origin("b.mds", "b.mds", "b"));
        builder.switch_to_index(foreign);
        let Err(err) = evaluate_with_map(&body, &mut Scope::new(), &mut vec![], builder) else {
            panic!("an unregistered current_src must not evaluate");
        };
        assert_eq!(
            err.to_string(),
            "syntax error: internal: MapBuilder current_src names no registered source — \
             compiler bug"
        );
    }

    #[test]
    fn evaluate_messages_seeded_threads_budgets_across_calls() {
        let body = parse_body("@for i in items:\n@message user:\nhi\n@end\n@end\n");
        let mut warnings = vec![];

        // Control: a zero seed collects the message and records what it spent.
        let mut budget = EvalBudget::default();
        let mut out = Vec::new();
        evaluate_messages_seeded(
            &body,
            &mut one_item_scope(),
            &mut warnings,
            "",
            "",
            &mut budget,
            &mut out,
        )
        .expect("a fresh budget must admit one message");
        assert_eq!(
            out,
            [EvalMessage {
                role: "user".to_string(),
                content: "hi".to_string()
            }]
        );
        let mut mem = MemBudget::default();
        mem.charge(
            Part::Buffers,
            Some(out[0].content.capacity() + out[0].role.capacity()),
        )
        .unwrap();
        assert_eq!(
            budget,
            EvalBudget {
                iterations: 1,
                message_bytes: 2,
                mem,
                ..EvalBudget::default()
            }
        );

        // Message bytes: two bytes short of the cap admits "hi" and spends it exactly;
        // the same budget then trips on the next message's first byte.
        let mut budget = EvalBudget {
            message_bytes: MAX_MESSAGES_TOTAL_SIZE - 2,
            ..EvalBudget::default()
        };
        let mut out = Vec::new();
        evaluate_messages_seeded(
            &body,
            &mut one_item_scope(),
            &mut warnings,
            "",
            "",
            &mut budget,
            &mut out,
        )
        .expect("the last message under the cap must be collected");
        assert_eq!(budget.message_bytes, MAX_MESSAGES_TOTAL_SIZE);
        let spent = budget.mem;
        assert_limit(
            evaluate_messages_seeded(
                &body,
                &mut one_item_scope(),
                &mut warnings,
                "",
                "",
                &mut budget,
                &mut out,
            ),
            MESSAGE_BYTES_LIMIT_MSG,
        );
        assert_eq!(out.len(), 1, "the tripping message is never collected");
        assert_eq!(
            budget.mem, spent,
            "the refused message releases its capacity"
        );

        // Iterations: the messages path spends from the same iteration budget.
        let mut budget = EvalBudget {
            iterations: MAX_TOTAL_ITERATIONS,
            ..EvalBudget::default()
        };
        assert_limit(
            evaluate_messages_seeded(
                &body,
                &mut one_item_scope(),
                &mut warnings,
                "",
                "",
                &mut budget,
                &mut Vec::new(),
            ),
            ITERATION_LIMIT_MSG,
        );
    }
}
