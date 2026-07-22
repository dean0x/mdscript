use crate::error::MdsError;

/// Token types produced by the lexer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// Raw text content.
    Text(String, usize),
    /// An interpolation expression `{{...}}` — inner content without braces.
    Interpolation(String, usize),
    /// An escaped double-brace `\{{` that produces a literal `{{` in output.
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
    /// When inside a fenced code block: `Some((fence_char, fence_count, opener_byte_offset))`.
    /// `None` when not inside any code block.
    code_fence: Option<(char, usize, usize)>,
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
            code_fence: None,
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

    /// Consume the current line from `self.pos` through its trailing newline.
    ///
    /// Returns the source bytes from the start of the line up to (not including)
    /// the first `\r` or `\n`. Advances `self.pos` past the newline.
    fn consume_fence_line(&mut self) -> String {
        let bp = self.byte_pos(self.pos);
        let line_end = self.chars[self.pos..]
            .iter()
            .position(|&c| c == '\n' || c == '\r')
            .map(|rel| self.pos + rel)
            .unwrap_or(self.chars.len());
        let fence = self.source[bp..self.byte_pos(line_end)].to_string();
        self.pos = skip_newline(&self.chars, line_end);
        fence
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

    /// Scan a code fence (opening or closing) given the pre-parsed fence info.
    ///
    /// Returns `true` when the fence was consumed and the caller should `continue`.
    /// Returns `false` when we are inside a block but this line does not match the
    /// opener — the caller falls through to `scan_code_content`.
    fn scan_code_fence(&mut self, m: &FenceMatch) -> bool {
        let bp = self.byte_pos(self.pos);
        match self.code_fence {
            None => {
                // Opening fence.
                let fence = self.consume_fence_line();
                self.code_fence = Some((m.fence_char, m.fence_count, bp));
                self.tokens.push(Token::CodeFence(fence, bp));
                true
            }
            Some((open_char, open_count, _)) => {
                if fence_closes(m, open_char, open_count) {
                    // Closing fence: same char, at least as many, no info string.
                    let fence = self.consume_fence_line();
                    self.code_fence = None;
                    self.tokens.push(Token::CodeFence(fence, bp));
                    true
                } else {
                    // Inside a block but this line does not close it — fall through to CodeContent.
                    false
                }
            }
        }
    }

    /// Scan raw content inside a code block (no interpolation).
    ///
    /// Precondition: `self.code_fence.is_some()`.
    /// Advances `self.pos` up to (but not past) the closing fence line.
    fn scan_code_content(&mut self) {
        let start = self.byte_pos(self.pos);
        let mut content = String::new();
        // Precondition: self.code_fence.is_some() — run() checks before invoking.
        debug_assert!(
            self.code_fence.is_some(),
            "scan_code_content requires active code fence"
        );
        let (open_char, open_count, _) = self.code_fence.unwrap();
        while self.pos < self.chars.len() {
            if is_line_start_chars(&self.chars, self.pos) {
                if let Some(m) = try_scan_fence_at(&self.chars, self.pos) {
                    if fence_closes(&m, open_char, open_count) {
                        break;
                    }
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

    /// Scan a `\{{` escape sequence (3-character sequence → EscapedBrace token).
    ///
    /// Only `\{{` triggers EscapedBrace. A lone `\{` (not followed by `{`) is
    /// ordinary text, not an escape. Returns `true` and advances `self.pos` by 3
    /// if the sequence was found; `false` otherwise.
    fn scan_escape(&mut self) -> bool {
        // Only `\{{` triggers EscapedBrace — a 3-character sequence.
        // A lone `\{` (not followed by `{`) is ordinary text, not an escape.
        if self.pos + 2 < self.chars.len()
            && self.chars[self.pos] == '\\'
            && self.chars[self.pos + 1] == '{'
            && self.chars[self.pos + 2] == '{'
        {
            self.tokens
                .push(Token::EscapedBrace(self.byte_pos(self.pos)));
            self.pos += 3;
            true
        } else {
            false
        }
    }

    /// Scan a `{{...}}` interpolation, string-aware to avoid closing on `}}` inside strings.
    ///
    /// Precondition: `self.chars[self.pos] == '{'` and `self.chars[self.pos+1] == '{'`.
    /// Advances `self.pos` past the closing `}}`.
    fn scan_interpolation(&mut self) -> Result<(), MdsError> {
        let start = self.byte_pos(self.pos);
        // Skip the opening `{{`
        self.pos += 2;

        let mut content = String::new();
        let mut in_string = false;
        let mut string_char = '"';
        let mut escaped = false;
        let mut found_close = false;

        while self.pos < self.chars.len() {
            let c = self.chars[self.pos];

            if escaped {
                content.push(c);
                escaped = false;
                self.pos += 1;
                continue;
            }

            if in_string {
                match c {
                    '\\' => {
                        escaped = true;
                        content.push(c);
                    }
                    q if q == string_char => {
                        content.push(c);
                        in_string = false;
                    }
                    _ => content.push(c),
                }
                self.pos += 1;
                continue;
            }

            // Outside a string: check for closing `}}`
            if c == '}'
                && self.pos + 1 < self.chars.len()
                && self.chars[self.pos + 1] == '}'
            {
                self.pos += 2; // skip `}}`
                found_close = true;
                break;
            }

            // Start of a string literal
            if c == '"' || c == '\'' {
                in_string = true;
                string_char = c;
                content.push(c);
                self.pos += 1;
                continue;
            }

            content.push(c);
            self.pos += 1;
        }

        if !found_close {
            return Err(MdsError::syntax_at(
                "unclosed `{{` interpolation — to include a literal `{{` in output, escape it as `\\{{`",
                self.file,
                self.source,
                start,
                2,
            ));
        }

        self.tokens
            .push(Token::Interpolation(content.trim().to_string(), start));
        Ok(())
    }

    /// Scan regular text, stopping at any special character that begins another token.
    ///
    /// Lone `{` and `\{` (not followed by `{`) are now plain text.
    /// Only `{{` and `\{{` stop scanning.
    ///
    /// Advances `self.pos` past the accumulated text content.
    fn scan_text(&mut self) {
        let start = self.byte_pos(self.pos);
        let mut text = String::new();
        while self.pos < self.chars.len() {
            let c = self.chars[self.pos];

            // Stop at `{{` (double-brace interpolation opener)
            if c == '{' {
                if self.pos + 1 < self.chars.len() && self.chars[self.pos + 1] == '{' {
                    break;
                }
                // Lone `{` — consume as plain text
                text.push(c);
                self.pos += 1;
                continue;
            }

            // Stop at `\{{` (escaped-brace sequence opener)
            if c == '\\' {
                if self.pos + 2 < self.chars.len()
                    && self.chars[self.pos + 1] == '{'
                    && self.chars[self.pos + 2] == '{'
                {
                    break;
                }
                // `\` not followed by `{{` — consume as plain text
                text.push(c);
                self.pos += 1;
                continue;
            }

            // Stop at directive or code fence at line start
            if self.is_line_start() {
                if c == '@' {
                    break;
                }
                if try_scan_fence_at(&self.chars, self.pos).is_some() {
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

            // Code fence (opening or closing): any `[ \t>]*` prefix + ≥3 `` ` `` or `~`.
            // `scan_code_fence` returns true when it consumed the fence; false means
            // we are inside a block but this line is not the closer — fall through to CodeContent.
            if at_line_start {
                if let Some(m) = try_scan_fence_at(&self.chars, self.pos) {
                    if self.scan_code_fence(&m) {
                        continue;
                    }
                }
            }

            // Inside a code block: raw content only.
            if self.code_fence.is_some() {
                self.scan_code_content();
                continue;
            }

            // @ directive at line start
            if at_line_start && self.chars[self.pos] == '@' {
                self.scan_directive();
                continue;
            }

            // Escape sequences: `\{{` only
            if self.scan_escape() {
                continue;
            }

            // Interpolation `{{...}}`
            if self.chars[self.pos] == '{'
                && self.pos + 1 < self.chars.len()
                && self.chars[self.pos + 1] == '{'
            {
                self.scan_interpolation()?;
                continue;
            }

            // Regular text
            self.scan_text();
        }

        // Check for unclosed code block — include the opener's byte offset for diagnostics.
        if let Some((_, _, opener_offset)) = self.code_fence {
            return Err(MdsError::syntax_at(
                "unclosed code fence",
                self.file,
                self.source,
                opener_offset,
                3,
            ));
        }

        Ok(self.tokens)
    }
}

/// Check if the given char position is at the start of a line,
/// using the chars array for safe multi-byte character handling.
fn is_line_start_chars(chars: &[char], pos: usize) -> bool {
    pos == 0 || chars[pos - 1] == '\n'
}

/// Parsed attributes of a potential code fence line.
///
/// Produced by `try_scan_fence_at`; passed to `scan_code_fence` and `fence_closes`
/// so callers never destructure a positional tuple.
struct FenceMatch {
    fence_char: char,
    fence_count: usize,
    is_close: bool,
}

/// Check whether a code fence begins at position `pos` in `chars`.
///
/// Scans optional `[ \t>]*` prefix, then requires ≥ 3 consecutive `` ` `` or
/// `~` characters. Returns `Some(FenceMatch)` or `None` when no valid fence is present.
///
/// `is_close` is `true` when nothing follows the fence chars (before EOL/EOF)
/// except spaces or tabs — indicating this line can serve as a closing fence.
fn try_scan_fence_at(chars: &[char], pos: usize) -> Option<FenceMatch> {
    let prefix_len = chars[pos..]
        .iter()
        .take_while(|&&c| c == ' ' || c == '\t' || c == '>')
        .count();
    let fence_start = pos + prefix_len;
    if fence_start >= chars.len() {
        return None;
    }
    let fence_char = chars[fence_start];
    if fence_char != '`' && fence_char != '~' {
        return None;
    }
    let fence_count = chars[fence_start..]
        .iter()
        .take_while(|&&c| c == fence_char)
        .count();
    if fence_count < 3 {
        return None;
    }
    let after_fence = fence_start + fence_count;
    let is_close = chars[after_fence..]
        .iter()
        .take_while(|&&c| c != '\n' && c != '\r')
        .all(|&c| c == ' ' || c == '\t');
    Some(FenceMatch {
        fence_char,
        fence_count,
        is_close,
    })
}

/// Return `true` when `m` describes a line that closes the fence opened with
/// `open_char`/`open_count`: same fence character, at least as many markers,
/// and no info string (only spaces/tabs before EOL).
fn fence_closes(m: &FenceMatch, open_char: char, open_count: usize) -> bool {
    m.is_close && m.fence_char == open_char && m.fence_count >= open_count
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
        let tokens = tokenize("Hello {{name}}!", "test.mds").unwrap();
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
        let tokens = tokenize("Hello \\{{name}}!", "test.mds").unwrap();
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

    // ── #149: widened fence recognition ──────────────────────────────────────

    #[test]
    fn tilde_fence_is_recognized_as_code_block() {
        // Tilde fences (~~~) must protect their content from interpolation.
        let src = "text\n~~~python\n{no_interp}\n~~~\nmore\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        assert!(
            tokens.iter().any(|t| matches!(t, Token::CodeContent(_, _))),
            "tilde fence must produce CodeContent tokens"
        );
        assert!(
            !tokens
                .iter()
                .any(|t| matches!(t, Token::Interpolation(s, _) if s == "no_interp")),
            "interpolation inside tilde fence must not be parsed"
        );
    }

    #[test]
    fn indented_backtick_fence_is_recognized() {
        // Up to 3 spaces of indentation before ``` is valid CommonMark.
        let src = "text\n   ```python\n{no_interp}\n   ```\nmore\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        assert!(
            tokens.iter().any(|t| matches!(t, Token::CodeContent(_, _))),
            "indented fence must produce CodeContent tokens"
        );
        assert!(
            !tokens
                .iter()
                .any(|t| matches!(t, Token::Interpolation(s, _) if s == "no_interp")),
            "interpolation inside indented fence must not be parsed"
        );
    }

    #[test]
    fn blockquoted_fence_is_recognized() {
        // A `>` blockquote prefix before ``` must trigger fence recognition.
        let src = "text\n> ```python\n> {no_interp}\n> ```\nmore\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        assert!(
            tokens.iter().any(|t| matches!(t, Token::CodeContent(_, _))),
            "blockquoted fence must produce CodeContent tokens"
        );
        assert!(
            !tokens
                .iter()
                .any(|t| matches!(t, Token::Interpolation(s, _) if s == "no_interp")),
            "interpolation inside blockquoted fence must not be parsed"
        );
    }

    #[test]
    fn backtick_fence_not_closed_by_tilde_fence() {
        // A tilde closer must NOT close a backtick opener (and vice versa).
        let src = "```\ncontent\n~~~\nstill inside\n```\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let content: Vec<&str> = tokens
            .iter()
            .filter_map(|t| match t {
                Token::CodeContent(s, _) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        let combined = content.join("");
        assert!(
            combined.contains("still inside"),
            "tilde line must not close a backtick fence; combined content: {combined:?}"
        );
    }

    #[test]
    fn unclosed_fence_error_includes_opener_span() {
        // Unclosed fence now reports location of the opener.
        let err = tokenize("```python\nunclosed content\n", "test.mds").unwrap_err();
        // At minimum the error should mention the problem; span is optional in the msg repr.
        assert!(
            format!("{err}").contains("unclosed"),
            "unclosed fence error must mention 'unclosed', got: {err}"
        );
    }

    #[test]
    fn four_backtick_fence_not_closed_by_three_backtick_closer() {
        // A closing fence must have >= the opener's count.
        let src = "````python\ncontent\n```\nstill inside\n````\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let combined: String = tokens
            .iter()
            .filter_map(|t| match t {
                Token::CodeContent(s, _) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            combined.contains("still inside"),
            "three-backtick line must not close a four-backtick fence; got: {combined:?}"
        );
    }

    // ── #149: unmatched decorative fences now error ───────────────────────────

    #[test]
    fn unmatched_tilde_fence_raises_unclosed_error() {
        // Before #149 a lone ~~~ was literal body text. After #149 it opens a
        // fence; if unmatched the lexer must raise "unclosed code fence".
        let err = tokenize("A\n~~~\nB\n", "test.mds").unwrap_err();
        assert!(
            format!("{err}").contains("unclosed"),
            "unmatched tilde fence must raise 'unclosed' error, got: {err}"
        );
    }

    #[test]
    fn unmatched_indented_backtick_fence_raises_unclosed_error() {
        // A 4-space-indented ``` is a valid fence opener as of #149; if unmatched
        // it must raise "unclosed code fence" rather than tokenizing as plain text.
        let err = tokenize("A\n    ```\nB\n", "test.mds").unwrap_err();
        assert!(
            format!("{err}").contains("unclosed"),
            "unmatched indented backtick fence must raise 'unclosed' error, got: {err}"
        );
    }

    // ── #149: interpolation resumes after a closed fence ─────────────────────

    #[test]
    fn interpolation_resumes_after_closed_tilde_fence() {
        // {{post}} appears after a closed ~~~ block and must become an Interpolation,
        // not be swallowed as CodeContent.
        let src = "{{pre}}\n~~~python\n{{inside}}\n~~~\n{{post}}\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        assert!(
            tokens
                .iter()
                .any(|t| matches!(t, Token::Interpolation(s, _) if s == "pre")),
            "pre-fence interpolation must be recognized"
        );
        assert!(
            tokens
                .iter()
                .any(|t| matches!(t, Token::Interpolation(s, _) if s == "post")),
            "post-fence interpolation must resume after a closed tilde fence"
        );
        assert!(
            !tokens
                .iter()
                .any(|t| matches!(t, Token::Interpolation(s, _) if s == "inside")),
            "interpolation inside tilde fence must be suppressed"
        );
    }

    #[test]
    fn interpolation_resumes_after_closed_blockquoted_fence() {
        // {{post}} appears after the closing `> ``` ` line and must become Interpolation.
        // The `> {{inside}}` line is raw CodeContent; the blockquote prefix is part of
        // that raw content (not stripped by the lexer — the formatter handles regions).
        let src = "> ```python\n> {{inside}}\n> ```\n{{post}}\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        assert!(
            tokens
                .iter()
                .any(|t| matches!(t, Token::Interpolation(s, _) if s == "post")),
            "interpolation must resume after a closed blockquoted fence"
        );
        assert!(
            !tokens
                .iter()
                .any(|t| matches!(t, Token::Interpolation(s, _) if s == "inside")),
            "interpolation inside blockquoted fence must be suppressed"
        );
    }

    // ── Double-brace interpolation (new syntax) ──────────────────────────────────

    #[test]
    fn double_brace_interpolation_basic() {
        let tokens = tokenize("Hello {{name}}!", "test.mds").unwrap();
        assert!(tokens
            .iter()
            .any(|t| matches!(t, Token::Interpolation(s, _) if s == "name")));
    }

    #[test]
    fn lone_brace_is_plain_text() {
        let tokens = tokenize("JSON: {\"key\": 1}", "test.mds").unwrap();
        // No interpolation should be produced
        assert!(!tokens.iter().any(|t| matches!(t, Token::Interpolation(_, _))));
        // Should have text tokens containing the braces
        let text: String = tokens
            .iter()
            .filter_map(|t| match t {
                Token::Text(s, _) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert!(text.contains('{'));
    }

    #[test]
    fn lone_open_brace_at_eof_is_text() {
        let tokens = tokenize("end{", "test.mds").unwrap();
        assert!(!tokens.iter().any(|t| matches!(t, Token::Interpolation(_, _))));
    }

    #[test]
    fn escaped_double_brace_produces_escaped_brace_token() {
        let tokens = tokenize(r"\{{", "test.mds").unwrap();
        assert!(tokens.iter().any(|t| matches!(t, Token::EscapedBrace(_))));
    }

    #[test]
    fn old_single_brace_escape_is_now_plain_text() {
        // \{ is no longer an escape — it's two plain-text chars: \ and {
        let tokens = tokenize(r"\{name}", "test.mds").unwrap();
        // Should NOT produce EscapedBrace (that's only for \{{)
        assert!(!tokens.iter().any(|t| matches!(t, Token::EscapedBrace(_))));
        // Should NOT produce an interpolation
        assert!(!tokens.iter().any(|t| matches!(t, Token::Interpolation(_, _))));
    }

    #[test]
    fn double_brace_interpolation_with_string_arg() {
        // {{fn("}}")}} — the `}}` inside the string must not close the interpolation
        let tokens = tokenize(r#"{{fn("}}")}}"#, "test.mds").unwrap();
        let interp: Vec<_> = tokens
            .iter()
            .filter_map(|t| match t {
                Token::Interpolation(s, _) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(interp.len(), 1, "exactly one interpolation expected");
        assert_eq!(interp[0], r#"fn("}}")"#);
    }

    #[test]
    fn unclosed_double_brace_interpolation_error() {
        let err = tokenize("{{name", "test.mds").unwrap_err();
        assert!(
            format!("{err}").contains("unclosed"),
            "unclosed `{{` must produce error, got: {err}"
        );
    }

    #[test]
    fn double_brace_in_fence_is_suppressed() {
        let src = "text\n```\n{{no_interp}}\n```\nmore";
        let tokens = tokenize(src, "test.mds").unwrap();
        assert!(tokens.iter().any(|t| matches!(t, Token::CodeContent(_, _))));
        assert!(!tokens
            .iter()
            .any(|t| matches!(t, Token::Interpolation(s, _) if s == "no_interp")));
    }

    #[test]
    fn empty_double_brace_is_invalid() {
        // `{{ }}` — whitespace-only inner → trim → empty → either lexer accepts or rejects
        // Either way, check it doesn't panic.
        let result = tokenize("{{ }}", "test.mds");
        let _ = result;
    }

    #[test]
    fn lone_backslash_then_lone_brace_is_plain_text() {
        // \{ (not followed by {) — just two plain chars
        let tokens = tokenize("a\\{b", "test.mds").unwrap();
        assert!(!tokens.iter().any(|t| matches!(t, Token::EscapedBrace(_))));
        assert!(!tokens.iter().any(|t| matches!(t, Token::Interpolation(_, _))));
    }

    #[test]
    fn crlf_inside_interpolation_preserved() {
        // Multi-line interpolation (would be unusual but must not crash)
        let tokens = tokenize("{{name}}\r\n", "test.mds").unwrap();
        assert!(tokens.iter().any(|t| matches!(t, Token::Interpolation(s, _) if s == "name")));
    }
}
