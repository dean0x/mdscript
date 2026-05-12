use crate::ast::*;
use crate::error::MdsError;
use crate::lexer::Token;

/// Parse a stream of tokens into a Module AST.
pub fn parse(tokens: &[Token]) -> Result<Module, MdsError> {
    let mut parser = Parser { tokens, pos: 0 };
    parser.parse_module()
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
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

    /// Consume the closing `@end` token, returning an error if absent or wrong.
    fn consume_end(&mut self, block_name: &str) -> Result<(), MdsError> {
        if self.pos >= self.tokens.len() {
            return Err(MdsError::syntax(format!(
                "unclosed {block_name} block (missing @end)"
            )));
        }
        match &self.tokens[self.pos] {
            Token::Directive(d, _) if d.trim() == "@end" => {
                self.pos += 1;
                Ok(())
            }
            Token::Directive(d, _) => Err(MdsError::syntax(format!(
                "expected @end to close {block_name} block, got '{}'",
                d.trim()
            ))),
            _ => Err(MdsError::syntax(format!(
                "expected @end to close {block_name} block"
            ))),
        }
    }

    fn parse_frontmatter(&mut self) -> Option<Frontmatter> {
        if !matches!(self.peek(), Some(Token::FrontmatterFence(_))) {
            return None;
        }
        self.pos += 1; // skip opening fence

        let fm = if let Some(Token::FrontmatterContent(content, offset)) = self.peek() {
            let fm = Frontmatter {
                raw: content.clone(),
                offset: *offset,
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
                    // Check if this is a terminator
                    for term in terminators {
                        if trimmed == *term {
                            return Ok(nodes);
                        }
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
                    let interp = parse_interpolation_expr(expr, *offset)?;
                    nodes.push(Node::Interpolation(interp));
                    self.pos += 1;
                }
                Token::EscapedBrace(_) => {
                    nodes.push(Node::EscapedBrace);
                    self.pos += 1;
                }
                Token::CodeFence(fence, offset) => {
                    // Emit code fence as text
                    let mut code_text = fence.clone();
                    code_text.push('\n');
                    nodes.push(Node::Text(TextNode {
                        text: code_text,
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
        if trimmed.starts_with("@import") {
            return parse_import_directive(trimmed, offset);
        }
        if trimmed.starts_with("@export") {
            return parse_export_directive(trimmed, offset);
        }
        if let Some(rest) = trimmed.strip_prefix("@include ") {
            let alias = rest.trim().to_string();
            return Ok(Node::Include(IncludeDirective { alias, offset }));
        }

        Err(MdsError::syntax(format!(
            "unknown directive: {trimmed}. Valid directives: @if, @else, @end, @for, @define, @import, @export, @include"
        )))
    }

    fn parse_if_block(&mut self, rest: &str, offset: usize) -> Result<Node, MdsError> {
        let condition = rest
            .trim()
            .strip_suffix(':')
            .ok_or_else(|| MdsError::syntax("@if directive must end with ':'"))?
            .trim()
            .to_string();

        let then_body = self.parse_body(&["@else:", "@end"])?;

        let else_body = if self.pos < self.tokens.len() {
            if let Token::Directive(d, _) = &self.tokens[self.pos] {
                if d.trim() == "@else:" {
                    self.pos += 1; // skip @else:
                    let body = self.parse_body(&["@end"])?;
                    Some(body)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        self.consume_end("@if")?;

        Ok(Node::If(IfBlock {
            condition,
            then_body,
            else_body,
            offset,
        }))
    }

    fn parse_for_block(&mut self, rest: &str, offset: usize) -> Result<Node, MdsError> {
        let rest = rest.trim();
        let rest = rest
            .strip_suffix(':')
            .ok_or_else(|| MdsError::syntax("@for directive must end with ':'"))?
            .trim();

        // Parse "item in items"
        let parts: Vec<&str> = rest.splitn(3, ' ').collect();
        if parts.len() != 3 || parts[1] != "in" {
            return Err(MdsError::syntax(
                "@for must follow pattern: @for <var> in <iterable>:",
            ));
        }
        let var = parts[0].to_string();
        let iterable = parts[2].trim().to_string();

        let body = self.parse_body(&["@end"])?;

        self.consume_end("@for")?;

        Ok(Node::For(ForBlock {
            var,
            iterable,
            body,
            offset,
        }))
    }

    fn parse_define_block(&mut self, rest: &str, offset: usize) -> Result<Node, MdsError> {
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
        let params_str = &rest[paren_start + 1..paren_end];
        let params: Vec<String> = params_str
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();

        let body = self.parse_body(&["@end"])?;

        // Trim surrounding newlines added by the block's colons and @end lines.
        let body = strip_trailing_newline(strip_leading_newline(body));

        self.consume_end("@define")?;

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

        let after = rest[brace_end + 1..].trim();
        let path_part = after
            .strip_prefix("from")
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
    // parse_quoted_path succeeded, so quotes must exist — but use safe fallback
    let after_path_start = rest
        .find('"')
        .ok_or_else(|| MdsError::syntax("missing opening quote in import path"))?;
    let after_path_end = rest[after_path_start + 1..]
        .find('"')
        .ok_or_else(|| MdsError::syntax("missing closing quote in import path"))?
        + after_path_start
        + 2;
    let after = rest[after_path_end..].trim();

    if let Some(alias) = after.strip_prefix("as ") {
        Ok(Node::Import(ImportDirective::Alias {
            path,
            alias: alias.trim().to_string(),
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
fn parse_export_directive(directive: &str, offset: usize) -> Result<Node, MdsError> {
    let rest = directive.trim_start_matches("@export").trim();

    // Wildcard re-export: @export * from "path"
    if rest.starts_with("* from ") || rest.starts_with("*from ") {
        let from_part = rest
            .strip_prefix("* from ")
            .or_else(|| rest.strip_prefix("*from "))
            .unwrap_or("");
        let path = parse_quoted_path(from_part.trim())?;
        return Ok(Node::Export(ExportDirective::Wildcard { path, offset }));
    }

    // Check for "name from" pattern: @export name from "path"
    let parts: Vec<&str> = rest.splitn(3, ' ').collect();
    if parts.len() >= 3 && parts[1] == "from" {
        let name = parts[0].to_string();
        let path = parse_quoted_path(parts[2])?;
        return Ok(Node::Export(ExportDirective::ReExport {
            name,
            path,
            offset,
        }));
    }

    // Named export: @export name
    let name = rest.trim().to_string();
    if name.is_empty() {
        return Err(MdsError::syntax("@export requires a name"));
    }
    Ok(Node::Export(ExportDirective::Named { name, offset }))
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

/// Parse the expression inside `{ }` into an Expr.
fn parse_interpolation_expr(content: &str, offset: usize) -> Result<Interpolation, MdsError> {
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
fn parse_args(args_str: &str) -> Result<Vec<Arg>, MdsError> {
    let args_str = args_str.trim();
    if args_str.is_empty() {
        return Ok(Vec::new());
    }

    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let mut string_char = '"';
    let mut escaped = false;

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
        } else if ch == ',' {
            args.push(parse_single_arg(current.trim())?);
            current.clear();
        } else {
            current.push(ch);
        }
    }

    let trimmed = current.trim();
    if !trimmed.is_empty() {
        args.push(parse_single_arg(trimmed)?);
    }

    Ok(args)
}

fn parse_single_arg(s: &str) -> Result<Arg, MdsError> {
    let s = s.trim();
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        let inner = &s[1..s.len() - 1];
        let unescaped = inner
            .replace("\\\"", "\"")
            .replace("\\'", "'")
            .replace("\\\\", "\\");
        Ok(Arg::StringLiteral(unescaped))
    } else if is_valid_identifier(s) {
        // Variable reference
        Ok(Arg::Var(s.to_string()))
    } else {
        Err(MdsError::syntax(format!(
            "invalid function argument: '{s}'"
        )))
    }
}

fn is_valid_identifier(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Strip a leading newline from the body text nodes.
fn strip_leading_newline(mut nodes: Vec<Node>) -> Vec<Node> {
    if let Some(Node::Text(t)) = nodes.first_mut() {
        if t.text.starts_with('\n') {
            t.text = t.text[1..].to_string();
        } else if t.text.starts_with("\r\n") {
            t.text = t.text[2..].to_string();
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
        let module = parse(&tokens).unwrap();
        assert!(module.frontmatter.is_none());
        assert_eq!(module.body.len(), 1);
    }

    #[test]
    fn parse_frontmatter() {
        let src = "---\nname: Alice\n---\nHello!";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse(&tokens).unwrap();
        assert!(module.frontmatter.is_some());
        assert!(module.frontmatter.unwrap().raw.contains("name: Alice"));
    }

    #[test]
    fn parse_if_block() {
        let src = "@if premium:\nPremium!\n@end\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse(&tokens).unwrap();
        assert!(matches!(module.body[0], Node::If(_)));
    }

    #[test]
    fn parse_if_else() {
        let src = "@if premium:\nPremium!\n@else:\nFree!\n@end\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse(&tokens).unwrap();
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
        let module = parse(&tokens).unwrap();
        assert!(matches!(module.body[0], Node::For(_)));
    }

    #[test]
    fn parse_define() {
        let src = "@define greet(name):\nHello {name}!\n@end\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse(&tokens).unwrap();
        assert!(matches!(module.body[0], Node::Define(_)));
    }

    #[test]
    fn parse_import_alias() {
        let src = "@import \"./utils.mds\" as utils\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse(&tokens).unwrap();
        assert!(matches!(
            module.body[0],
            Node::Import(ImportDirective::Alias { .. })
        ));
    }

    #[test]
    fn parse_import_merge() {
        let src = "@import \"./base.mds\"\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse(&tokens).unwrap();
        assert!(matches!(
            module.body[0],
            Node::Import(ImportDirective::Merge { .. })
        ));
    }

    #[test]
    fn parse_import_selective() {
        let src = "@import { greet, farewell } from \"./utils.mds\"\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse(&tokens).unwrap();
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
        let module = parse(&tokens).unwrap();
        assert!(matches!(
            module.body[0],
            Node::Export(ExportDirective::Named { .. })
        ));
    }

    #[test]
    fn parse_export_reexport() {
        let src = "@export greet from \"./greetings.mds\"\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse(&tokens).unwrap();
        assert!(matches!(
            module.body[0],
            Node::Export(ExportDirective::ReExport { .. })
        ));
    }

    #[test]
    fn parse_export_wildcard() {
        let src = "@export * from \"./formatting.mds\"\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse(&tokens).unwrap();
        assert!(matches!(
            module.body[0],
            Node::Export(ExportDirective::Wildcard { .. })
        ));
    }

    #[test]
    fn parse_include() {
        let src = "@include footer\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse(&tokens).unwrap();
        assert!(matches!(module.body[0], Node::Include(_)));
    }

    #[test]
    fn parse_function_call_interpolation() {
        let src = "{greet(\"Alice\")}";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse(&tokens).unwrap();
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
        let module = parse(&tokens).unwrap();
        if let Node::Interpolation(interp) = &module.body[0] {
            assert!(matches!(interp.expr, Expr::QualifiedCall { .. }));
        } else {
            panic!("expected Interpolation node with QualifiedCall");
        }
    }

    // Fix 1 & 2: parse_single_arg panic guard and escape handling
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

    // Fix 3: ASCII-only identifier validation
    #[test]
    fn is_valid_identifier_rejects_unicode() {
        assert!(!is_valid_identifier("café"), "unicode must be rejected");
        assert!(!is_valid_identifier("αβγ"), "greek letters must be rejected");
        assert!(is_valid_identifier("hello"), "ascii ident must be accepted");
        assert!(is_valid_identifier("_foo_42"), "underscored ident ok");
    }
}
