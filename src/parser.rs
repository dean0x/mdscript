use std::collections::HashSet;

use crate::ast::{
    Arg, DefineBlock, ExportDirective, Expr, ForBlock, Frontmatter, IfBlock, ImportDirective,
    IncludeDirective, Interpolation, Module, Node, TextNode,
};
use crate::error::MdsError;
use crate::lexer::Token;

/// Maximum nesting depth for @if/@for/@define blocks.
/// Prevents stack overflow from crafted inputs with thousands of nested blocks.
pub(crate) const MAX_NESTING_DEPTH: usize = 256;

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

impl Parser<'_> {
    fn parse_module(&mut self) -> Result<Module, MdsError> {
        let frontmatter = self.parse_frontmatter();
        let body = self.parse_body(&[])?;
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
    /// Terminators: @end, @else:
    fn parse_body(&mut self, terminators: &[&str]) -> Result<Vec<Node>, MdsError> {
        let mut nodes = Vec::new();

        while self.pos < self.tokens.len() {
            let token = &self.tokens[self.pos];

            match token {
                Token::Directive(dir, _offset) => {
                    let trimmed = dir.trim();
                    if terminators.contains(&trimmed) {
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
                    let interp =
                        parse_interpolation_expr(expr, *offset, self.file, self.source)?;
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
            return parse_export_directive(trimmed, offset);
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

        Err(MdsError::syntax(format!(
            "unknown directive: {trimmed}. Valid directives: @if, @else:, @end, @for, @define, @import, @export, @include"
        )))
    }

    fn parse_if_block(&mut self, rest: &str, offset: usize) -> Result<Node, MdsError> {
        self.enter_block()?;

        let condition = rest
            .trim()
            .strip_suffix(':')
            .ok_or_else(|| MdsError::syntax("@if directive must end with ':'"))?
            .trim()
            .to_string();

        if !is_valid_identifier(&condition) {
            return Err(MdsError::syntax(format!(
                "@if condition must be a variable name, got '{condition}' — negation and expressions are not supported in v0.1"
            )));
        }

        let then_body = self.parse_body(&["@else:", "@end"])?;

        let else_body = if matches!(self.peek(), Some(Token::Directive(d, _)) if d.trim() == "@else:")
        {
            self.pos += 1; // skip @else:
            Some(self.parse_body(&["@end"])?)
        } else {
            None
        };

        self.consume_end("@if")?;

        self.depth -= 1;
        Ok(Node::If(IfBlock {
            condition,
            then_body,
            else_body,
            offset,
        }))
    }

    fn parse_for_block(&mut self, rest: &str, offset: usize) -> Result<Node, MdsError> {
        self.enter_block()?;

        let rest = rest.trim();
        let rest = rest
            .strip_suffix(':')
            .ok_or_else(|| MdsError::syntax("@for directive must end with ':'"))?
            .trim();

        // Parse "item in items"
        let (var, iterable) = match rest.splitn(3, ' ').collect::<Vec<_>>().as_slice() {
            [v, "in", it] => (v.to_string(), it.trim().to_string()),
            _ => {
                return Err(MdsError::syntax(
                    "@for must follow pattern: @for <var> in <iterable>:",
                ))
            }
        };

        if !is_valid_identifier(&var) {
            return Err(MdsError::syntax(format!(
                "invalid loop variable name: '{var}'"
            )));
        }
        if !is_valid_identifier(&iterable) {
            return Err(MdsError::syntax(format!(
                "invalid iterable name: '{iterable}'"
            )));
        }

        let body = self.parse_body(&["@end"])?;

        self.consume_end("@for")?;

        self.depth -= 1;
        Ok(Node::For(ForBlock {
            var,
            iterable,
            body,
            offset,
        }))
    }

    fn parse_define_block(&mut self, rest: &str, offset: usize) -> Result<Node, MdsError> {
        self.enter_block()?;

        let rest = rest.trim();
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
        let params: Vec<String> = params_str
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();

        let mut seen = HashSet::new();
        for param in &params {
            if !is_valid_identifier(param) {
                return Err(MdsError::syntax(format!(
                    "invalid parameter name: '{param}'"
                )));
            }
            if !seen.insert(param.as_str()) {
                return Err(MdsError::syntax(format!(
                    "duplicate parameter name '{param}' in @define {name}"
                )));
            }
        }

        let body = self.parse_body(&["@end"])?;

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

/// Parse an `@import` directive into a Node.
fn parse_import_directive(directive: &str, offset: usize) -> Result<Node, MdsError> {
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
fn parse_export_directive(directive: &str, _offset: usize) -> Result<Node, MdsError> {
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
fn parse_quoted_path(s: &str) -> Result<String, MdsError> {
    let s = s.trim();
    if !s.starts_with('"') {
        return Err(MdsError::syntax(format!("expected quoted path, got: {s}")));
    }
    let end = s[1..]
        .find('"')
        .ok_or_else(|| MdsError::syntax("unclosed quote in path"))?;
    Ok(s[1..=end].to_string())
}

/// Build an actionable error for unsupported dot-notation variable access (e.g. `{alias.name}`).
///
/// Attaches source-location context when `file` and `source` are non-empty, falling back
/// to a plain syntax error otherwise. `offset` marks the opening `{` and `content_len` is
/// the length of the trimmed content between the braces (used to compute the interp span).
fn dot_notation_error(
    content: &str,
    namespace: &str,
    field: &str,
    file: &str,
    source: &str,
    offset: usize,
    content_len: usize,
) -> MdsError {
    let message = format!(
        "dot notation for variables is not supported in v0.1: '{content}'. \
         To call a function from an imported module use: {{{namespace}.{field}()}}",
    );
    // The interpolation token's offset points to the `{`; the span covers the
    // entire interpolation including the surrounding braces (content_len + 2 for `{` and `}`).
    let interp_len = if !source.is_empty() {
        source[offset..]
            .find('}')
            .map(|end| end + 1)
            .unwrap_or(content_len + 2)
    } else {
        content_len + 2
    };
    if !file.is_empty() && !source.is_empty() {
        MdsError::syntax_at(message, file, source, offset, interp_len)
    } else {
        MdsError::syntax(message)
    }
}

/// Parse the expression inside `{ }` into an Expr.
fn parse_interpolation_expr(
    content: &str,
    offset: usize,
    file: &str,
    source: &str,
) -> Result<Interpolation, MdsError> {
    let content = content.trim();
    let len = content.len();

    // Check for qualified call: namespace.name(args)
    if let Some(dot_pos) = content.find('.') {
        let rest_after_dot = &content[dot_pos + 1..];
        if let Some(paren_pos) = rest_after_dot.find('(') {
            let namespace = content[..dot_pos].trim().to_string();
            let name = rest_after_dot[..paren_pos].trim().to_string();
            let args_str = rest_after_dot[paren_pos + 1..]
                .trim()
                .strip_suffix(')')
                .ok_or_else(|| MdsError::syntax("unclosed parenthesis in function call"))?;
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
        // Dot found but no '(' follows — this looks like variable property access
        // (e.g. {alias.name}), which is not supported. Give a clear, actionable error.
        let namespace = content[..dot_pos].trim();
        let field = rest_after_dot.trim();
        return Err(dot_notation_error(content, namespace, field, file, source, offset, len));
    }

    // Check for function call: name(args)
    if let Some(paren_pos) = content.find('(') {
        let name = content[..paren_pos].trim().to_string();
        let args_str = content[paren_pos + 1..]
            .trim()
            .strip_suffix(')')
            .ok_or_else(|| MdsError::syntax("unclosed parenthesis in function call"))?;
        let args = parse_args(args_str)?;
        return Ok(Interpolation {
            expr: Expr::Call { name, args },
            offset,
            len,
        });
    }

    // Simple variable reference
    if !is_valid_identifier(content) {
        return Err(MdsError::syntax(format!(
            "invalid interpolation: '{content}' is not a valid expression. Use a variable name (letters, numbers, underscores), a function call like func(), or escape with \\{{{{ for literal braces."
        )));
    }
    Ok(Interpolation {
        expr: Expr::Var(content.to_string()),
        offset,
        len,
    })
}

/// Parse function call arguments.
/// Handles nested parentheses so that `inner("arg")` is kept as a single token.
fn parse_args(args_str: &str) -> Result<Vec<Arg>, MdsError> {
    parse_args_inner(args_str, 0)
}

fn parse_args_inner(args_str: &str, depth: usize) -> Result<Vec<Arg>, MdsError> {
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
        } else if in_string {
            if ch == '\\' {
                escaped = true;
                current.push(ch);
            } else if ch == string_char {
                current.push(ch);
                in_string = false;
            } else {
                current.push(ch);
            }
        } else if ch == '"' || ch == '\'' {
            in_string = true;
            string_char = ch;
            current.push(ch);
        } else if ch == '(' {
            paren_depth += 1;
            current.push(ch);
        } else if ch == ')' {
            // paren_depth should not reach 0 here since the outer call site
            // already stripped the closing ')' of the top-level call
            paren_depth = paren_depth.saturating_sub(1);
            current.push(ch);
        } else if ch == ',' && paren_depth == 0 {
            args.push(parse_single_arg_inner(current.trim(), depth)?);
            current.clear();
        } else {
            current.push(ch);
        }
    }

    let trimmed = current.trim();
    if !trimmed.is_empty() {
        args.push(parse_single_arg_inner(trimmed, depth)?);
    }

    Ok(args)
}

#[cfg(test)]
fn parse_single_arg(s: &str) -> Result<Arg, MdsError> {
    parse_single_arg_inner(s, 0)
}

fn parse_single_arg_inner(s: &str, depth: usize) -> Result<Arg, MdsError> {
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
fn unescape_string(s: &str) -> String {
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
fn is_directive_token(directive: &str, keyword: &str) -> bool {
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
fn strip_leading_newline(mut nodes: Vec<Node>) -> Vec<Node> {
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
fn strip_trailing_newline(mut nodes: Vec<Node>) -> Vec<Node> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;

    #[test]
    fn parse_simple_text() {
        let tokens = tokenize("Hello world!", "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "", "").unwrap();
        assert!(module.frontmatter.is_none());
        assert_eq!(module.body.len(), 1);
    }

    #[test]
    fn parse_frontmatter() {
        let src = "---\nname: Alice\n---\nHello!";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "", "").unwrap();
        assert!(module.frontmatter.is_some());
        assert!(module.frontmatter.unwrap().raw.contains("name: Alice"));
    }

    #[test]
    fn parse_if_block() {
        let src = "@if premium:\nPremium!\n@end\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "", "").unwrap();
        assert!(matches!(module.body[0], Node::If(_)));
    }

    #[test]
    fn parse_if_else() {
        let src = "@if premium:\nPremium!\n@else:\nFree!\n@end\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "", "").unwrap();
        if let Node::If(block) = &module.body[0] {
            assert!(block.else_body.is_some());
        } else {
            panic!("expected If node");
        }
    }

    #[test]
    fn parse_for_block() {
        let src = "@for item in items:\n- {item}\n@end\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "", "").unwrap();
        assert!(matches!(module.body[0], Node::For(_)));
    }

    #[test]
    fn parse_define() {
        let src = "@define greet(name):\nHello {name}!\n@end\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "", "").unwrap();
        assert!(matches!(module.body[0], Node::Define(_)));
    }

    #[test]
    fn parse_import_alias() {
        let src = "@import \"./utils.mds\" as utils\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "", "").unwrap();
        assert!(matches!(
            module.body[0],
            Node::Import(ImportDirective::Alias { .. })
        ));
    }

    #[test]
    fn parse_import_merge() {
        let src = "@import \"./base.mds\"\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "", "").unwrap();
        assert!(matches!(
            module.body[0],
            Node::Import(ImportDirective::Merge { .. })
        ));
    }

    #[test]
    fn parse_import_selective() {
        let src = "@import { greet, farewell } from \"./utils.mds\"\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "", "").unwrap();
        if let Node::Import(ImportDirective::Selective { names, .. }) = &module.body[0] {
            assert_eq!(names, &["greet", "farewell"]);
        } else {
            panic!("expected Selective import");
        }
    }

    #[test]
    fn parse_export_named() {
        let src = "@export greet\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "", "").unwrap();
        assert!(matches!(
            module.body[0],
            Node::Export(ExportDirective::Named { .. })
        ));
    }

    #[test]
    fn parse_export_reexport() {
        let src = "@export greet from \"./greetings.mds\"\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "", "").unwrap();
        assert!(matches!(
            module.body[0],
            Node::Export(ExportDirective::ReExport { .. })
        ));
    }

    #[test]
    fn parse_export_wildcard() {
        let src = "@export * from \"./formatting.mds\"\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "", "").unwrap();
        assert!(matches!(
            module.body[0],
            Node::Export(ExportDirective::Wildcard { .. })
        ));
    }

    #[test]
    fn parse_include() {
        let src = "@include footer\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "", "").unwrap();
        assert!(matches!(module.body[0], Node::Include(_)));
    }

    #[test]
    fn parse_function_call_interpolation() {
        let src = "{greet(\"Alice\")}";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "", "").unwrap();
        if let Node::Interpolation(interp) = &module.body[0] {
            assert!(matches!(interp.expr, Expr::Call { .. }));
        } else {
            panic!("expected Interpolation node");
        }
    }

    #[test]
    fn parse_qualified_call() {
        let src = "{utils.greet(\"Alice\")}";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "", "").unwrap();
        if let Node::Interpolation(interp) = &module.body[0] {
            assert!(matches!(interp.expr, Expr::QualifiedCall { .. }));
        } else {
            panic!("expected Interpolation node with QualifiedCall");
        }
    }

    #[test]
    fn parse_single_arg_lone_quote_returns_error() {
        // A lone `"` is not a valid string literal (len < 2) — must not panic
        let result = parse_single_arg("\"");
        assert!(result.is_err(), "lone quote should return Err, not panic");
    }

    #[test]
    fn parse_single_arg_escaped_quote_in_string() {
        // `"say \"hi\""` should parse to the string: say "hi"
        let result = parse_single_arg(r#""say \"hi\"""#);
        assert!(result.is_ok(), "escaped quote in string should parse ok");
        if let Ok(Arg::StringLiteral(s)) = result {
            assert_eq!(s, r#"say "hi""#);
        } else {
            panic!("expected StringLiteral");
        }
    }

    #[test]
    fn unescape_backslash_then_quote() {
        // `"a\\\"b"` inner content is `a\\\"b`:
        // \\  -> single backslash
        // \"  -> literal quote
        // Result: `a\"b` (backslash, quote, b)
        let result = parse_single_arg(r#""a\\\"b""#).unwrap();
        if let Arg::StringLiteral(s) = result {
            assert_eq!(s, "a\\\"b", "escaped backslash then escaped quote");
        } else {
            panic!("expected StringLiteral");
        }
    }

    #[test]
    fn unescape_double_backslash() {
        // `"a\\\\b"` inner content is `a\\\\b`:
        // \\  -> single backslash
        // \\  -> single backslash
        // Result: `a\\b`
        let result = parse_single_arg(r#""a\\\\b""#).unwrap();
        if let Arg::StringLiteral(s) = result {
            assert_eq!(s, "a\\\\b", "double escaped backslash");
        } else {
            panic!("expected StringLiteral");
        }
    }

    #[test]
    fn unescape_unknown_sequence_preserved() {
        // `"a\nb"` — `\n` is not a recognized escape, kept verbatim
        let result = parse_single_arg(r#""a\nb""#).unwrap();
        if let Arg::StringLiteral(s) = result {
            assert_eq!(s, "a\\nb", "unknown escape sequence kept verbatim");
        } else {
            panic!("expected StringLiteral");
        }
    }

    #[test]
    fn parse_args_escaped_comma_in_string() {
        // A comma inside a string arg must not split the arg
        let result = parse_args(r#""hello, world""#).unwrap();
        assert_eq!(result.len(), 1);
        if let Arg::StringLiteral(s) = &result[0] {
            assert_eq!(s, "hello, world");
        } else {
            panic!("expected StringLiteral");
        }
    }

    #[test]
    fn parse_nesting_depth_limit_rejected() {
        // Build a source string with MAX_NESTING_DEPTH + 1 nested @if blocks.
        // Each @if requires a condition variable — we use "x" consistently.
        let depth = MAX_NESTING_DEPTH + 1;
        let mut src = String::new();
        for _ in 0..depth {
            src.push_str("@if x:\n");
        }
        for _ in 0..depth {
            src.push_str("@end\n");
        }
        let tokens = tokenize(&src, "test.mds").unwrap();
        let result = parse_with_ctx(&tokens, "", "");
        assert!(
            result.is_err(),
            "nesting depth > MAX_NESTING_DEPTH must be rejected"
        );
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("nesting depth"),
            "error must mention nesting depth, got: {msg}"
        );
    }

    #[test]
    fn parse_nesting_depth_at_limit_accepted() {
        // Exactly MAX_NESTING_DEPTH nested @if blocks must succeed.
        let depth = MAX_NESTING_DEPTH;
        let mut src = String::new();
        for _ in 0..depth {
            src.push_str("@if x:\n");
        }
        for _ in 0..depth {
            src.push_str("@end\n");
        }
        let tokens = tokenize(&src, "test.mds").unwrap();
        let result = parse_with_ctx(&tokens, "", "");
        assert!(
            result.is_ok(),
            "nesting depth == MAX_NESTING_DEPTH must be accepted: {result:?}"
        );
    }

    #[test]
    fn is_valid_identifier_rejects_unicode() {
        assert!(!is_valid_identifier("café"), "unicode must be rejected");
        assert!(
            !is_valid_identifier("αβγ"),
            "greek letters must be rejected"
        );
        assert!(is_valid_identifier("hello"), "ascii ident must be accepted");
        assert!(is_valid_identifier("_foo_42"), "underscored ident ok");
    }
}
