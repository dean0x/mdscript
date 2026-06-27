//! Parser helper functions extracted from `parser.rs`.
//!
//! This module contains the low-level parsing primitives used by the main
//! [`super::parser`] module. Responsibilities are organised by concern:
//!
//! - **Scan primitive** — [`scan_bytes`]: shared quote+escape+optional-paren state machine
//!   used by the 6 byte-level scanners.
//! - **Directive string utilities** — `strip_trailing_directive_colon`,
//!   `has_unterminated_string`
//! - **Expression parsing** — `parse_expr_inner`, `expr_dispatch` (shared dispatch)
//! - **Condition parsing** — `parse_condition`, `parse_negation_condition`,
//!   `find_unquoted_operator`, `parse_cond_value`
//! - **Directive parsing** — `parse_import_directive`, `parse_export_directive`,
//!   `parse_for_vars`, `parse_define_params`
//! - **Interpolation parsing** — `parse_interpolation_expr`, `parse_dot_expr`,
//!   `parse_args`, `parse_args_inner`, `parse_single_arg`, `parse_single_arg_inner`
//! - **Utilities** — `parse_quoted_path`, `validate_dot_path_parts`,
//!   `unescape_string`, `is_valid_identifier`, `is_directive_token`,
//!   `strip_leading_newline`, `strip_trailing_newline`

use std::collections::HashSet;

use crate::ast::{
    Arg, Condition, ExportDirective, Expr, ImportDirective, Interpolation, Node, Param,
};
use crate::error::MdsError;
use crate::limits::{MAX_DOT_SEGMENTS, MAX_LOGICAL_OPERANDS, MAX_NESTING_DEPTH};

// ── Byte-level scan primitive ─────────────────────────────────────────────────

/// Shared byte-level state machine for scanning a `&str` with quote and
/// optional paren tracking.
///
/// Iterates over `bytes` maintaining escape-aware quote state.  For each byte
/// that lies **outside** a string literal the closure `on_unquoted` is called
/// with `(byte_index, byte, paren_depth)`.  `paren_depth` is always `0` when
/// `track_parens` is `false`.
///
/// Early termination: the closure returns `true` to stop scanning immediately.
///
/// # Safety (UTF-8)
///
/// All bytes that the closure acts on (`!`, `=`, `(`, `)`, `:`, etc.) are
/// single-byte ASCII (≤ 0x7F).  UTF-8 continuation bytes always have the high
/// bit set (≥ 0x80), so they cannot alias any ASCII sentinel.  Byte indices
/// returned by callers therefore always fall on valid `str` boundaries.
#[inline]
fn scan_bytes<F>(bytes: &[u8], track_parens: bool, mut on_unquoted: F)
where
    F: FnMut(usize, u8, usize) -> bool,
{
    let len = bytes.len();
    let mut i = 0;
    let mut in_string = false;
    let mut string_char = b'"';
    let mut paren_depth: usize = 0;

    while i < len {
        let ch = bytes[i];

        if in_string {
            // A backslash inside a string always consumes the next byte.
            if ch == b'\\' && i + 1 < len {
                i += 2;
                continue;
            }
            if ch == string_char {
                in_string = false;
            }
            i += 1;
            continue;
        }

        // Outside a string: check for quote open, parens (optional), then dispatch.
        match ch {
            b'"' | b'\'' => {
                in_string = true;
                string_char = ch;
            }
            b'(' if track_parens => paren_depth += 1,
            b')' if track_parens => paren_depth = paren_depth.saturating_sub(1),
            _ => {
                if on_unquoted(i, ch, paren_depth) {
                    return;
                }
            }
        }
        i += 1;
    }
}

// ── Expression dispatch ────────────────────────────────────────────────────────

/// Result of the shared dot/paren dispatch used by both `parse_expr_inner` and
/// `parse_interpolation_expr`.
///
/// Both functions inspect the first `.` and first `(` in the trimmed input to
/// decide which expression form to parse.  This type captures that decision so
/// the two callers can share the dispatch logic without sharing their error
/// messages or return types.
enum ExprDispatch {
    /// A `.` appears before any `(` (or without any `(`).  `dot_pos` is the
    /// byte index of the first `.` in the trimmed string.
    DotFirst { dot_pos: usize },
    /// A `(` appears with no `.` before it (or with no `.` at all).
    /// `paren_pos` is the byte index of the first `(`.
    ParenFirst { paren_pos: usize },
    /// Neither `.` nor `(` — simple identifier or literal.
    Neither,
}

/// Classify the trimmed expression string into one of three dispatch branches.
///
/// Shared by [`parse_expr_inner`] (directives) and [`parse_interpolation_expr`]
/// (interpolations).  Applying ADR-010: a single grammar for call/dot/var
/// dispatch that both callers specialise for their output type and error style.
#[inline]
fn expr_dispatch(s: &str) -> ExprDispatch {
    let first_dot = s.find('.');
    let first_paren = s.find('(');
    match (first_dot, first_paren) {
        (Some(d), Some(p)) if d < p => ExprDispatch::DotFirst { dot_pos: d },
        (Some(d), None) => ExprDispatch::DotFirst { dot_pos: d },
        (_, Some(p)) => ExprDispatch::ParenFirst { paren_pos: p },
        (None, None) => ExprDispatch::Neither,
    }
}

/// Return `true` if `s` is a complete, properly-terminated quoted string literal.
///
/// A string is complete when:
/// 1. It begins and ends with the same quote character (`"` or `'`), and
/// 2. The final character is **not** preceded by an odd number of backslashes
///    (i.e., the closing quote is not escaped).
///
/// Examples:
/// - `"hello"` → true
/// - `"say \"hi\""` → true (inner `\"` is escaped; the outer `"` closes the string)
/// - `"\""` → false (the only closing candidate is escaped — unterminated)
/// - `"\\"` → true (double-backslash then unescaped closing quote)
fn is_complete_string_literal(s: &str) -> bool {
    let bytes = s.as_bytes();
    let n = bytes.len();
    if n < 2 {
        return false;
    }
    let open = bytes[0];
    if open != b'"' && open != b'\'' {
        return false;
    }
    if bytes[n - 1] != open {
        return false;
    }
    // Count consecutive backslashes immediately before the closing quote.
    // An even number means the closing quote is unescaped; an odd number means it is escaped.
    let mut backslashes: usize = 0;
    let mut i = n - 2; // byte just before the closing quote
    loop {
        if bytes[i] != b'\\' {
            break;
        }
        backslashes += 1;
        if i == 1 {
            break; // reached the byte after the opening quote — stop
        }
        i -= 1;
    }
    backslashes.is_multiple_of(2)
}

