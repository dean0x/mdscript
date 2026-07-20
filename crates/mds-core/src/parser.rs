//! MDS template parser.
//!
//! This module is split across three files to keep each unit manageable:
//!
//! - **`parser.rs`** (this file) — top-level [`parse_with_ctx`] entry point and the
//!   [`Parser`] state machine that walks the token stream and builds the AST.
//! - **`parser_helpers.rs`** — low-level parsing primitives (condition parsing,
//!   directive parsing, interpolation parsing, argument parsing, identifier
//!   validation, and related utilities).
//! - **`parser_tests.rs`** — integration and unit tests for both modules.

use crate::ast::{
    BlockNode, DefineBlock, ElseifBranch, Expr, ExtendsDirective, ForBlock, Frontmatter, IfBlock,
    IncludeDirective, MessageBlock, Module, Node, TextNode,
};
use crate::error::MdsError;
use crate::lexer::Token;
use crate::limits::{MAX_ELSEIF_BRANCHES, MAX_NESTING_DEPTH};

#[path = "parser_helpers.rs"]
mod helpers;
pub(crate) use helpers::is_valid_identifier;
use helpers::*;

#[cfg(test)]
#[path = "parser_tests.rs"]
mod tests;

/// Parse a stream of tokens into a Module AST with optional source context for error spans.
///
/// Pass non-empty `file` and `source` to enable source-location labels on parse errors.
/// When context is not available (e.g. unit tests), pass empty strings.
pub(crate) fn parse_with_ctx<'src>(
    tokens: &[Token],
    file: &'src str,
    source: &'src str,
) -> Result<Module, MdsError> {
    let mut parser = Parser {
        tokens,
        pos: 0,
        depth: 0,
        inside_message: false,
        inside_block: false,
        file,
        source,
    };
    parser.parse_module()
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    depth: usize,
    /// True when we are currently inside a `@message` block body.
    /// Used to detect and reject nested `@message` blocks.
    inside_message: bool,
    /// True when we are currently inside a `@block` body.
    /// Used to enforce that `@block` is top-level only (cannot nest inside itself or
    /// inside `@if`, `@for`, `@define`, or `@message`).
    inside_block: bool,
    file: &'a str,
    source: &'a str,
}

/// RAII guard that restores `inside_message` and decrements `depth` when dropped.
///
/// Created immediately after `enter_block()` and `inside_message = true` in
/// `parse_message_block`. All subsequent error paths — including `?` propagation —
/// will trigger Drop, so the invariant is structural rather than manually maintained.
struct MessageGuard<'p, 'a>(&'p mut Parser<'a>);

impl Drop for MessageGuard<'_, '_> {
    fn drop(&mut self) {
        // Invariant: depth was incremented by enter_block() before this guard was created.
        // A depth of 0 here would mean the decrement underflows — a compiler bug, not user input.
        debug_assert!(self.0.depth > 0, "MessageGuard::drop: depth underflow");
        self.0.inside_message = false;
        self.0.depth -= 1;
    }
}

/// RAII guard that restores `inside_block` and decrements `depth` when dropped.
///
/// Modeled on `MessageGuard`. Created immediately after `enter_block()` and
/// `inside_block = true` in `parse_block`. All `?` error paths trigger Drop,
/// keeping the invariant structural rather than manual.
struct BlockGuard<'p, 'a>(&'p mut Parser<'a>);

impl Drop for BlockGuard<'_, '_> {
    fn drop(&mut self) {
        debug_assert!(self.0.depth > 0, "BlockGuard::drop: depth underflow");
        self.0.inside_block = false;
        self.0.depth -= 1;
    }
}

/// Return the byte length of the line in `source` that starts at `offset`.
///
/// Used to bound the miette underline when anchoring directive errors. If `offset`
/// is out of bounds or lands on a non-char-boundary, returns 0 rather than panicking.
fn directive_line_len(source: &str, offset: usize) -> usize {
    if offset <= source.len() && source.is_char_boundary(offset) {
        source[offset..]
            .find('\n')
            .unwrap_or(source[offset..].len())
    } else {
        0
    }
}

