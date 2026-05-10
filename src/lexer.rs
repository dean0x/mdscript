use crate::error::MdsError;

/// Token types produced by the lexer.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    /// Raw text content.
    Text(String, usize),
    /// An interpolation expression `{...}` — inner content without braces.
    Interpolation(String, usize),
    /// An escaped brace `\{`.
    EscapedBrace(usize),
    /// A directive line starting with `@`.
    Directive(String, usize),
    /// Frontmatter opening/closing `---`.
    FrontmatterFence(usize),
    /// Frontmatter content between `---` fences.
    FrontmatterContent(String, usize),
    /// A fenced code block opening/closing ``` marker.
    CodeFence(String, usize),
    /// Raw content inside a fenced code block (no interpolation).
    CodeContent(String, usize),
}

/// Tokenize MDS source text into tokens.
pub fn tokenize(source: &str, file: &str) -> Result<Vec<Token>, MdsError> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = source.chars().collect();
    let mut pos = 0;
    let mut in_code_block = false;
    let mut code_fence_prefix = String::new();

    // Check for frontmatter at the very start
    if source.starts_with("---\n") || source.starts_with("---\r\n") {
        tokens.push(Token::FrontmatterFence(0));
        pos = 3;
        // skip the newline after ---
        if pos < chars.len() && chars[pos] == '\n' {
            pos += 1;
        } else if pos < chars.len() && chars[pos] == '\r' {
            pos += 1;
            if pos < chars.len() && chars[pos] == '\n' {
                pos += 1;
            }
        }

        // Find closing ---
        let fm_start = pos;
        let mut found_close = false;
        while pos < chars.len() {
            // Check if current position starts a line with ---
            if is_line_start(source, pos) && source[pos..].starts_with("---") {
                let end_pos = pos + 3;
                let at_end = end_pos >= chars.len()
                    || chars[end_pos] == '\n'
                    || chars[end_pos] == '\r';
                if at_end {
                    let content: String = chars[fm_start..pos].iter().collect();
                    tokens.push(Token::FrontmatterContent(content, fm_start));
                    tokens.push(Token::FrontmatterFence(pos));
                    pos = end_pos;
                    // skip newline after closing ---
                    if pos < chars.len() && chars[pos] == '\n' {
                        pos += 1;
                    } else if pos < chars.len() && chars[pos] == '\r' {
                        pos += 1;
                        if pos < chars.len() && chars[pos] == '\n' {
                            pos += 1;
                        }
                    }
                    found_close = true;
                    break;
                }
            }
            pos += 1;
        }
        if !found_close {
            return Err(MdsError::syntax_at(
                "unclosed frontmatter (missing closing ---)",
                file,
                source,
                0,
                3,
            ));
        }
    }

    // Process rest of the file
    while pos < chars.len() {
        // Check for code fences (```)
        if is_line_start(source, pos) && source[pos..].starts_with("```") {
            let fence_start = pos;
            pos += 3;
            // consume any remaining backticks and language tag
            let mut fence = String::from("```");
            while pos < chars.len() && chars[pos] != '\n' && chars[pos] != '\r' {
                fence.push(chars[pos]);
                pos += 1;
            }
            // consume newline
            if pos < chars.len() && chars[pos] == '\r' {
                pos += 1;
            }
            if pos < chars.len() && chars[pos] == '\n' {
                pos += 1;
            }

            if !in_code_block {
                in_code_block = true;
                code_fence_prefix = fence.clone();
                tokens.push(Token::CodeFence(fence, fence_start));
            } else {
                in_code_block = false;
                tokens.push(Token::CodeFence(fence, fence_start));
                code_fence_prefix.clear();
            }
            continue;
        }

        if in_code_block {
            // Inside code block: no interpolation, just raw content
            let start = pos;
            let mut content = String::new();
            while pos < chars.len() {
                // Check for closing code fence
                if is_line_start(source, pos) && source[pos..].starts_with("```") {
                    break;
                }
                content.push(chars[pos]);
                pos += 1;
            }
            if !content.is_empty() {
                tokens.push(Token::CodeContent(content, start));
            }
            continue;
        }

        // Check for @ directives at line start
        if is_line_start(source, pos) && pos < chars.len() && chars[pos] == '@' {
            let start = pos;
            let mut line = String::new();
            while pos < chars.len() && chars[pos] != '\n' {
                line.push(chars[pos]);
                pos += 1;
            }
            // consume newline
            if pos < chars.len() && chars[pos] == '\n' {
                pos += 1;
            }
            tokens.push(Token::Directive(line, start));
            continue;
        }

        // Check for escaped brace
        if pos + 1 < chars.len() && chars[pos] == '\\' && chars[pos + 1] == '{' {
            tokens.push(Token::EscapedBrace(pos));
            pos += 2;
            continue;
        }

        // Check for interpolation
        if chars[pos] == '{' {
            let start = pos;
            pos += 1; // skip {
            let mut depth = 1;
            let mut content = String::new();
            while pos < chars.len() && depth > 0 {
                if chars[pos] == '{' {
                    depth += 1;
                    content.push(chars[pos]);
                } else if chars[pos] == '}' {
                    depth -= 1;
                    if depth > 0 {
                        content.push(chars[pos]);
                    }
                } else {
                    content.push(chars[pos]);
                }
                pos += 1;
            }
            if depth != 0 {
                return Err(MdsError::syntax_at(
                    "unclosed interpolation brace",
                    file,
                    source,
                    start,
                    1,
                ));
            }
            tokens.push(Token::Interpolation(content.trim().to_string(), start));
            continue;
        }

        // Regular text
        let start = pos;
        let mut text = String::new();
        while pos < chars.len() {
            // Stop at interpolation, escaped brace, directive start, or code fence
            if chars[pos] == '{' {
                break;
            }
            if pos + 1 < chars.len() && chars[pos] == '\\' && chars[pos + 1] == '{' {
                break;
            }
            if is_line_start(source, pos) && pos < chars.len() && chars[pos] == '@' {
                break;
            }
            if is_line_start(source, pos) && source[pos..].starts_with("```") {
                break;
            }
            text.push(chars[pos]);
            pos += 1;
        }
        if !text.is_empty() {
            tokens.push(Token::Text(text, start));
        }
    }

    // Check for unclosed code block
    if in_code_block {
        return Err(MdsError::syntax("unclosed code fence"));
    }

    Ok(tokens)
}

