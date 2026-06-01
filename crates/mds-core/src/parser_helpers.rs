use crate::ast::{
    Arg, CondValue, Condition, ExportDirective, Expr, ImportDirective, Interpolation, Node,
};
use crate::error::MdsError;
use crate::limits::{MAX_DOT_SEGMENTS, MAX_NESTING_DEPTH};

/// Parse a dot-separated path string (e.g. `"config.debug"`) into a `Vec<String>`.
///
/// Returns an error if any segment is not a valid identifier or if the path
/// exceeds `MAX_DOT_SEGMENTS`.
pub(super) fn parse_dot_path(s: &str) -> Result<Vec<String>, MdsError> {
    let mut out: Vec<String> = Vec::with_capacity(4);
    for raw in s.split('.') {
        if out.len() >= MAX_DOT_SEGMENTS {
            return Err(MdsError::syntax(format!(
                "@if condition dot path exceeds maximum segment count of {MAX_DOT_SEGMENTS}"
            )));
        }
        let part = raw.trim();
        if !is_valid_identifier(part) {
            return Err(MdsError::syntax(format!(
                "@if condition must be a variable name or dot path, got '{s}'"
            )));
        }
        out.push(part.to_string());
    }
    Ok(out)
}

/// Parse a literal value for the RHS of a comparison condition.
///
/// Accepts:
/// - Quoted strings: `"admin"` or `'admin'`
/// - Numbers: `42`, `-5`, `3.14`
/// - Booleans: `true`, `false`
/// - Null: `null`
pub(super) fn parse_cond_value(s: &str) -> Result<CondValue, MdsError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(MdsError::syntax(
            "comparison values must be string, number, boolean, or null",
        ));
    }

    // Quoted string (single or double)
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        let inner = &s[1..s.len() - 1];
        return Ok(CondValue::String(unescape_string(inner)));
    }

    // Unterminated string
    if s.starts_with('"') || s.starts_with('\'') {
        return Err(MdsError::syntax(
            "unterminated string literal in @if condition",
        ));
    }

    // Boolean literals
    if s == "true" {
        return Ok(CondValue::Boolean(true));
    }
    if s == "false" {
        return Ok(CondValue::Boolean(false));
    }

    // Null literal
    if s == "null" {
        return Ok(CondValue::Null);
    }

    // Numeric (integer or float, including negative)
    if let Ok(n) = s.parse::<f64>() {
        if !n.is_finite() {
            return Err(MdsError::syntax(
                "NaN and infinity are not valid condition values",
            ));
        }
        return Ok(CondValue::Number(n));
    }

    Err(MdsError::syntax(
        "comparison values must be string, number, boolean, or null",
    ))
}

/// Scan `s` for the first unquoted `==` or `!=` operator.
///
/// Tracks whether the scanner is inside a single- or double-quoted string and
/// only reports an operator position when outside any string literal. This
/// handles cases like `@if msg == "a == b":` correctly (the `==` inside the
/// string literal is ignored).
///
/// Returns `Some((byte_index, "=="` or `"!="))` pointing to the start of the
/// operator, or `None` if no unquoted operator is present.
///
/// # Byte-level scan safety
///
/// This function receives a `&str`, which Rust guarantees is valid UTF-8.  The
/// ASCII bytes we search for (`!`, `=`, `"`, `'`, `\`) are all single-byte
/// characters in the range 0x00–0x7F.  In UTF-8, continuation bytes of
/// multi-byte sequences always have their high bit set (≥ 0x80), so they can
/// never be mistaken for any ASCII byte we inspect.  Consequently, scanning
/// `s.as_bytes()` byte-by-byte and acting only on those ASCII sentinel values
/// is sound: we never split a multi-byte code-point, and every byte index we
/// return points to the first byte of an ASCII character that is also a valid
/// `str` boundary.
pub(super) fn find_unquoted_operator(s: &str) -> Option<(usize, &'static str)> {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_string = false;
    let mut string_char = b'"';

    while i < len {
        let ch = bytes[i];

        if in_string {
            // Check escape before close-quote: a backslash always consumes the
            // next character, so the close-quote check must never run for the
            // escaped character.
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

        // Outside a string
        if ch == b'"' || ch == b'\'' {
            in_string = true;
            string_char = ch;
            i += 1;
            continue;
        }

        // Check for != (must check before single = check)
        if ch == b'!' && i + 1 < len && bytes[i + 1] == b'=' {
            return Some((i, "!="));
        }

        // Check for == (two consecutive '=', not just one)
        if ch == b'=' && i + 1 < len && bytes[i + 1] == b'=' {
            return Some((i, "=="));
        }

        i += 1;
    }

    None
}

/// Parse the body of a negation condition (everything after the leading `!`).
///
/// Validates that the negation is not doubled (`!!`), not combined with a
/// comparison operator, and not missing the variable name.  Returns
/// `Condition::Not(path)` on success.
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
    let path = parse_dot_path(rest)?;
    Ok(Condition::Not(path))
}