/// Return `true` if `s` contains a bare `=` (not `==` and not `!=`) that appears
/// outside any quoted string or parenthesised group.
///
/// Used by [`parse_simple_condition`] to give a targeted error hint when a user
/// writes `var = "value"` instead of `var == "value"`.
fn has_bare_equals(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut found = false;
    scan_bytes(bytes, true, |i, ch, depth| {
        if ch == b'=' && depth == 0 {
            let next = if i + 1 < bytes.len() { bytes[i + 1] } else { 0 };
            if next != b'=' && !(i > 0 && bytes[i - 1] == b'!') {
                found = true;
                return true; // stop
            }
        }
        false
    });
    found
}

/// Strip the trailing `:` from a directive condition/iterable string, respecting
/// quoted strings and parentheses so that colons inside string literals or function
/// arguments are not mistaken for the directive colon.
///
/// Returns `Some(stripped)` when the last unquoted, unparenthesised character
/// is `:`, or `None` if no such colon exists (indicating a malformed directive).
pub(super) fn strip_trailing_directive_colon(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }

    // Track the last bare colon and final paren depth separately so we can
    // detect unclosed parens after the scan.
    let mut last_bare_colon: Option<usize> = None;
    let mut final_paren_depth: usize = 0;

    // scan_bytes does not expose the paren depth after the loop, so we track
    // it ourselves by mirroring the paren logic through the closure.
    //
    // Note: scan_bytes does NOT call the closure for `(` / `)` when
    // track_parens=true — they are consumed by the inner paren tracking.
    // We need the final depth, so we run a short second pass only for parens,
    // or we replicate the paren tracking inside the closure via a cell.
    //
    // The simplest approach: use `track_parens=false` and handle all bytes in
    // the closure, maintaining our own paren depth.  This keeps the closure
    // fully in control of paren state while still benefiting from
    // scan_bytes' quote+escape handling.
    scan_bytes(bytes, false, |i, ch, _depth| {
        match ch {
            b'(' => final_paren_depth += 1,
            b')' => final_paren_depth = final_paren_depth.saturating_sub(1),
            b':' if final_paren_depth == 0 => {
                last_bare_colon = Some(i);
            }
            _ => {}
        }
        false // never stop early
    });

    // An unclosed parenthesis group means the directive string is structurally
    // malformed (e.g. `func(a:`). Treat it the same as a missing colon so the
    // caller reports a parse error rather than silently using a colon that was
    // inside an argument list.
    if final_paren_depth > 0 {
        return None;
    }

    last_bare_colon.and_then(|pos| {
        // Only use the colon if it is at the very end of the (trimmed) string.
        let after = s[pos + 1..].trim();
        if after.is_empty() {
            Some(s[..pos].trim_end())
        } else {
            None
        }
    })
}

/// Return `true` if the string contains an unterminated string literal
/// (a quote that is opened but never closed).
///
/// Used to give targeted error messages when a directive appears to be missing
/// its trailing colon due to an unterminated string literal inside it.
///
/// Note: this scanner intentionally does **not** track paren depth — only
/// quote state matters for its purpose.
pub(super) fn has_unterminated_string(s: &str) -> bool {
    // scan_bytes tracks quote state for us; if any byte is reached that is
    // "outside a string" (i.e. the quote was closed), we do nothing.
    // After the scan, we cannot directly query "are we in_string?" from
    // scan_bytes.  Instead we exploit the invariant: scan_bytes processes bytes
    // INSIDE strings internally (skipping them) and only calls the closure for
    // bytes OUTSIDE strings.  The final quote state is implicit: if we never
    // see the closing quote byte through the string-tracking machinery, the
    // string remains open.
    //
    // Easier: replicate the minimal state machine directly.  scan_bytes handles
    // the common case but `has_unterminated_string` is uniquely about the
    // FINAL state of in_string, which scan_bytes does not expose.  We
    // therefore run it without scan_bytes to keep the logic transparent and to
    // avoid a mutable capture smuggled through a closure.
    let bytes = s.as_bytes();
    let mut in_string = false;
    let mut string_char = b'"';
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i];
        if in_string {
            if ch == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if ch == string_char {
                in_string = false;
            }
        } else if ch == b'"' || ch == b'\'' {
            in_string = true;
            string_char = ch;
        }
        i += 1;
    }
    in_string
}

/// Parse a function-call or qualified-call expression from `parse_expr_inner`.
///
/// Called when a `(` appears before any `.` (simple call) or when a `.` precedes
/// a `(` (qualified call: `ns.func(args)`). Returns `None` when the input falls
/// through to the member-access path instead.
fn parse_call_expr(
    s: &str,
    first_dot: Option<usize>,
    first_paren: usize,
) -> Option<Result<Expr, MdsError>> {
    if first_dot.is_none_or(|d| first_paren < d) {
        // Simple call: func(args)
        return Some(parse_simple_call_expr(s, first_paren));
    }

    // dot before paren: qualified call — or falls through to member-access if no '(' after dot.
    let dot_pos = first_dot?;
    let rest_after_dot = &s[dot_pos + 1..];
    let paren_in_rest = rest_after_dot.find('(')?;
    Some(parse_qualified_call_expr(
        s,
        dot_pos,
        rest_after_dot,
        paren_in_rest,
    ))
}

fn parse_simple_call_expr(s: &str, paren_pos: usize) -> Result<Expr, MdsError> {
    let name = s[..paren_pos].trim().to_string();
    if !is_valid_identifier(&name) {
        return Err(MdsError::syntax(format!(
            "invalid function name in directive expression: '{name}'"
        )));
    }
    let args_str = s[paren_pos + 1..]
        .trim()
        .strip_suffix(')')
        .ok_or_else(|| MdsError::syntax("unclosed parenthesis in directive expression"))?;
    let args = parse_args(args_str)?;
    Ok(Expr::Call { name, args })
}

