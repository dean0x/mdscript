//! Held regions of the source map: a condition, an iterable or a call argument.
//!
//! Such a region gives a value, but its text is not output. While the region is held,
//! the source map records nothing: a call takes the suppressed path, so it switches to
//! no source and registers none. The release puts back the cursor and the segment
//! count, so the cursor stays one global output offset.
//!
//! These helpers are in their own module on purpose. rustc puts the code of a module
//! in one codegen unit. When the helpers were in the `evaluator` module, that unit
//! grew and rustc grouped the units of the crate in a different way, so LLVM inlined
//! other code differently. In the WASM build, that cost about 1 KB more than the
//! helpers themselves.

use super::{call_function, evaluate_expr, EvalContext};
use crate::ast::{Arg, Expr};
use crate::error::MdsError;
use crate::scope::{Scope, Slot};

/// Start a held region. `None` when no map is recorded.
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

/// Evaluate an expression whose value is used but whose text is not output: an operand
/// of a condition or the iterable of a loop. The map is held for the whole evaluation.
#[inline(never)]
pub(super) fn evaluate_held(
    expr: &Expr,
    scope: &mut Scope,
    ctx: &mut EvalContext,
) -> Result<Slot, MdsError> {
    let held = hold_map(ctx);
    let slot = evaluate_expr(expr, scope, ctx);
    release_map(ctx, held);
    slot
}

/// Call a function whose result is an argument of another call: the callee gets the
/// value, and its text is not output here. The map is held for the call.
pub(super) fn call_held(
    name: &str,
    args: &[Arg],
    scope: &mut Scope,
    ctx: &mut EvalContext,
    depth: usize,
) -> Result<Slot, MdsError> {
    let held = hold_map(ctx);
    let slot = call_function(name, args, scope, ctx, depth);
    release_map(ctx, held);
    slot
}