/// Parse an `@if` or `@elseif` condition string into a `Condition`.
///
/// Accepted forms:
/// - `var` / `config.debug` → `Condition::Truthy`
/// - `!var` / `!config.debug` → `Condition::Not`
/// - `var == "value"` / `var != 42` → `Condition::Eq` / `Condition::NotEq`
pub(super) fn parse_condition(s: &str) -> Result<Condition, MdsError> {
    let s = s.trim();

    // Negation prefix
    if let Some(rest) = s.strip_prefix('!') {
        return parse_negation_condition(rest);
    }

    // Equality/inequality operators
    if let Some((op_pos, op)) = find_unquoted_operator(s) {
        let lhs = s[..op_pos].trim();
        let rhs_start = op_pos + op.len();
        let rhs = s[rhs_start..].trim();

        if rhs.is_empty() {
            return Err(MdsError::syntax(format!("expected value after '{op}'")));
        }

        let path = parse_dot_path(lhs)?;
        let value = parse_cond_value(rhs)?;

        return match op {
            "==" => Ok(Condition::Eq(path, value)),
            "!=" => Ok(Condition::NotEq(path, value)),
            other => Err(MdsError::syntax(format!(
                "internal error: unrecognised operator '{other}' in @if condition"
            ))),
        };
    }

    // Check for bare `=` (not `==`) — give a targeted hint
    // We look for `=` that is NOT followed by another `=` and NOT preceded by `!`
    if let Some(eq_pos) = s.find('=') {
        let before = &s[..eq_pos];
        let after = &s[eq_pos + 1..];
        // Bare `=` (not `==` and not `!=`)
        if !after.starts_with('=') && !before.ends_with('!') {
            return Err(MdsError::syntax("use '==' for comparison, not '='"));
        }
    }

    // Default: truthy check
    let path = parse_dot_path(s)?;
    Ok(Condition::Truthy(path))
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

/// Parse the expression inside `{ }` into an Expr.
///
/// Dispatches across four expression types by examining the positions of the
/// first `.` and first `(`:
///   dot before `(`  → [`parse_dot_expr`] (QualifiedCall or MemberAccess)
///   `(` without dot → Call
///   neither         → Var
pub(super) fn parse_interpolation_expr(
    content: &str,
    offset: usize,
    file: &str,
    source: &str,
) -> Result<Interpolation, MdsError> {
    let content = content.trim();
    let len = content.len();

    // Dot before any `(`: QualifiedCall or MemberAccess.
    let first_dot = content.find('.');
    let first_paren = content.find('(');
    if let (Some(dot_pos), paren_opt) = (first_dot, first_paren) {
        if paren_opt.is_none_or(|p| dot_pos < p) {
            return parse_dot_expr(content, dot_pos, offset, len, file, source);
        }
    }

    // Paren without prior dot: simple Call.
    if let Some(paren_pos) = first_paren {
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

#[cfg(test)]
pub(super) fn parse_single_arg(s: &str) -> Result<Arg, MdsError> {
    parse_single_arg_inner(s, 0)
}

pub(super) fn parse_single_arg_inner(s: &str, depth: usize) -> Result<Arg, MdsError> {
    let s = s.trim();
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
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