fn parse_qualified_call_expr(
    s: &str,
    dot_pos: usize,
    rest_after_dot: &str,
    paren_in_rest: usize,
) -> Result<Expr, MdsError> {
    let namespace = s[..dot_pos].trim().to_string();
    let name = rest_after_dot[..paren_in_rest].trim().to_string();
    if !is_valid_identifier(&namespace) {
        return Err(MdsError::syntax(format!(
            "invalid namespace in qualified call: '{namespace}'"
        )));
    }
    if !is_valid_identifier(&name) {
        return Err(MdsError::syntax(format!(
            "invalid function name in qualified call: '{name}'"
        )));
    }
    let args_str = rest_after_dot[paren_in_rest + 1..]
        .trim()
        .strip_suffix(')')
        .ok_or_else(|| MdsError::syntax("unclosed parenthesis in qualified call expression"))?;
    let args = parse_args(args_str)?;
    Ok(Expr::QualifiedCall {
        namespace,
        name,
        args,
    })
}

/// Parse an expression for use in directive conditions or iterables.
///
/// Accepts the same forms as interpolation expressions plus literal values:
/// - Quoted strings → `Expr::StringLiteral`
/// - Numeric literals → `Expr::NumberLiteral`
/// - Boolean literals → `Expr::BooleanLiteral`
/// - `null` → `Expr::NullLiteral`
/// - `identifier` → `Expr::Var`
/// - `ns.func(args)` → `Expr::QualifiedCall`
/// - `func(args)` → `Expr::Call`
/// - `obj.field` → `Expr::MemberAccess`
///
/// The call/dot/var dispatch is shared with [`parse_interpolation_expr`] via
/// [`expr_dispatch`] (ADR-010).  This function adds literal handling on top —
/// interpolations intentionally do **not** accept literals.
pub(super) fn parse_expr_inner(s: &str) -> Result<Expr, MdsError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(MdsError::syntax("expected an expression"));
    }

    // Quoted string literal — only accept when the closing quote is not escaped.
    if is_complete_string_literal(s) {
        let inner = &s[1..s.len() - 1];
        return Ok(Expr::StringLiteral(unescape_string(inner)));
    }

    // Unterminated string
    if s.starts_with('"') || s.starts_with('\'') {
        return Err(MdsError::syntax(
            "unterminated string literal in directive expression",
        ));
    }

    // Boolean literals
    if s == "true" {
        return Ok(Expr::BooleanLiteral(true));
    }
    if s == "false" {
        return Ok(Expr::BooleanLiteral(false));
    }

    // Null literal
    if s == "null" {
        return Ok(Expr::NullLiteral);
    }

    // Numeric literal (checked before dot-path so "3.14" is a number, not MemberAccess)
    if looks_like_number(s) {
        if let Ok(n) = s.parse::<f64>() {
            if !n.is_finite() {
                return Err(MdsError::syntax(
                    "NaN and infinity are not valid directive expression values",
                ));
            }
            return Ok(Expr::NumberLiteral(n));
        }
    }

    // Shared call/dot/var dispatch (ADR-010).
    match expr_dispatch(s) {
        ExprDispatch::ParenFirst { paren_pos } => {
            // `(` without a prior dot: simple call, or qualified-call falls through.
            if let Some(result) = parse_call_expr(s, s.find('.'), paren_pos) {
                return result;
            }
            // parse_call_expr returns None only when dot-before-paren → member access.
            // That case is handled by DotFirst below; we shouldn't reach here.
        }
        ExprDispatch::DotFirst { dot_pos } => {
            // dot before any `(`: could be qualified call or member access.
            // Delegate to parse_call_expr which handles both.
            let first_paren = s.find('(');
            if let Some(paren_pos) = first_paren {
                if let Some(result) = parse_call_expr(s, Some(dot_pos), paren_pos) {
                    return result;
                }
            }
            // No `(` after the dot — pure member access.
            let parts: Vec<&str> = s.split('.').collect();
            validate_dot_path_parts(&parts).map_err(|reason| {
                MdsError::syntax(format!(
                    "invalid dot-path in directive expression: '{s}' — {reason}"
                ))
            })?;
            let object = parts[0].trim().to_string();
            let fields: Vec<String> = parts[1..].iter().map(|p| p.trim().to_string()).collect();
            return Ok(Expr::MemberAccess { object, fields });
        }
        ExprDispatch::Neither => {}
    }

    // Simple identifier
    if is_valid_identifier(s) {
        return Ok(Expr::Var(s.to_string()));
    }

    Err(MdsError::syntax(format!(
        "invalid expression in directive: '{s}' — expected a variable, function call, or literal"
    )))
}

/// Parse a **literal** default value for a `@define` parameter.
///
/// Accepts exactly four forms, returning the matching literal `Expr` variant:
/// - Quoted strings: `"admin"` or `'admin'` → [`Expr::StringLiteral`]
/// - Numbers: `42`, `-5`, `3.14` → [`Expr::NumberLiteral`]
/// - Booleans: `true`, `false` → [`Expr::BooleanLiteral`]
/// - Null: `null` → [`Expr::NullLiteral`]
///
/// # Literal-only guard (zero-behaviour-change)
///
/// `Param.default` is typed as `Option<Expr>`, so the type *could* represent any
/// expression (a function call, a variable reference, an interpolation). It must
/// not: a non-literal default such as `@define f(x = upper("a"))` has always been
/// rejected and must stay rejected. This function never constructs a non-literal
/// variant — anything that is not one of the four literal forms above falls
/// through to the trailing `Err` ("comparison values must be string, number,
/// boolean, or null"), which [`parse_define_params`] surfaces as the
/// "invalid default value for parameter" diagnostic. There is therefore no path
/// by which a non-literal `Expr` reaches `Param.default`.
///
/// The accepted forms and the exact error strings are preserved verbatim from the
/// pre-unification dedicated default-literal parser so that retyping the result
/// to `Expr` is byte-for-byte behaviour-preserving.
pub(super) fn parse_cond_value(s: &str) -> Result<Expr, MdsError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(MdsError::syntax(
            "comparison values must be string, number, boolean, or null",
        ));
    }

    // Quoted string (single or double) — only accept when the closing quote is not escaped.
    if is_complete_string_literal(s) {
        let inner = &s[1..s.len() - 1];
        return Ok(Expr::StringLiteral(unescape_string(inner)));
    }

    // Unterminated string
    if s.starts_with('"') || s.starts_with('\'') {
        return Err(MdsError::syntax(
            "unterminated string literal in @if condition",
        ));
    }

    // Boolean literals
    if s == "true" {
        return Ok(Expr::BooleanLiteral(true));
    }
    if s == "false" {
        return Ok(Expr::BooleanLiteral(false));
    }

    // Null literal
    if s == "null" {
        return Ok(Expr::NullLiteral);
    }

    // Numeric (integer or float, including negative)
    if let Ok(n) = s.parse::<f64>() {
        if !n.is_finite() {
            return Err(MdsError::syntax(
                "NaN and infinity are not valid condition values",
            ));
        }
        return Ok(Expr::NumberLiteral(n));
    }

    Err(MdsError::syntax(
        "comparison values must be string, number, boolean, or null",
    ))
}