impl Parser<'_> {
    fn parse_module(&mut self) -> Result<Module, MdsError> {
        let frontmatter = self.parse_frontmatter();
        // Consume an optional leading `@extends "path"` before parsing the body.
        // `parse_extends_if_present` will reject a stray @extends later in the body.
        let extends = self.parse_extends_if_present()?;
        let body = self.parse_body(&[], &[])?;
        Ok(Module {
            frontmatter,
            extends,
            body,
        })
    }

    /// Consume a leading `@extends "path"` directive if present.
    ///
    /// "Leading" means: before any non-blank-text node. Blank-line `Text` nodes
    /// (whitespace-only) are tolerated before `@extends`.
    ///
    /// If `@extends` is found, returns `Ok(Some(ExtendsDirective))` and advances
    /// `self.pos` past that token. If the next meaningful token is not `@extends`,
    /// returns `Ok(None)` without advancing. A later stray `@extends` is surfaced
    /// as an error in `parse_directive`.
    fn parse_extends_if_present(&mut self) -> Result<Option<ExtendsDirective>, MdsError> {
        // Peek ahead over any blank Text tokens.
        let mut scan = self.pos;
        loop {
            match self.tokens.get(scan) {
                Some(Token::Text(t, _)) if t.trim().is_empty() => scan += 1,
                Some(Token::Directive(d, offset)) => {
                    let trimmed = d.trim();
                    if trimmed == "@extends" || trimmed.starts_with("@extends ") {
                        // Consume the blank text tokens we scanned over.
                        self.pos = scan + 1;
                        let offset = *offset;
                        // Parse the path.
                        let rest = trimmed.strip_prefix("@extends").unwrap_or("").trim();
                        if rest.is_empty() {
                            return Err(self.syntax_at(
                                "@extends requires a quoted path: @extends \"./base.mds\"",
                                offset,
                            ));
                        }
                        let path = parse_quoted_path(rest)?;
                        return Ok(Some(ExtendsDirective { path, offset }));
                    }
                    break;
                }
                _ => break,
            }
        }
        Ok(None)
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn skip_if_frontmatter_fence(&mut self) {
        if matches!(self.peek(), Some(Token::FrontmatterFence(_))) {
            self.pos += 1;
        }
    }

    /// Increment nesting depth, returning an error if the limit is exceeded.
    fn enter_block(&mut self) -> Result<(), MdsError> {
        self.depth += 1;
        if self.depth > MAX_NESTING_DEPTH {
            return Err(MdsError::syntax(format!(
                "nesting depth exceeds maximum of {MAX_NESTING_DEPTH}"
            )));
        }
        Ok(())
    }

    /// Build a span-bearing syntax error anchored at `offset` in this parser's source.
    ///
    /// Replaces the 5-argument `MdsError::syntax_at(msg, self.file, self.source, offset,
    /// directive_line_len(self.source, offset))` pattern that appears throughout this file.
    fn syntax_at(&self, message: impl Into<String>, offset: usize) -> MdsError {
        MdsError::syntax_at(
            message,
            self.file,
            self.source,
            offset,
            directive_line_len(self.source, offset),
        )
    }

    /// Build a missing-colon error for `directive` with a source span at `offset`.
    ///
    /// Emits a targeted "unterminated string literal" message when `rest` contains an
    /// unclosed quote; otherwise emits the generic "must end with ':'" message (B2).
    fn colon_error(&self, directive: &str, rest: &str, offset: usize) -> MdsError {
        if has_unterminated_string(rest) {
            self.syntax_at(
                format!("unterminated string literal in {directive} condition"),
                offset,
            )
        } else {
            self.syntax_at(format!("{directive} directive must end with ':'"), offset)
        }
    }

    /// Consume the `@end` token that closes `block_name`, returning its byte offset on success.
    ///
    /// `opener_offset` is the byte offset of the OPENING directive token (e.g.
    /// the `@if` that this `@end` must close). When the block is never closed
    /// the error is anchored at the opener so the user sees the unclosed line
    /// rather than EOF.
    ///
    /// The returned `usize` is the byte offset of the `@end` token in the
    /// source — used by lint rules to compute multi-line removal spans.
    fn consume_end(&mut self, block_name: &str, opener_offset: usize) -> Result<usize, MdsError> {
        match self.tokens.get(self.pos) {
            Some(Token::Directive(d, end_off)) if d.trim() == "@end" => {
                let end_offset = *end_off;
                self.pos += 1;
                Ok(end_offset)
            }
            Some(Token::Directive(d, off)) => {
                let off = *off;
                Err(self.syntax_at(
                    format!(
                        "expected @end to close {block_name} block, got '{}'",
                        d.trim()
                    ),
                    off,
                ))
            }
            _ => Err(MdsError::syntax_at(
                format!("unclosed {block_name} block (missing @end)"),
                self.file,
                self.source,
                opener_offset,
                block_name.len(),
            )),
        }
    }

    fn parse_frontmatter(&mut self) -> Option<Frontmatter> {
        if !matches!(self.peek(), Some(Token::FrontmatterFence(_))) {
            return None;
        }
        self.pos += 1; // skip opening fence

        let fm = if let Some(Token::FrontmatterContent(content, _)) = self.peek() {
            let fm = Frontmatter {
                raw: content.clone(),
            };
            self.pos += 1; // skip content
            Some(fm)
        } else {
            None
        };

        self.skip_if_frontmatter_fence();
        fm
    }

    /// Parse body nodes until we hit a terminator directive or end of tokens.
    ///
    /// `exact_terminators` — directives that stop parsing when they exactly match (e.g. `"@end"`, `"@else:"`).
    /// `prefix_terminators` — stop when the directive *starts with* any of these prefixes (e.g. `"@elseif "`).
    ///
    /// Returns with `self.pos` pointing AT the terminator token (not past it).
    fn parse_body(
        &mut self,
        exact_terminators: &[&str],
        prefix_terminators: &[&str],
    ) -> Result<Vec<Node>, MdsError> {
        let mut nodes = Vec::new();

        while self.pos < self.tokens.len() {
            let token = &self.tokens[self.pos];

            match token {
                Token::Directive(dir, _offset) => {
                    let trimmed = dir.trim();
                    if exact_terminators.contains(&trimmed) {
                        return Ok(nodes);
                    }
                    if prefix_terminators.iter().any(|p| trimmed.starts_with(p)) {
                        return Ok(nodes);
                    }
                    let node = self.parse_directive()?;
                    nodes.push(node);
                }
                Token::Text(text, offset) => {
                    nodes.push(Node::Text(TextNode {
                        text: text.clone(),
                        offset: *offset,
                    }));
                    self.pos += 1;
                }
                Token::Interpolation(expr, offset) => {
                    let interp = parse_interpolation_expr(expr, *offset, self.file, self.source)?;
                    nodes.push(Node::Interpolation(interp));
                    self.pos += 1;
                }
                Token::EscapedBrace(offset) => {
                    nodes.push(Node::EscapedBrace { offset: *offset });
                    self.pos += 1;
                }
                Token::CodeFence(fence, offset) => {
                    nodes.push(Node::Text(TextNode {
                        text: format!("{fence}\n"),
                        offset: *offset,
                    }));
                    self.pos += 1;
                }
                Token::CodeContent(content, offset) => {
                    nodes.push(Node::Text(TextNode {
                        text: content.clone(),
                        offset: *offset,
                    }));
                    self.pos += 1;
                }
                Token::FrontmatterFence(_) | Token::FrontmatterContent(_, _) => {
                    // Should not appear here
                    self.pos += 1;
                }
            }
        }

        Ok(nodes)
    }

    fn parse_directive(&mut self) -> Result<Node, MdsError> {
        let (dir, offset) = match &self.tokens[self.pos] {
            Token::Directive(d, o) => (d.clone(), *o),
            _ => return Err(MdsError::syntax("expected directive")),
        };
        self.pos += 1;

        let trimmed = dir.trim();

        if let Some(rest) = trimmed.strip_prefix("@if ") {
            return self.parse_if_block(rest, offset);
        }
        if let Some(rest) = trimmed.strip_prefix("@for ") {
            return self.parse_for_block(rest, offset);
        }
        if let Some(rest) = trimmed.strip_prefix("@define ") {
            return self.parse_define_block(rest, offset);
        }
        if is_directive_token(trimmed, "@import") {
            return parse_import_directive(trimmed, offset);
        }
        if is_directive_token(trimmed, "@export") {
            return parse_export_directive(trimmed, offset);
        }
        if let Some(rest) = trimmed.strip_prefix("@include ") {
            let alias = rest.trim().to_string();
            if !is_valid_identifier(&alias) {
                return Err(self.syntax_at(format!("invalid include alias: '{alias}'"), offset));
            }
            return Ok(Node::Include(IncludeDirective { alias, offset }));
        }
        if let Some(rest) = trimmed.strip_prefix("@message ") {
            return self.parse_message_block(rest, offset);
        }
        if let Some(rest) = trimmed.strip_prefix("@block ") {
            return self.parse_block(rest, offset);
        }

        // Give a targeted hint if the user wrote @else without the required colon
        if trimmed == "@else" {
            return Err(self.syntax_at(
                "found '@else' without colon — use '@else:' (with trailing colon)",
                offset,
            ));
        }

        // Give targeted hints for @elseif used outside an @if block
        if trimmed.starts_with("@elseif ") || trimmed == "@elseif" {
            return Err(self.syntax_at("@elseif must appear inside an @if block", offset));
        }

        // Give a targeted hint for @elseif: (missing condition after the colon)
        if trimmed.starts_with("@elseif:") {
            return Err(self.syntax_at(
                "found '@elseif:' without a condition — use '@elseif <condition>:' (condition required)",
                offset,
            ));
        }

        // Reject a stray @extends that is not at the leading position (E1/E2).
        // parse_extends_if_present already consumed the leading @extends (if any);
        // any @extends that reaches parse_directive is by definition misplaced.
        if trimmed == "@extends" || trimmed.starts_with("@extends ") {
            return Err(MdsError::extends_error_at(
                "@extends must be the first directive after frontmatter — only one @extends is allowed and it must appear before any other content",
                self.file,
                self.source,
                offset,
                dir.len(),
            ));
        }

        Err(self.syntax_at(
            format!("unknown directive: {trimmed}. Valid directives: @if, @elseif, @else:, @end, @for, @define, @import, @export, @include, @message, @block, @extends"),
            offset,
        ))
    }

    fn parse_if_block(&mut self, rest: &str, offset: usize) -> Result<Node, MdsError> {
        self.enter_block()?;

        let trimmed = rest.trim();
        let condition_str = strip_trailing_directive_colon(trimmed)
            .ok_or_else(|| self.colon_error("@if", trimmed, offset))?;

        let condition = parse_condition(condition_str).map_err(|e| {
            e.or_span(
                self.file,
                self.source,
                offset,
                directive_line_len(self.source, offset),
            )
        })?;

        // Parse then-body; stops at @else:, @end, or any @elseif prefix
        let then_body = self.parse_body(&["@else:", "@end"], &["@elseif "])?;

        let elseif_branches = self.collect_elseif_branches()?;

        // Capture the @else: offset for accurate lint spans, then parse the body.
        let (else_body, else_offset) = if let Some(Token::Directive(d, else_off)) = self.peek() {
            if d.trim() == "@else:" {
                let captured_offset = *else_off;
                self.pos += 1; // skip @else:
                let body = self.parse_body(&["@end"], &[])?;
                (Some(body), Some(captured_offset))
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };

        let end_offset = self.consume_end("@if", offset)?;

        self.depth -= 1;
        Ok(Node::If(IfBlock {
            condition,
            elseif_branches,
            then_body,
            else_body,
            offset,
            else_offset,
            end_offset,
        }))
    }

    /// Consume all consecutive `@elseif` directive tokens and return the parsed branches.
    ///
    /// Each branch carries its byte offset (the position of the `@elseif` token) so
    /// lint rules can anchor diagnostics at the exact `@elseif` line (#181).
    ///
    /// The limit check runs **before** parsing each branch body so that adversarial
    /// input that exceeds `MAX_ELSEIF_BRANCHES` cannot force unbounded parse work.
    fn collect_elseif_branches(&mut self) -> Result<Vec<ElseifBranch>, MdsError> {
        let mut branches: Vec<ElseifBranch> = Vec::with_capacity(4);
        while let Some(Token::Directive(d, off)) = self.peek() {
            if !d.trim().starts_with("@elseif ") {
                break;
            }

            // d and off are borrowed from self.tokens[self.pos] via peek(); clone
            // before advancing pos so the borrows end before the mutable advance.
            // Capture elseif_offset BEFORE the limit check so the span is available.
            let elseif_dir = d.clone();
            let elseif_offset = *off;

            // Enforce the branch limit before doing any parse work for this iteration.
            if branches.len() >= MAX_ELSEIF_BRANCHES {
                return Err(self.syntax_at(
                    format!("@if block has more than {MAX_ELSEIF_BRANCHES} @elseif branches"),
                    elseif_offset,
                ));
            }

            self.pos += 1;

            // Extract condition string: strip "@elseif " prefix and trailing ":".
            // strip_prefix cannot fail here: the while guard already checked starts_with("@elseif ").
            let elseif_rest = elseif_dir
                .trim()
                .strip_prefix("@elseif ")
                .expect("loop guard guarantees @elseif prefix")
                .trim();
            let elseif_cond_str = strip_trailing_directive_colon(elseif_rest)
                .ok_or_else(|| self.colon_error("@elseif", elseif_rest, elseif_offset))?;

            let condition = parse_condition(elseif_cond_str).map_err(|e| {
                e.or_span(
                    self.file,
                    self.source,
                    elseif_offset,
                    directive_line_len(self.source, elseif_offset),
                )
            })?;
            let body = self.parse_body(&["@else:", "@end"], &["@elseif "])?;

            branches.push(ElseifBranch {
                condition,
                body,
                offset: elseif_offset,
            });
        }
        Ok(branches)
    }

    fn parse_for_block(&mut self, rest: &str, offset: usize) -> Result<Node, MdsError> {
        self.enter_block()?;

        let trimmed = rest.trim();
        let rest = strip_trailing_directive_colon(trimmed)
            .ok_or_else(|| self.colon_error("@for", trimmed, offset))?;

        // Split on " in " to separate variable part from iterable part.
        // Supports both:
        //   @for item in iterable:
        //   @for key, value in iterable:
        let in_idx = rest.find(" in ").ok_or_else(|| {
            self.syntax_at(
                "@for must follow pattern: @for <var> in <iterable>:",
                offset,
            )
        })?;
        let var_part = rest[..in_idx].trim();
        let iterable_str = rest[in_idx + 4..].trim();

        let (key_var, var) = parse_for_vars(var_part)?;

        // Parse iterable as a full expression (variable, dot-path, function call, etc.)
        let iterable = parse_expr_inner(iterable_str).map_err(|e| {
            e.or_span(
                self.file,
                self.source,
                offset,
                directive_line_len(self.source, offset),
            )
        })?;
        // Reject bare literals as iterables: @for x in "str": makes no sense.
        if matches!(
            iterable,
            Expr::StringLiteral(_)
                | Expr::NumberLiteral(_)
                | Expr::BooleanLiteral(_)
                | Expr::NullLiteral
        ) {
            return Err(self.syntax_at(
                format!("cannot use a literal value as @for iterable: '{iterable_str}' — use a variable or function call"),
                offset,
            ));
        }

        let body = self.parse_body(&["@end"], &[])?;

        let end_offset = self.consume_end("@for", offset)?;

        self.depth -= 1;
        Ok(Node::For(ForBlock {
            var,
            key_var,
            iterable,
            body,
            offset,
            end_offset,
        }))
    }

    /// Parse a `@message role:` ... `@end` block.
    ///
    /// Role parsing distinguishes two forms:
    /// - Bare word: `@message system:` → `Expr::StringLiteral("system")`.
    /// - Brace expression: `@message {role}:` → parsed via `parse_expr_inner`.
    ///
    /// Nested `@message` blocks are rejected (the `inside_message` flag tracks this).
    ///
    /// State invariant: `inside_message` and `depth` are set AFTER role parsing
    /// succeeds, so any `?` on role parsing does not leave them in an inconsistent
    /// state. Once the flags are set, a `MessageGuard` Drop implementation ensures
    /// both are restored on every exit path — including future early-return `?`s.
    fn parse_message_block(&mut self, rest: &str, offset: usize) -> Result<Node, MdsError> {
        if self.inside_message {
            return Err(self.syntax_at(
                "@message blocks cannot be nested inside another @message block",
                offset,
            ));
        }

        // Parse the role expression BEFORE mutating parser state so that a `?`
        // here leaves `inside_message` and `depth` untouched.
        let trimmed = rest.trim();
        let role_str = strip_trailing_directive_colon(trimmed)
            .ok_or_else(|| self.colon_error("@message", trimmed, offset))?;
        let role_trimmed = role_str.trim();

        let role = if role_trimmed.starts_with('{') && role_trimmed.ends_with('}') {
            // Dynamic role expression: @message {role_var}:
            let inner = role_trimmed[1..role_trimmed.len() - 1].trim();
            parse_expr_inner(inner) // safe: state not yet mutated
                .map_err(|e| {
                    e.or_span(
                        self.file,
                        self.source,
                        offset,
                        directive_line_len(self.source, offset),
                    )
                })?
        } else {
            // Bare-word role: @message system: → literal string "system"
            if role_trimmed.is_empty() {
                return Err(self.syntax_at(
                    "@message role must not be empty — use e.g. @message system:",
                    offset,
                ));
            }
            Expr::StringLiteral(role_trimmed.to_string())
        };

        // Role is valid — now commit to the block and set flags.
        // The guard restores both flags on every exit path (including `?`),
        // making the invariant structural rather than manually maintained.
        self.enter_block()?;
        self.inside_message = true;
        let _guard = MessageGuard(self);

        let body = _guard.0.parse_body(&["@end"], &[])?;

        let _end_offset = _guard.0.consume_end("@message", offset)?;

        // Guard drops here, restoring inside_message=false and depth-=1.
        Ok(Node::Message(MessageBlock { role, body, offset }))
    }

    /// Parse a `@block name:` ... `@end` node.
    ///
    /// `@block` is top-level only: rejected inside `@if`, `@for`, `@define`, `@message`,
    /// or another `@block`. The `inside_block` flag (and the `inside_message` flag)
    /// enforce this at parse time.
    ///
    /// State invariant: `inside_block` and `depth` are set AFTER name parsing succeeds,
    /// so any `?` on name parsing does not leave them in an inconsistent state.
    /// A `BlockGuard` Drop implementation ensures both are restored on every exit path.
    ///
    /// Block body bytes pass through verbatim (interior-verbatim whitespace contract,
    /// #150/#151): the directive lines (@block … :, @end) are consumed, everything
    /// else is left exactly as authored.
    fn parse_block(&mut self, rest: &str, offset: usize) -> Result<Node, MdsError> {
        // Reject @block inside other blocks (top-level only — decision #5).
        // E9: @block-nesting → mds::syntax (correct; not mds::extends — per error-code mapping).
        if self.inside_block {
            return Err(self.syntax_at("@block cannot be nested inside another @block", offset));
        }
        if self.inside_message {
            return Err(self.syntax_at("@block cannot be nested inside a @message block", offset));
        }
        // depth > 0 means we are inside @if, @for, or @define (top-level depth == 0).
        if self.depth > 0 {
            return Err(self.syntax_at(
                "@block is top-level only — it cannot appear inside @if, @for, or @define",
                offset,
            ));
        }

        // Parse the name BEFORE mutating parser state, so a `?` leaves flags untouched.
        let trimmed = rest.trim();
        let name_str = strip_trailing_directive_colon(trimmed)
            .ok_or_else(|| self.colon_error("@block", trimmed, offset))?;
        let name = name_str.trim().to_string();
        if name.is_empty() {
            return Err(self.syntax_at(
                "@block name must not be empty — use e.g. @block instructions:",
                offset,
            ));
        }
        if !is_valid_identifier(&name) {
            return Err(self.syntax_at(
                format!("invalid @block name: '{name}' — must be a valid identifier"),
                offset,
            ));
        }

        // Name is valid — now commit to the block and set flags.
        self.enter_block()?;
        self.inside_block = true;
        let _guard = BlockGuard(self);

        let body = _guard.0.parse_body(&["@end"], &[])?;

        let _end_offset = _guard.0.consume_end("@block", offset)?;

        // Guard drops here, restoring inside_block=false and depth-=1.
        Ok(Node::Block(BlockNode { name, body, offset }))
    }

    fn parse_define_block(&mut self, rest: &str, offset: usize) -> Result<Node, MdsError> {
        self.enter_block()?;

        let rest = rest.trim();
        // `strip_suffix(':')` is safe here (unlike @if/@for which use the quote+paren-aware
        // strip_trailing_directive_colon). @define's grammar enforces `name(params):` — the
        // entire parameter list is enclosed in parentheses, so any colon inside a default-value
        // string literal (e.g., `@define foo(sep = ":")`) is contained within the parens and
        // the final character of the directive is always the unambiguous directive colon.
        let rest = rest
            .strip_suffix(':')
            .ok_or_else(|| self.syntax_at("@define directive must end with ':'", offset))?
            .trim();

        // Parse "name(params)"
        let paren_start = rest.find('(').ok_or_else(|| {
            self.syntax_at(
                "@define must have parameter list: @define name(params):",
                offset,
            )
        })?;
        let paren_end = rest
            .find(')')
            .ok_or_else(|| self.syntax_at("@define: unclosed parenthesis", offset))?;

        let name = rest[..paren_start].trim().to_string();

        if !is_valid_identifier(&name) {
            return Err(self.syntax_at(format!("invalid function name: '{name}'"), offset));
        }

        let params_str = &rest[paren_start + 1..paren_end];
        let params = parse_define_params(params_str, &name)?;

        let body = self.parse_body(&["@end"], &[])?;

        let end_offset = self.consume_end("@define", offset)?;

        self.depth -= 1;
        Ok(Node::Define(DefineBlock {
            name,
            params,
            body,
            offset,
            end_offset,
        }))
    }
}
