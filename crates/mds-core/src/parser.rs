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
    Condition, DefineBlock, Expr, ForBlock, Frontmatter, IfBlock, IncludeDirective, Module, Node,
    TextNode,
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
        file,
        source,
    };
    parser.parse_module()
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    depth: usize,
    file: &'a str,
    source: &'a str,
}

/// Build the appropriate parse error when a directive's trailing `:` is missing.
///
/// Produces a targeted "unterminated string literal" message when the input contains
/// an unclosed quote, or a generic "must end with ':'" message otherwise.
fn directive_colon_error(directive: &str, rest: &str) -> MdsError {
    if has_unterminated_string(rest) {
        MdsError::syntax(format!(
            "unterminated string literal in {directive} condition"
        ))
    } else {
        MdsError::syntax(format!("{directive} directive must end with ':'"))
    }
}

impl Parser<'_> {
    fn parse_module(&mut self) -> Result<Module, MdsError> {
        let frontmatter = self.parse_frontmatter();
        let body = self.parse_body(&[], &[])?;
        Ok(Module { frontmatter, body })
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

    /// Consume the closing `@end` token, returning an error if absent or wrong.
    fn consume_end(&mut self, block_name: &str) -> Result<(), MdsError> {
        match self.tokens.get(self.pos) {
            Some(Token::Directive(d, _)) if d.trim() == "@end" => {
                self.pos += 1;
                Ok(())
            }
            Some(Token::Directive(d, _)) => Err(MdsError::syntax(format!(
                "expected @end to close {block_name} block, got '{}'",
                d.trim()
            ))),
            _ => Err(MdsError::syntax(format!(
                "unclosed {block_name} block (missing @end)"
            ))),
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
                Token::Text(text, _offset) => {
                    nodes.push(Node::Text(TextNode { text: text.clone() }));
                    self.pos += 1;
                }
                Token::Interpolation(expr, offset) => {
                    let interp = parse_interpolation_expr(expr, *offset, self.file, self.source)?;
                    nodes.push(Node::Interpolation(interp));
                    self.pos += 1;
                }
                Token::EscapedBrace(_) => {
                    nodes.push(Node::EscapedBrace);
                    self.pos += 1;
                }
                Token::CodeFence(fence, _offset) => {
                    nodes.push(Node::Text(TextNode {
                        text: format!("{fence}\n"),
                    }));
                    self.pos += 1;
                }
                Token::CodeContent(content, _offset) => {
                    nodes.push(Node::Text(TextNode {
                        text: content.clone(),
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
            return parse_export_directive(trimmed);
        }
        if let Some(rest) = trimmed.strip_prefix("@include ") {
            let alias = rest.trim().to_string();
            if !is_valid_identifier(&alias) {
                return Err(MdsError::syntax(format!(
                    "invalid include alias: '{alias}'"
                )));
            }
            return Ok(Node::Include(IncludeDirective { alias, offset }));
        }

        // Give a targeted hint if the user wrote @else without the required colon
        if trimmed == "@else" {
            return Err(MdsError::syntax(
                "found '@else' without colon — use '@else:' (with trailing colon)",
            ));
        }

        // Give targeted hints for @elseif used outside an @if block
        if trimmed.starts_with("@elseif ") || trimmed == "@elseif" {
            return Err(MdsError::syntax("@elseif must appear inside an @if block"));
        }

        // Give a targeted hint for @elseif: (missing condition after the colon)
        if trimmed.starts_with("@elseif:") {
            return Err(MdsError::syntax(
                "found '@elseif:' without a condition — use '@elseif <condition>:' (condition required)",
            ));
        }

        Err(MdsError::syntax(format!(
            "unknown directive: {trimmed}. Valid directives: @if, @elseif, @else:, @end, @for, @define, @import, @export, @include"
        )))
    }

    fn parse_if_block(&mut self, rest: &str, offset: usize) -> Result<Node, MdsError> {
        self.enter_block()?;

        let trimmed = rest.trim();
        let condition_str = strip_trailing_directive_colon(trimmed)
            .ok_or_else(|| directive_colon_error("@if", trimmed))?;

        let condition = parse_condition(condition_str)?;

        // Parse then-body; stops at @else:, @end, or any @elseif prefix
        let then_body = self.parse_body(&["@else:", "@end"], &["@elseif "])?;

        let elseif_branches = self.collect_elseif_branches()?;

        let else_body = if matches!(self.peek(), Some(Token::Directive(d, _)) if d.trim() == "@else:")
        {
            self.pos += 1; // skip @else:
            Some(self.parse_body(&["@end"], &[])?)
        } else {
            None
        };

        self.consume_end("@if")?;

        self.depth -= 1;
        Ok(Node::If(IfBlock {
            condition,
            elseif_branches,
            then_body,
            else_body,
            offset,
        }))
    }

    /// Consume all consecutive `@elseif` directive tokens and return the parsed branches.
    ///
    /// The limit check runs **before** parsing each branch body so that adversarial
    /// input that exceeds `MAX_ELSEIF_BRANCHES` cannot force unbounded parse work.
    fn collect_elseif_branches(&mut self) -> Result<Vec<(Condition, Vec<Node>)>, MdsError> {
        let mut branches: Vec<(Condition, Vec<Node>)> = Vec::with_capacity(4);
        while let Some(Token::Directive(d, _)) = self.peek() {
            if !d.trim().starts_with("@elseif ") {
                break;
            }

            // Enforce the branch limit before doing any parse work for this iteration.
            if branches.len() >= MAX_ELSEIF_BRANCHES {
                return Err(MdsError::syntax(format!(
                    "@if block has more than {MAX_ELSEIF_BRANCHES} @elseif branches"
                )));
            }

            // Consume the @elseif directive token.
            let elseif_dir = d.clone();
            self.pos += 1;

            // Extract condition string: strip "@elseif " prefix and trailing ":".
            // strip_prefix cannot fail here: the while guard already checked starts_with("@elseif ").
            let elseif_rest = elseif_dir
                .trim()
                .strip_prefix("@elseif ")
                .expect("loop guard guarantees @elseif prefix")
                .trim();
            let elseif_cond_str = strip_trailing_directive_colon(elseif_rest)
                .ok_or_else(|| directive_colon_error("@elseif", elseif_rest))?;

            let elseif_cond = parse_condition(elseif_cond_str)?;
            let elseif_body = self.parse_body(&["@else:", "@end"], &["@elseif "])?;

            branches.push((elseif_cond, elseif_body));
        }
        Ok(branches)
    }

    fn parse_for_block(&mut self, rest: &str, offset: usize) -> Result<Node, MdsError> {
        self.enter_block()?;

        let trimmed = rest.trim();
        let rest = strip_trailing_directive_colon(trimmed)
            .ok_or_else(|| directive_colon_error("@for", trimmed))?;

        // Split on " in " to separate variable part from iterable part.
        // Supports both:
        //   @for item in iterable:
        //   @for key, value in iterable:
        let in_idx = rest.find(" in ").ok_or_else(|| {
            MdsError::syntax("@for must follow pattern: @for <var> in <iterable>:")
        })?;
        let var_part = rest[..in_idx].trim();
        let iterable_str = rest[in_idx + 4..].trim();

        let (key_var, var) = parse_for_vars(var_part)?;

        // Parse iterable as a full expression (variable, dot-path, function call, etc.)
        let iterable = parse_expr_inner(iterable_str)?;
        // Reject bare literals as iterables: @for x in "str": makes no sense.
        if matches!(
            iterable,
            Expr::StringLiteral(_)
                | Expr::NumberLiteral(_)
                | Expr::BooleanLiteral(_)
                | Expr::NullLiteral
        ) {
            return Err(MdsError::syntax(format!(
                "cannot use a literal value as @for iterable: '{iterable_str}' — use a variable or function call"
            )));
        }

        let body = self.parse_body(&["@end"], &[])?;

        self.consume_end("@for")?;

        self.depth -= 1;
        Ok(Node::For(ForBlock {
            var,
            key_var,
            iterable,
            body,
            offset,
        }))
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
            .ok_or_else(|| MdsError::syntax("@define directive must end with ':'"))?
            .trim();

        // Parse "name(params)"
        let paren_start = rest.find('(').ok_or_else(|| {
            MdsError::syntax("@define must have parameter list: @define name(params):")
        })?;
        let paren_end = rest
            .find(')')
            .ok_or_else(|| MdsError::syntax("@define: unclosed parenthesis"))?;

        let name = rest[..paren_start].trim().to_string();

        if !is_valid_identifier(&name) {
            return Err(MdsError::syntax(format!("invalid function name: '{name}'")));
        }

        let params_str = &rest[paren_start + 1..paren_end];
        let params = parse_define_params(params_str, &name)?;

        let body = self.parse_body(&["@end"], &[])?;

        // Trim surrounding newlines added by the block's colons and @end lines.
        let body = strip_trailing_newline(strip_leading_newline(body));

        self.consume_end("@define")?;

        self.depth -= 1;
        Ok(Node::Define(DefineBlock {
            name,
            params,
            body,
            offset,
        }))
    }
}