/// Scan `s` for the first unquoted `==` or `!=` operator that is outside
/// both string literals and parentheses.
///
/// Tracks whether the scanner is inside a single- or double-quoted string and
/// whether it is inside parentheses (to handle function-call arguments like
/// `contains(s, "==")` correctly). Only reports an operator when outside any
/// string or paren nesting.
///
/// Returns `Some((byte_index, "=="` or `"!="))` pointing to the start of the
/// operator, or `None` if no unquoted operator is present.
///
/// # Byte-level scan safety
///
/// This function receives a `&str`, which Rust guarantees is valid UTF-8.  The
/// ASCII bytes we search for (`!`, `=`, `"`, `'`, `\`, `(`, `)`) are all
/// single-byte characters in the range 0x00–0x7F.  In UTF-8, continuation bytes
/// of multi-byte sequences always have their high bit set (≥ 0x80), so they can
/// never be mistaken for any ASCII byte we inspect.  Consequently, scanning
/// `s.as_bytes()` byte-by-byte and acting only on those ASCII sentinel values
/// is sound: we never split a multi-byte code-point, and every byte index we
/// return points to the first byte of an ASCII character that is also a valid
/// `str` boundary.
pub(super) fn find_unquoted_operator(s: &str) -> Option<(usize, &'static str)> {
    let bytes = s.as_bytes();
    let mut result: Option<(usize, &'static str)> = None;
    scan_bytes(bytes, true, |i, ch, depth| {
        if depth == 0 {
            // Check for != (must precede == check to avoid `!` consuming `=`)
            if ch == b'!' && i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                result = Some((i, "!="));
                return true; // stop
            }
            if ch == b'=' && i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                result = Some((i, "=="));
                return true; // stop
            }
        }
        false
    });
    result
}

/// Parse the body of a negation condition (everything after the leading `!`).
///
/// Validates that the negation is not doubled (`!!`), not combined with a
/// comparison operator, and not missing the expression.  Returns
/// `Condition::Not(expr)` on success.
pub(super) fn parse_negation_condition(rest: &str) -> Result<Condition, MdsError> {
    if rest.starts_with('!') {
        return Err(MdsError::syntax("double negation is not supported"));
    }
    if find_unquoted_operator(rest).is_some() {
        return Err(MdsError::syntax(
            "cannot combine negation with comparison; use @if var != 'value': instead",
        ));
    }
    let rest = rest.trim();
    if rest.is_empty() {
        return Err(MdsError::syntax("expected variable name after '!'"));
    }
    let expr = parse_expr_inner(rest)?;
    // Reject bare literals in negation position: !true, !"str", !42, !null make no sense.
    match &expr {
        Expr::StringLiteral(_)
        | Expr::NumberLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NullLiteral => {
            return Err(MdsError::syntax(
                "cannot use a literal value after '!' — use a variable or function call",
            ));
        }
        _ => {}
    }
    Ok(Condition::Not(expr))
}

/// Split a string on a 2-character operator (`&&` or `||`) that appears outside
/// of quoted strings and outside parentheses.
///
/// Returns a `Vec<&str>` of the parts (not trimmed). Returns a single-element
/// vec containing the original string if the operator is not found.
///
/// # Byte-level scan safety
///
/// `&&` and `||` are both ASCII (single-byte), so byte-level scanning is sound
/// for the same reason as `find_unquoted_operator`.
///
/// # Two-byte advance
///
/// [`scan_bytes`] advances one byte at a time, so after matching a 2-byte
/// operator at index `i` the closure is re-entered on the operator's *second*
/// byte (`i + 1`). The `skip_next` flag suppresses that re-entry: the byte right
/// after a matched operator is never examined as a potential match start or
/// segment boundary. This reproduces the pre-`scan_bytes` hand-rolled loop,
/// which advanced by 2 after a match. Without it, three or more consecutive
/// operator characters would let `segment_start` (`= i + 2`) exceed the current
/// index and produce a reversed `&s[segment_start..i]` byte range — a panic.
pub(super) fn split_on_unquoted_op<'a>(s: &'a str, op: &str) -> Vec<&'a str> {
    debug_assert_eq!(op.len(), 2, "op must be a 2-byte ASCII operator");
    let op_bytes = op.as_bytes();
    let bytes = s.as_bytes();
    let mut parts: Vec<&'a str> = Vec::new();
    let mut segment_start = 0;
    let mut skip_next = false;

    scan_bytes(bytes, true, |i, ch, depth| {
        // Skip the operator's second byte: scan_bytes re-enters on it, but it is
        // part of the operator already consumed, never a new boundary.
        if skip_next {
            skip_next = false;
            return false;
        }
        if depth == 0 && i + 1 < bytes.len() && ch == op_bytes[0] && bytes[i + 1] == op_bytes[1] {
            parts.push(&s[segment_start..i]);
            segment_start = i + 2;
            skip_next = true;
        }
        false // never stop early — collect all occurrences
    });

    parts.push(&s[segment_start..]);
    parts
}

/// Count the total number of leaf operands in a condition tree.
/// Used to enforce MAX_LOGICAL_OPERANDS.
fn count_leaf_operands(condition: &Condition) -> usize {
    match condition {
        Condition::And(ops) | Condition::Or(ops) => ops.iter().map(count_leaf_operands).sum(),
        _ => 1,
    }
}

/// Parse an `@if` condition at the "and-level" — a chain of `&&` separated simple conditions.
fn parse_and_level(s: &str) -> Result<Condition, MdsError> {
    let parts = split_on_unquoted_op(s, "&&");
    if parts.len() == 1 {
        return parse_simple_condition(parts[0].trim());
    }
    let mut operands: Vec<Condition> = Vec::new();
    for part in &parts {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            return Err(MdsError::syntax("empty operand in '&&' expression"));
        }
        operands.push(parse_simple_condition(trimmed)?);
    }
    Ok(Condition::And(operands))
}