/// Check if the given position is at the start of a line.
fn is_line_start(source: &str, pos: usize) -> bool {
    if pos == 0 {
        return true;
    }
    let bytes = source.as_bytes();
    pos <= bytes.len() && bytes[pos - 1] == b'\n'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_simple_text() {
        let tokens = tokenize("Hello world!", "test.mds").unwrap();
        assert_eq!(tokens.len(), 1);
        match &tokens[0] {
            Token::Text(t, _) => assert_eq!(t, "Hello world!"),
            _ => panic!("expected Text token"),
        }
    }

    #[test]
    fn tokenize_interpolation() {
        let tokens = tokenize("Hello {name}!", "test.mds").unwrap();
        assert_eq!(tokens.len(), 3);
        match &tokens[1] {
            Token::Interpolation(expr, _) => assert_eq!(expr, "name"),
            _ => panic!("expected Interpolation token"),
        }
    }

    #[test]
    fn tokenize_frontmatter() {
        let src = "---\nname: Alice\n---\nHello!";
        let tokens = tokenize(src, "test.mds").unwrap();
        assert!(tokens.iter().any(|t| matches!(t, Token::FrontmatterContent(_, _))));
    }

    #[test]
    fn tokenize_escaped_brace() {
        let tokens = tokenize("Hello \\{name}!", "test.mds").unwrap();
        assert!(tokens.iter().any(|t| matches!(t, Token::EscapedBrace(_))));
    }

    #[test]
    fn tokenize_code_block_passthrough() {
        let src = "text\n```\n{no_interp}\n```\nmore";
        let tokens = tokenize(src, "test.mds").unwrap();
        // The {no_interp} should be CodeContent, not Interpolation
        assert!(tokens.iter().any(|t| matches!(t, Token::CodeContent(_, _))));
        assert!(!tokens.iter().any(|t| {
            matches!(t, Token::Interpolation(s, _) if s == "no_interp")
        }));
    }

    #[test]
    fn tokenize_directive() {
        let src = "@if premium:\nContent\n@end\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        assert!(tokens.iter().any(|t| matches!(t, Token::Directive(d, _) if d.starts_with("@if"))));
    }
}
