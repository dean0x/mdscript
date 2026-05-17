use crate::error::MdsError;

/// Token types produced by the lexer.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    Lexer::new(source, file).run()
}

/// Stateful lexer that converts MDS source text into a flat token stream.
///
/// `run()` is the only public entry point. Each `scan_*` method advances
/// `self.pos` past the content it consumes and pushes tokens to `self.tokens`.
struct Lexer<'a> {
    source: &'a str,
    file: &'a str,
    chars: Vec<char>,
    byte_offsets: Vec<usize>,
    pos: usize,
    tokens: Vec<Token>,
    /// Non-zero when inside a fenced code block; holds the opening backtick count.
    code_fence_backticks: usize,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str, file: &'a str) -> Self {
        let chars: Vec<char> = source.chars().collect();
        // Build a mapping from char index to byte offset for correct str slicing.
        let byte_offsets: Vec<usize> = source
            .char_indices()
            .map(|(byte_idx, _)| byte_idx)
            .collect();
        Self {
            source,
            file,
            chars,
            byte_offsets,
            pos: 0,
            tokens: Vec::new(),
            code_fence_backticks: 0,
        }
    }

    /// Convert a char-index position to a byte offset into `self.source`.
    fn byte_pos(&self, char_pos: usize) -> usize {
        if char_pos >= self.byte_offsets.len() {
            self.source.len()
        } else {
            self.byte_offsets[char_pos]
        }
    }

    /// Return true when `self.pos` is at the start of a line.
    fn is_line_start(&self) -> bool {
        self.pos == 0 || self.chars[self.pos - 1] == '\n'
    }

    /// Scan a frontmatter block starting at position 0.
    ///
    /// Precondition: the source starts with `---\n` or `---\r\n`.
    /// Advances `self.pos` past the closing `---` fence (including its newline).
    fn scan_frontmatter(&mut self) -> Result<(), MdsError> {
        self.tokens.push(Token::FrontmatterFence(0));
        self.pos = skip_newline(&self.chars, 3);

        let fm_start = self.pos;
        let mut found_close = false;

        while self.pos < self.chars.len() {
            let bp = self.byte_pos(self.pos);
            if is_line_start_chars(&self.chars, self.pos) && self.source[bp..].starts_with("---") {
                let end_pos = self.pos + 3;
                let at_end = end_pos >= self.chars.len()
                    || self.chars[end_pos] == '\n'
                    || self.chars[end_pos] == '\r';
                if at_end {
                    // Strip \r for Windows line endings (\r\n) in frontmatter
                    let content: String = self.chars[fm_start..self.pos]
                        .iter()
                        .filter(|&&c| c != '\r')
                        .collect();
                    let fm_byte_offset = self.byte_pos(fm_start);
                    self.tokens
                        .push(Token::FrontmatterContent(content, fm_byte_offset));
                    self.tokens
                        .push(Token::FrontmatterFence(self.byte_pos(self.pos)));
                    self.pos = skip_newline(&self.chars, end_pos);
                    found_close = true;
                    break;
                }
            }
            self.pos += 1;
        }

        if !found_close {
            return Err(MdsError::syntax_at(
                "unclosed frontmatter (missing closing ---)",
                self.file,
                self.source,
                0,
                3,
            ));
        }
        Ok(())
    }

    /// Scan a code fence (opening or closing ` ``` `).
    ///
    /// Returns `true` if a fence was processed and the caller should `continue`
    /// the main dispatch loop. Returns `false` if this position is inside a code
    /// block with fewer backticks than needed — the caller falls through to
    /// `scan_code_content`.
    fn scan_code_fence(&mut self) -> bool {
        let bp = self.byte_pos(self.pos);
        let (backtick_count, rest_is_close) = scan_fence(&self.chars, self.pos);

        if self.code_fence_backticks == 0 {
            // Opening fence — record the backtick count
            let fence_start = bp;
            self.pos += backtick_count;
            let mut fence = "`".repeat(backtick_count);
            // consume any remaining language tag characters
            while self.pos < self.chars.len()
                && self.chars[self.pos] != '\n'
                && self.chars[self.pos] != '\r'
            {
                fence.push(self.chars[self.pos]);
                self.pos += 1;
            }
            self.pos = skip_newline(&self.chars, self.pos);
            self.code_fence_backticks = backtick_count;
            self.tokens.push(Token::CodeFence(fence, fence_start));
            true
        } else if rest_is_close && backtick_count >= self.code_fence_backticks {
            // Closing fence — must have >= opening backtick count, no non-space suffix
            let fence_start = bp;
            self.pos += backtick_count;
            let fence = "`".repeat(backtick_count);
            self.pos = skip_newline(&self.chars, self.pos);
            self.code_fence_backticks = 0;
            self.tokens.push(Token::CodeFence(fence, fence_start));
            true
        } else {
            // Fewer backticks inside block — falls through to CodeContent
            false
        }
    }

    /// Scan raw content inside a code block (no interpolation).
    ///
    /// Precondition: `self.code_fence_backticks > 0`.
    /// Advances `self.pos` up to (but not past) the closing fence.
    fn scan_code_content(&mut self) {
        let start = self.byte_pos(self.pos);
        let mut content = String::new();
        while self.pos < self.chars.len() {
            let bp = self.byte_pos(self.pos);
            if is_line_start_chars(&self.chars, self.pos) && self.source[bp..].starts_with("```") {
                let (bc, is_close) = scan_fence(&self.chars, self.pos);
                if is_close && bc >= self.code_fence_backticks {
                    break;
                }
            }
            content.push(self.chars[self.pos]);
            self.pos += 1;
        }
        if !content.is_empty() {
            self.tokens.push(Token::CodeContent(content, start));
        }
    }

    /// Scan an `@` directive at line start.
    ///
    /// Precondition: `self.is_line_start()` and `self.chars[self.pos] == '@'`.
    /// Advances `self.pos` past the directive line including its newline.
    fn scan_directive(&mut self) {
        let start = self.byte_pos(self.pos);
        let mut line = String::new();
        while self.pos < self.chars.len() && self.chars[self.pos] != '\n' {
            line.push(self.chars[self.pos]);
            self.pos += 1;
        }
        // consume newline
        if self.pos < self.chars.len() && self.chars[self.pos] == '\n' {
            self.pos += 1;
        }
        // Strip trailing \r for Windows line endings
        let line = line.trim_end_matches('\r').to_string();
        self.tokens.push(Token::Directive(line, start));
    }

    /// Scan a `\{` or `\}` escape sequence.
    ///
    /// Returns `true` and advances `self.pos` by 2 if an escape was found;
    /// returns `false` otherwise (caller continues to next check).
    fn scan_escape(&mut self) -> bool {
        if self.pos + 1 >= self.chars.len() || self.chars[self.pos] != '\\' {
            return false;
        }
        let next = self.chars[self.pos + 1];
        if next == '{' {
            self.tokens
                .push(Token::EscapedBrace(self.byte_pos(self.pos)));
            self.pos += 2;
            true
        } else if next == '}' {
            self.tokens
                .push(Token::Text("}".to_string(), self.byte_pos(self.pos)));
            self.pos += 2;
            true
        } else {
            false
        }
    }

    /// Scan a `{...}` interpolation with brace depth tracking.
    ///
    /// Precondition: `self.chars[self.pos] == '{'`.
    /// Advances `self.pos` past the closing `}`.
    fn scan_interpolation(&mut self) -> Result<(), MdsError> {
        let start = self.byte_pos(self.pos);
        self.pos += 1; // skip opening {
        let mut depth = 1usize;
        let mut content = String::new();
        while self.pos < self.chars.len() && depth > 0 {
            match self.chars[self.pos] {
                '{' => {
                    depth += 1;
                    content.push('{');
                }
                '}' => {
                    depth -= 1;
                    if depth > 0 {
                        content.push('}');
                    }
                }
                c => {
                    content.push(c);
                }
            }
            self.pos += 1;
        }
        if depth != 0 {
            return Err(MdsError::syntax_at(
                "unclosed interpolation brace",
                self.file,
                self.source,
                start,
                1,
            ));
        }
        self.tokens
            .push(Token::Interpolation(content.trim().to_string(), start));
        Ok(())
    }

    /// Scan regular text, stopping at any special character that begins another token.
    ///
    /// Advances `self.pos` past the accumulated text content.
    fn scan_text(&mut self) {
        let start = self.byte_pos(self.pos);
        let mut text = String::new();
        while self.pos < self.chars.len() {
            let c = self.chars[self.pos];
            // Stop at interpolation
            if c == '{' {
                break;
            }
            // Stop at escape sequences
            if c == '\\' && self.pos + 1 < self.chars.len() {
                let next = self.chars[self.pos + 1];
                if next == '{' || next == '}' {
                    break;
                }
            }
            // Stop at directive or code fence at line start
            if self.is_line_start() {
                if c == '@' {
                    break;
                }
                let bp = self.byte_pos(self.pos);
                if self.source[bp..].starts_with("```") {
                    break;
                }
            }
            text.push(c);
            self.pos += 1;
        }
        if !text.is_empty() {
            self.tokens.push(Token::Text(text, start));
        }
    }

    /// Main dispatch loop — drives the lexer to completion and returns all tokens.
    fn run(mut self) -> Result<Vec<Token>, MdsError> {
        // Check for frontmatter at the very start
        if self.source.starts_with("---\n") || self.source.starts_with("---\r\n") {
            self.scan_frontmatter()?;
        }

        while self.pos < self.chars.len() {
            let at_line_start = self.is_line_start();
            let bp = self.byte_pos(self.pos);

            // Code fence (opening or closing).
            // `scan_code_fence` returns true when it consumed the fence; false means
            // we are inside a block with fewer backticks and fall through to CodeContent.
            if at_line_start && self.source[bp..].starts_with("```") && self.scan_code_fence() {
                continue;
            }

            // Inside a code block: raw content only
            if self.code_fence_backticks > 0 {
                self.scan_code_content();
                continue;
            }

            // @ directive at line start
            if at_line_start && self.chars[self.pos] == '@' {
                self.scan_directive();
                continue;
            }

            // Escape sequences: `\{` and `\}`
            if self.scan_escape() {
                continue;
            }

            // Interpolation `{...}`
            if self.chars[self.pos] == '{' {
                self.scan_interpolation()?;
                continue;
            }

            // Regular text
            self.scan_text();
        }

        // Check for unclosed code block
        if self.code_fence_backticks > 0 {
            return Err(MdsError::syntax("unclosed code fence"));
        }

        Ok(self.tokens)
    }
}