/// Parse an `@if` or `@elseif` condition string into a `Condition`.
///
/// Accepted forms:
/// - `var` / `config.debug` / `func(args)` / `ns.func(args)` → `Condition::Truthy`
/// - `!var` / `!func(args)` → `Condition::Not`
/// - `expr == expr` / `expr != expr` → `Condition::Eq` / `Condition::NotEq`
///   where each side is any expression accepted by `parse_expr_inner`:
///   variables, dot-paths, function calls, string/number/boolean/null literals.
///   Examples: `var == "value"`, `var != 42`, `func(a) == func(b)`,
///   `env.get("KEY") != null`
/// - `a && b` → `Condition::And([a, b])`
/// - `a || b` → `Condition::Or([a, b])`
/// - `a && b || c` → `Condition::Or([And([a, b]), c])` (`||` binds less tightly)
pub(super) fn parse_condition(s: &str) -> Result<Condition, MdsError> {
    let s = s.trim();

    // Split on `||` first (lower precedence)
    let or_parts = split_on_unquoted_op(s, "||");
    if or_parts.len() > 1 {
        let mut operands: Vec<Condition> = Vec::new();
        for part in &or_parts {
            let trimmed = part.trim();
            if trimmed.is_empty() {
                return Err(MdsError::syntax("empty operand in '||' expression"));
            }
            operands.push(parse_and_level(trimmed)?);
        }
        let cond = Condition::Or(operands);
        let leaf_count = count_leaf_operands(&cond);
        if leaf_count > MAX_LOGICAL_OPERANDS {
            return Err(MdsError::syntax(format!(
                "logical expression has {leaf_count} operands, exceeding maximum of {MAX_LOGICAL_OPERANDS}"
            )));
        }
        return Ok(cond);
    }

    // No `||` — check for `&&`
    let cond = parse_and_level(s)?;
    if let Condition::And(_) = &cond {
        let leaf_count = count_leaf_operands(&cond);
        if leaf_count > MAX_LOGICAL_OPERANDS {
            return Err(MdsError::syntax(format!(
                "logical expression has {leaf_count} operands, exceeding maximum of {MAX_LOGICAL_OPERANDS}"
            )));
        }
    }
    Ok(cond)
}

/// Parse a single (non-compound) `@if` or `@elseif` condition.
///
/// Accepted forms:
/// - `var` / `config.debug` / `func(args)` / `ns.func(args)` → `Condition::Truthy`
/// - `!var` / `!func(args)` → `Condition::Not`
/// - `expr == expr` / `expr != expr` → `Condition::Eq` / `Condition::NotEq`
fn parse_simple_condition(s: &str) -> Result<Condition, MdsError> {
    let s = s.trim();

    // Negation prefix
    if let Some(rest) = s.strip_prefix('!') {
        return parse_negation_condition(rest);
    }

    // Equality/inequality operators (only outside quotes and parens)
    if let Some((op_pos, op)) = find_unquoted_operator(s) {
        let lhs = s[..op_pos].trim();
        let rhs_start = op_pos + op.len();
        let rhs = s[rhs_start..].trim();

        if lhs.is_empty() {
            return Err(MdsError::syntax(format!(
                "expected expression before '{op}'"
            )));
        }
        if rhs.is_empty() {
            return Err(MdsError::syntax(format!("expected value after '{op}'")));
        }

        let lhs_expr = parse_expr_inner(lhs)?;
        let rhs_expr = parse_expr_inner(rhs)?;

        return match op {
            "==" => Ok(Condition::Eq(lhs_expr, rhs_expr)),
            "!=" => Ok(Condition::NotEq(lhs_expr, rhs_expr)),
            other => Err(MdsError::syntax(format!(
                "internal error: unrecognised operator '{other}' in @if condition"
            ))),
        };
    }

    // Check for bare `=` (not `==`) — give a targeted hint.
    if has_bare_equals(s) {
        return Err(MdsError::syntax("use '==' for comparison, not '='"));
    }

    // Default: truthy check — evaluate as expression
    let expr = parse_expr_inner(s)?;
    // Reject bare literals in truthy position: @if true: / @if "str": make no sense.
    match &expr {
        Expr::StringLiteral(_)
        | Expr::NumberLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NullLiteral => {
            return Err(MdsError::syntax(
                "cannot use a literal value in @if condition — use a variable or function call",
            ));
        }
        _ => {}
    }
    Ok(Condition::Truthy(expr))
}

/// Parse an `@import` directive into a Node.
pub(super) fn parse_import_directive(directive: &str, offset: usize) -> Result<Node, MdsError> {
    let rest = directive.trim_start_matches("@import").trim();

    // Selective import: @import { name1, name2 } from "path"
    if rest.starts_with('{') {
        let brace_end = rest
            .find('}')
            .ok_or_else(|| MdsError::syntax("unclosed { in selective import"))?;
        let names_str = &rest[1..brace_end];
        let names: Vec<String> = names_str
            .split(',')
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .collect();

        for name in &names {
            if !is_valid_identifier(name) {
                return Err(MdsError::syntax(format!("invalid import name: '{name}'")));
            }
        }

        let after = rest[brace_end + 1..].trim();
        let path_part = after
            .strip_prefix("from ")
            .or_else(|| after.strip_prefix("from\t"))
            .ok_or_else(|| MdsError::syntax("selective import requires 'from' keyword"))?
            .trim();
        let path = parse_quoted_path(path_part)?;

        return Ok(Node::Import(ImportDirective::Selective {
            names,
            path,
            offset,
        }));
    }

    // Alias import: @import "path" as alias
    // Merge import: @import "path"
    let path = parse_quoted_path(rest)?;
    // Skip past the quoted path: opening `"` + content + closing `"`
    let quoted_len = 2 + path.len();
    let after = rest[quoted_len..].trim();

    if let Some(alias) = after.strip_prefix("as ") {
        let alias = alias.trim();
        if !is_valid_identifier(alias) {
            return Err(MdsError::syntax(format!("invalid import alias: '{alias}'")));
        }
        Ok(Node::Import(ImportDirective::Alias {
            path,
            alias: alias.to_string(),
            offset,
        }))
    } else if after.is_empty() {
        Ok(Node::Import(ImportDirective::Merge { path, offset }))
    } else {
        Err(MdsError::syntax(format!(
            "unexpected text after import path: '{after}'"
        )))
    }
}

/// Parse an `@export` directive into a Node.
pub(super) fn parse_export_directive(directive: &str) -> Result<Node, MdsError> {
    let rest = directive.trim_start_matches("@export").trim();

    // Wildcard re-export: @export * from "path"
    if let Some(from_part) = rest
        .strip_prefix("* from ")
        .or_else(|| rest.strip_prefix("*from "))
    {
        let path = parse_quoted_path(from_part.trim())?;
        return Ok(Node::Export(ExportDirective::Wildcard { path }));
    }

    // Check for "name from" pattern: @export name from "path"
    let parts: Vec<&str> = rest.splitn(3, ' ').collect();
    if parts.len() >= 3 && parts[1] == "from" {
        let name = parts[0].to_string();
        if !is_valid_identifier(&name) {
            return Err(MdsError::syntax(format!(
                "invalid re-export name: '{name}'"
            )));
        }
        let path = parse_quoted_path(parts[2])?;
        return Ok(Node::Export(ExportDirective::ReExport { name, path }));
    }

    // Named export: @export name
    let name = rest.trim().to_string();
    if name.is_empty() {
        return Err(MdsError::syntax("@export requires a name"));
    }
    if !is_valid_identifier(&name) {
        return Err(MdsError::syntax(format!("invalid export name: '{name}'")));
    }
    Ok(Node::Export(ExportDirective::Named { name }))
}

/// Parse a quoted path like `"./utils.mds"` and return the inner string.
pub(super) fn parse_quoted_path(s: &str) -> Result<String, MdsError> {
    let s = s.trim();
    if !s.starts_with('"') {
        return Err(MdsError::syntax(format!("expected quoted path, got: {s}")));
    }
    let end = s[1..]
        .find('"')
        .ok_or_else(|| MdsError::syntax("unclosed quote in path"))?;
    Ok(s[1..=end].to_string())
}

/// Parse the variable part of a `@for` directive (before `in`).
///
/// Accepts either a single loop variable (`item`) or a key-value pair
/// (`key, value`). Returns `(key_var, loop_var)` where `key_var` is `Some`
/// only for key-value iteration.
pub(super) fn parse_for_vars(var_part: &str) -> Result<(Option<String>, String), MdsError> {
    if let Some(comma_idx) = var_part.find(',') {
        let key = var_part[..comma_idx].trim().to_string();
        let val = var_part[comma_idx + 1..].trim().to_string();
        if !is_valid_identifier(&key) {
            return Err(MdsError::syntax(format!(
                "invalid key variable name: '{key}'"
            )));
        }
        if !is_valid_identifier(&val) {
            return Err(MdsError::syntax(format!(
                "invalid value variable name: '{val}'"
            )));
        }
        Ok((Some(key), val))
    } else if !is_valid_identifier(var_part) {
        Err(MdsError::syntax(format!(
            "invalid loop variable name: '{var_part}'"
        )))
    } else {
        Ok((None, var_part.to_string()))
    }
}

/// Validates that every segment of a split dot-path is a valid identifier and that the
/// segment count does not exceed `MAX_DOT_SEGMENTS`.
///
/// Returns `Ok(())` on success, or `Err` with a human-readable reason string that the
/// caller embeds into the appropriate `MdsError` variant (syntax or syntax_at).
pub(super) fn validate_dot_path_parts(parts: &[&str]) -> Result<(), String> {
    if parts.len() > MAX_DOT_SEGMENTS {
        return Err(format!(
            "dot path exceeds maximum segment count of {MAX_DOT_SEGMENTS}"
        ));
    }
    for part in parts.iter().map(|p| p.trim()) {
        if !is_valid_identifier(part) {
            return Err(format!(
                "each segment must be a valid identifier (got '{part}')"
            ));
        }
    }
    Ok(())
}

/// Resolve a dot-leading expression into a QualifiedCall or MemberAccess interpolation.
///
/// Called when a dot appears before any `(` in interpolation content, i.e.:
///   `{obj.key}`       → MemberAccess
///   `{ns.func(args)}` → QualifiedCall
///
/// `dot_pos` is the byte index of the first `.` in `content`.
pub(super) fn parse_dot_expr(
    content: &str,
    dot_pos: usize,
    offset: usize,
    len: usize,
    file: &str,
    source: &str,
) -> Result<Interpolation, MdsError> {
    let rest_after_dot = &content[dot_pos + 1..];

    if let Some(paren_pos) = rest_after_dot.find('(') {
        // namespace.func(args) — QualifiedCall
        let namespace = content[..dot_pos].trim().to_string();
        let name = rest_after_dot[..paren_pos].trim().to_string();
        if !is_valid_identifier(&namespace) {
            return Err(MdsError::syntax_at(
                format!("invalid namespace in qualified call: '{namespace}' — must be a valid identifier"),
                file, source, offset, len,
            ));
        }
        if !is_valid_identifier(&name) {
            return Err(MdsError::syntax_at(
                format!("invalid function name in qualified call: '{name}' — must be a valid identifier"),
                file, source, offset, len,
            ));
        }
        let args_str = rest_after_dot[paren_pos + 1..]
            .trim()
            .strip_suffix(')')
            .ok_or_else(|| {
                MdsError::syntax_at(
                    "unclosed parenthesis in function call",
                    file,
                    source,
                    offset,
                    len,
                )
            })?;
        let args = parse_args(args_str)?;
        return Ok(Interpolation {
            expr: Expr::QualifiedCall {
                namespace,
                name,
                args,
            },
            offset,
            len,
        });
    }

    // No '(' anywhere — obj.field or obj.field1.field2 (MemberAccess)
    let parts: Vec<&str> = content.split('.').collect();
    validate_dot_path_parts(&parts).map_err(|reason| {
        MdsError::syntax_at(
            format!("invalid dot-path in interpolation: '{content}' — {reason}"),
            file,
            source,
            offset,
            len,
        )
    })?;
    let object = parts[0].trim().to_string();
    let fields: Vec<String> = parts[1..].iter().map(|s| s.trim().to_string()).collect();
    Ok(Interpolation {
        expr: Expr::MemberAccess { object, fields },
        offset,
        len,
    })
}