/// Check if the given char position is at the start of a line,
/// using the chars array for safe multi-byte character handling.
fn is_line_start_chars(chars: &[char], pos: usize) -> bool {
    pos == 0 || chars[pos - 1] == '\n'
}

/// Count consecutive backticks starting at `pos` and determine whether the
/// rest of the line (after the backticks) contains only optional whitespace.
///
/// Returns `(count, is_close_candidate)` where `is_close_candidate` is true
/// when nothing follows the backticks except spaces/tabs before EOL or EOF.
fn scan_fence(chars: &[char], pos: usize) -> (usize, bool) {
    let count = chars[pos..].iter().take_while(|&&c| c == '`').count();
    let is_close = chars[pos + count..]
        .iter()
        .take_while(|&&c| c != '\n' && c != '\r')
        .all(|&c| c == ' ' || c == '\t');
    (count, is_close)
}

/// Advance `pos` past a line ending (`\n`, `\r\n`, or bare `\r`), if present.
fn skip_newline(chars: &[char], mut pos: usize) -> usize {
    if pos < chars.len() && chars[pos] == '\r' {
        pos += 1;
    }
    if pos < chars.len() && chars[pos] == '\n' {
        pos += 1;
    }
    pos
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
        assert!(tokens
            .iter()
            .any(|t| matches!(t, Token::FrontmatterContent(_, _))));
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
        assert!(!tokens
            .iter()
            .any(|t| { matches!(t, Token::Interpolation(s, _) if s == "no_interp") }));
    }

    #[test]
    fn tokenize_directive() {
        let src = "@if premium:\nContent\n@end\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        assert!(tokens
            .iter()
            .any(|t| matches!(t, Token::Directive(d, _) if d.starts_with("@if"))));
    }
}