/// Parse the expression inside `{ }` into an Interpolation.
///
/// Dispatches across three expression types using the shared [`expr_dispatch`]
/// helper (ADR-010):
///   dot before `(`  → [`parse_dot_expr`] (QualifiedCall or MemberAccess)
///   `(` without dot → Call
///   neither         → Var
///
/// Unlike [`parse_expr_inner`] (used for directive conditions), this function
/// intentionally does **not** accept literal values (strings, numbers, booleans,
/// null).  `{42}` or `{"hello"}` in interpolation position is a syntax error.
pub(super) fn parse_interpolation_expr(
    content: &str,
    offset: usize,
    file: &str,
    source: &str,
) -> Result<Interpolation, MdsError> {
    let content = content.trim();
    let len = content.len();

    // Shared call/dot/var dispatch (ADR-010).
    // No literal handling here — that is the deliberate no-literals difference.
    match expr_dispatch(content) {
        ExprDispatch::DotFirst { dot_pos } => {
            // dot before any `(`: QualifiedCall or MemberAccess.
            return parse_dot_expr(content, dot_pos, offset, len, file, source);
        }
        ExprDispatch::ParenFirst { paren_pos } => {
            // `(` without a prior dot: simple Call.
            let name = content[..paren_pos].trim().to_string();
            let args_str = content[paren_pos + 1..]
                .trim()
                .strip_suffix(')')
                .ok_or_else(|| {
                    MdsError::syntax_at(
                        "unclosed parenthesis in function call",
                        file,
                        source,
                        offset,
                        len,
                    )
                })?;
            let args = parse_args(args_str)?;
            return Ok(Interpolation {
                expr: Expr::Call { name, args },
                offset,
                len,
            });
        }
        ExprDispatch::Neither => {}
    }

    // Simple variable reference.
    if !is_valid_identifier(content) {
        return Err(MdsError::syntax_at(
            format!(
                "invalid interpolation: '{content}' is not a valid expression. Use a variable name (letters, numbers, underscores), a function call like func(), or escape with \\{{{{ for literal braces."
            ),
            file, source, offset, len,
        ));
    }
    Ok(Interpolation {
        expr: Expr::Var(content.to_string()),
        offset,
        len,
    })
}

/// Parse function call arguments.
/// Handles nested parentheses so that `inner("arg")` is kept as a single token.
pub(super) fn parse_args(args_str: &str) -> Result<Vec<Arg>, MdsError> {
    parse_args_inner(args_str, 0)
}

/// Inner recursive implementation of [`parse_args`].
///
/// `depth` tracks the current nesting level and is checked against
/// [`MAX_NESTING_DEPTH`] to prevent stack overflow on adversarial input.
pub(super) fn parse_args_inner(args_str: &str, depth: usize) -> Result<Vec<Arg>, MdsError> {
    if depth > MAX_NESTING_DEPTH {
        return Err(MdsError::syntax(format!(
            "nested function call depth exceeds maximum of {MAX_NESTING_DEPTH}"
        )));
    }
    let args_str = args_str.trim();
    if args_str.is_empty() {
        return Ok(Vec::new());
    }

    // State machine variables:
    //   current     — token accumulator for the argument being built
    //   in_string   — true while scanning inside a quoted string literal
    //   string_char — the quote character that opened the current string ('"' or '\'')
    //   escaped     — true when the previous character was a backslash inside a string
    //   paren_depth — tracks nested parentheses so commas inside calls are not treated as separators
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let mut string_char = '"';
    let mut escaped = false;
    let mut paren_depth: usize = 0;

    for ch in args_str.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if in_string {
            match ch {
                '\\' => {
                    escaped = true;
                    current.push(ch);
                }
                c if c == string_char => {
                    current.push(ch);
                    in_string = false;
                }
                _ => current.push(ch),
            }
            continue;
        }
        match ch {
            '"' | '\'' => {
                in_string = true;
                string_char = ch;
                current.push(ch);
            }
            '(' => {
                paren_depth += 1;
                current.push(ch);
            }
            ')' => {
                paren_depth = paren_depth.saturating_sub(1);
                current.push(ch);
            }
            ',' if paren_depth == 0 => {
                args.push(parse_single_arg_inner(current.trim(), depth)?);
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    let trimmed = current.trim();
    if !trimmed.is_empty() {
        args.push(parse_single_arg_inner(trimmed, depth)?);
    }

    Ok(args)
}

/// Return `true` if `s` looks like a numeric literal (integer or float,
/// optionally negative).
///
/// Used by [`parse_single_arg_inner`] to distinguish numeric arguments from
/// dot-paths and identifiers before attempting `s.parse::<f64>()`.
fn looks_like_number(s: &str) -> bool {
    s.chars().next().is_some_and(|c| {
        c.is_ascii_digit()
            || (c == '-' && s.len() > 1 && s[1..].starts_with(|d: char| d.is_ascii_digit()))
    })
}

/// Parse a single argument string into an [`Arg`] node.
///
/// Test-only convenience wrapper around [`parse_single_arg_inner`] that starts
/// at nesting depth 0.
#[cfg(test)]
pub(super) fn parse_single_arg(s: &str) -> Result<Arg, MdsError> {
    parse_single_arg_inner(s, 0)
}

/// Inner implementation of single-argument parsing.
///
/// Classifies `s` as a string literal, a nested function call, or a bare
/// identifier/variable reference. `depth` is forwarded to [`parse_args_inner`]
/// for nested calls.
pub(super) fn parse_single_arg_inner(s: &str, depth: usize) -> Result<Arg, MdsError> {
    let s = s.trim();
    // Only accept as a complete string literal when the closing quote is not escaped.
    if is_complete_string_literal(s) {
        let inner = &s[1..s.len() - 1];
        let unescaped = unescape_string(inner);
        Ok(Arg::StringLiteral(unescaped))
    } else if let Some(paren_pos) = s.find('(') {
        // Nested function call: name(args)
        let name = s[..paren_pos].trim().to_string();
        if !is_valid_identifier(&name) {
            return Err(MdsError::syntax(format!(
                "invalid function name in argument: '{name}'"
            )));
        }
        let inner = s[paren_pos + 1..]
            .strip_suffix(')')
            .ok_or_else(|| MdsError::syntax("unclosed parenthesis in nested function call"))?;
        let nested_args = parse_args_inner(inner, depth + 1)?;
        Ok(Arg::Call {
            name,
            args: nested_args,
        })
    } else if s == "true" {
        Ok(Arg::BooleanLiteral(true))
    } else if s == "false" {
        Ok(Arg::BooleanLiteral(false))
    } else if s == "null" {
        Ok(Arg::NullLiteral)
    } else if looks_like_number(s) {
        // Numeric literal: integer or float, including negative.
        // Checked before member access so `3.14` is parsed as a number, not a dot-path.
        match s.parse::<f64>() {
            Ok(n) if n.is_finite() => Ok(Arg::NumberLiteral(n)),
            Ok(_) => Err(MdsError::syntax(format!(
                "NaN and infinity are not valid argument values: '{s}'"
            ))),
            Err(_) => Err(MdsError::syntax(format!("invalid numeric argument: '{s}'"))),
        }
    } else if s.contains('.') && !s.contains('(') {
        // Object member access as argument: config.name or a.b.c
        let parts: Vec<&str> = s.split('.').collect();
        validate_dot_path_parts(&parts).map_err(|reason| {
            MdsError::syntax(format!("invalid dot-path in argument: '{s}' — {reason}"))
        })?;
        Ok(Arg::MemberAccess {
            object: parts[0].trim().to_string(),
            fields: parts[1..].iter().map(|s| s.trim().to_string()).collect(),
        })
    } else if is_valid_identifier(s) {
        // Variable reference
        Ok(Arg::Var(s.to_string()))
    } else {
        Err(MdsError::syntax(format!(
            "invalid function argument: '{s}'"
        )))
    }
}

/// Split a string on commas that are not inside quoted strings.
///
/// Used to split function parameter lists while respecting quoted default values.
/// Returns a `Vec<String>` of raw tokens (trimmed).
fn split_on_unquoted_commas(s: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let mut string_char = '"';
    let mut escaped = false;

    for ch in s.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if in_string {
            match ch {
                '\\' => {
                    escaped = true;
                    current.push(ch);
                }
                c if c == string_char => {
                    current.push(ch);
                    in_string = false;
                }
                _ => current.push(ch),
            }
            continue;
        }
        match ch {
            '"' | '\'' => {
                in_string = true;
                string_char = ch;
                current.push(ch);
            }
            ',' => {
                tokens.push(current.trim().to_string());
                current = String::new();
            }
            _ => current.push(ch),
        }
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        tokens.push(trimmed);
    }
    tokens
}

/// Find the first `=` that is not inside a quoted string and not part of `==`.
///
/// Returns the byte index of the `=`, or `None` if not found.
///
/// Note: this scanner intentionally does **not** track paren depth.  A bare `=`
/// inside parentheses (e.g. a function argument default) is still found — callers
/// rely on this to detect default-value separators in `@define` parameter lists.
fn find_unquoted_equals(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut result: Option<usize> = None;
    scan_bytes(bytes, false, |i, ch, _depth| {
        // Single `=` not followed by `=`
        if ch == b'=' && (i + 1 >= bytes.len() || bytes[i + 1] != b'=') {
            result = Some(i);
            return true; // stop at first match
        }
        false
    });
    result
}

/// Parse a `@define` parameter list string into a `Vec<Param>`.
///
/// Supports:
/// - `name` — required parameter
/// - `name = value` — parameter with default value (any literal `Expr`:
///   string, number, boolean, or null; non-literal defaults are rejected)
///
/// Enforces:
/// - Required parameters must come before optional (defaulted) parameters
/// - No duplicate parameter names
pub(super) fn parse_define_params(params_str: &str, fn_name: &str) -> Result<Vec<Param>, MdsError> {
    let raw_tokens = split_on_unquoted_commas(params_str);
    let mut params: Vec<Param> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut seen_default = false;

    for token in &raw_tokens {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }

        let param = if let Some(eq_pos) = find_unquoted_equals(token) {
            let lhs = token[..eq_pos].trim();
            let rhs = token[eq_pos + 1..].trim();

            if !is_valid_identifier(lhs) {
                return Err(MdsError::syntax(format!("invalid parameter name: '{lhs}'")));
            }
            let default_val = parse_cond_value(rhs).map_err(|_| {
                MdsError::syntax(format!(
                    "invalid default value for parameter '{lhs}': '{rhs}' — must be a string, number, boolean, or null"
                ))
            })?;
            seen_default = true;
            Param {
                name: lhs.to_string(),
                default: Some(default_val),
            }
        } else {
            // Required parameter
            if !is_valid_identifier(token) {
                return Err(MdsError::syntax(format!(
                    "invalid parameter name: '{token}'"
                )));
            }
            if seen_default {
                return Err(MdsError::syntax(format!(
                    "required parameter '{token}' cannot follow an optional parameter in @define {fn_name}"
                )));
            }
            Param {
                name: token.to_string(),
                default: None,
            }
        };

        if !seen.insert(param.name.clone()) {
            return Err(MdsError::syntax(format!(
                "duplicate parameter name '{}' in @define {fn_name}",
                param.name
            )));
        }
        params.push(param);
    }

    Ok(params)
}

/// Single-pass unescape for string literals.
///
/// Recognises `\\`, `\"`, and `\'` escape sequences. A backslash followed
/// by any other character is kept verbatim (both the backslash and the
/// character), matching the least-surprise principle for a template language.
pub(super) fn unescape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('"') => out.push('"'),
                Some('\'') => out.push('\''),
                Some('\\') | None => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Return true if `directive` is exactly `keyword` or starts with `keyword`
/// followed by a space, tab, or `{`.
pub(super) fn is_directive_token(directive: &str, keyword: &str) -> bool {
    directive == keyword
        || directive
            .strip_prefix(keyword)
            .is_some_and(|rest| matches!(rest.chars().next(), Some(' ' | '\t' | '{')))
}

/// Returns `true` if `s` is a valid MDS identifier.
///
/// An identifier must start with an ASCII letter or `_` and contain only
/// ASCII alphanumeric characters or `_`.
pub(crate) fn is_valid_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Strip a leading newline from the body text nodes.
pub(super) fn strip_leading_newline(mut nodes: Vec<Node>) -> Vec<Node> {
    if let Some(Node::Text(t)) = nodes.first_mut() {
        if let Some(stripped) = t
            .text
            .strip_prefix("\r\n")
            .or_else(|| t.text.strip_prefix('\n'))
        {
            t.text = stripped.to_string();
        }
        if t.text.is_empty() {
            nodes.remove(0);
        }
    }
    nodes
}

/// Strip a trailing newline from the body text nodes.
pub(super) fn strip_trailing_newline(mut nodes: Vec<Node>) -> Vec<Node> {
    if let Some(Node::Text(t)) = nodes.last_mut() {
        if t.text.ends_with('\n') {
            t.text.pop();
            if t.text.ends_with('\r') {
                t.text.pop();
            }
        }
        if t.text.is_empty() {
            nodes.pop();
        }
    }
    nodes
}
