use crate::error::MdsError;
use crate::escape_class::is_control_char;
use crate::limits::MAX_NESTING_DEPTH;

pub(crate) const INDENTED_DIRECTIVES: &str = "indented-directives";
pub(crate) const FILE_KEY_FORM: &str =
    "expected indented-directives: true or indented-directives: false";
const DUPLICATE_FILE_KEY: &str = "indented-directives must appear only once";
const BAD_INDENT: &str = "invalid indent for a conditional directive";
const STRAY_CONDITIONAL: &str = "conditional directive needs an open if in this code fence";
const UNCLOSED_CONDITIONAL: &str = "conditional directive must end before the code fence closes";
const QUOTED_OPT_IN: &str = "conditional code fence cannot have a blockquote prefix";
const CONTROL_IN_INFO: &str = "conditional code fence info contains a control character";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Conditional {
    If,
    Elseif,
    Else,
    End,
}

#[derive(Debug, PartialEq, Eq)]
enum LineClass {
    Plain,
    Conditional(Conditional),
    BadIndent,
}

fn classify_line(chars: &[char], pos: usize) -> LineClass {
    let mut bad_indent = false;
    let mut pos = pos;
    for &ch in &chars[pos..] {
        match ch {
            ' ' | '\t' => (),
            '\n' => return LineClass::Plain,
            ch if ch.is_whitespace() || is_control_char(ch) => bad_indent = true,
            _ => break,
        }
        pos += 1;
    }
    let Some(word) = chars[pos..].strip_prefix(&['@']) else {
        return LineClass::Plain;
    };
    let len = word
        .iter()
        .take_while(|ch| ch.is_ascii_alphabetic())
        .count();
    if word
        .get(len)
        .is_some_and(|&ch| ch.is_ascii_digit() || matches!(ch, '_' | '-'))
    {
        return LineClass::Plain;
    }
    let keyword = [
        ("if", Conditional::If),
        ("elseif", Conditional::Elseif),
        ("else", Conditional::Else),
        ("end", Conditional::End),
    ]
    .into_iter()
    .find(|(name, _)| ascii_case_eq(&word[..len], name));
    match keyword {
        None => LineClass::Plain,
        Some(_) if bad_indent => LineClass::BadIndent,
        Some((_, keyword)) => LineClass::Conditional(keyword),
    }
}

fn ascii_case_eq(chars: &[char], ascii: &str) -> bool {
    chars.len() == ascii.len()
        && chars
            .iter()
            .zip(ascii.chars())
            .all(|(ch, c)| ch.eq_ignore_ascii_case(&c))
}

struct OpenFence {
    ch: char,
    count: usize,
    opener: usize,
    conditionals: bool,
    open_ifs: Vec<usize>,
}

impl OpenFence {
    fn closes(&self, m: &FenceMatch, chars: &[char]) -> bool {
        m.fence_char == self.ch
            && m.fence_count >= self.count
            && chars[m.info_start..]
                .iter()
                .take_while(|&&c| !matches!(c, '\r' | '\n'))
                .all(|&c| is_blank(c))
    }

    fn on_conditional(&mut self, keyword: Conditional, offset: usize) -> Result<(), String> {
        match keyword {
            Conditional::If => {
                if self.open_ifs.len() >= MAX_NESTING_DEPTH {
                    return Err(format!(
                        "nesting depth exceeds maximum of {MAX_NESTING_DEPTH}"
                    ));
                }
                self.open_ifs.push(offset);
            }
            _ if self.open_ifs.is_empty() => return Err(STRAY_CONDITIONAL.into()),
            Conditional::End => self.open_ifs.truncate(self.open_ifs.len() - 1),
            Conditional::Elseif | Conditional::Else => (),
        }
        Ok(())
    }

    fn on_close(&self) -> Result<(), usize> {
        match self.open_ifs.last() {
            Some(&offset) => Err(offset),
            None => Ok(()),
        }
    }
}

/// Token types produced by the lexer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// Raw text content.
    Text(String, usize),
    /// An interpolation expression `{{...}}` — inner content without braces.
    Interpolation(String, usize),
    /// An escaped double-brace `\{{` that produces a literal `{{` in output.
    EscapedBrace(usize),
    /// A directive line with a line-start offset. Conditional directives can have
    /// an indent and can occur inside an opted-in code fence.
    Directive(String, usize),
    /// Frontmatter opening/closing `---`.
    FrontmatterFence(usize),
    /// Frontmatter content between `---` fences.
    FrontmatterContent(String, usize),
    /// An opening or closing code fence with a line-start offset. A marked
    /// opener keeps only the source prefix before the opt-in marker and its blanks.
    CodeFence(String, usize),
    /// Raw code fence content. Interpolation and escapes do not apply.
    CodeContent(String, usize),
}

/// A rejected key-shaped line in the lexer's frontmatter text.
pub(crate) struct FileKeyError {
    line: usize,
    message: &'static str,
}

/// Read the file key from normalized frontmatter, not from the module body or YAML tree.
pub(crate) fn probe_file_key(raw: &str) -> Result<Option<bool>, FileKeyError> {
    locate_file_key(raw).map(|key| key.map(|(_, enabled)| enabled))
}

fn locate_file_key(raw: &str) -> Result<Option<(usize, bool)>, FileKeyError> {
    let mut key = None;
    let mut offset = 0;
    for (line, text) in raw.split_inclusive('\n').enumerate() {
        let start = offset;
        offset += text.len();
        let text = text.strip_suffix('\n').unwrap_or(text);
        let Some(tail) = text.strip_prefix(INDENTED_DIRECTIVES) else {
            continue;
        };
        if !tail.starts_with(|ch| ch == ':' || is_blank(ch)) {
            continue;
        }
        if key.is_some() {
            return Err(FileKeyError {
                line,
                message: DUPLICATE_FILE_KEY,
            });
        }
        let invalid = || FileKeyError {
            line,
            message: FILE_KEY_FORM,
        };
        let value = tail
            .strip_prefix(':')
            .ok_or_else(invalid)?
            .trim_start_matches(is_blank);
        let end = value.find(is_blank).unwrap_or(value.len());
        let enabled = match &value[..end] {
            "true" => true,
            "false" => false,
            _ => return Err(invalid()),
        };
        let suffix = value[end..].trim_start_matches(is_blank);
        if !suffix.is_empty() && !suffix.starts_with('#') {
            return Err(invalid());
        }
        key = Some((start + text.len() - value.len(), enabled));
    }
    Ok(key)
}

/// Replace the probe's value with a same-width YAML null. This lets the resolver
/// prove that YAML reads this line, not a different key with the same value.
pub(crate) fn mask_file_key(raw: &str) -> Option<String> {
    let (offset, enabled) = locate_file_key(raw).ok()??;
    let null = if enabled { "null" } else { "null " };
    let mut masked = raw.to_owned();
    masked.replace_range(offset..offset + null.len(), null);
    Some(masked)
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
    code_fence: Option<OpenFence>,
    indented_directives: bool,
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
            indented_directives: false,
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

    fn line_class(&self, pos: usize) -> LineClass {
        if !is_line_start_chars(&self.chars, pos)
            || !self
                .code_fence
                .as_ref()
                .map_or(self.indented_directives, |fence| fence.conditionals)
        {
            return LineClass::Plain;
        }
        classify_line(&self.chars, pos)
    }

    fn fence_line_end(&self, from: usize) -> usize {
        self.chars[from..]
            .iter()
            .position(|&ch| matches!(ch, '\r' | '\n'))
            .map_or(self.chars.len(), |rel| from + rel)
    }

    fn syntax_line(&self, message: impl Into<String>, offset: usize) -> MdsError {
        let line = &self.source[offset..];
        let line = line
            .split('\n')
            .next()
            .unwrap_or(line)
            .trim_end_matches('\r');
        MdsError::syntax_at(message, self.file, self.source, offset, line.len())
    }

    fn push_fence(&mut self, kept_end: usize, line_end: usize) {
        let offset = self.byte_pos(self.pos);
        let text = self.source[offset..self.byte_pos(kept_end)].to_owned();
        self.tokens.push(Token::CodeFence(text, offset));
        self.pos = skip_newline(&self.chars, line_end);
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
                    self.indented_directives = probe_file_key(&content)
                        .map_err(|err| {
                            let prefix_len: usize = self.source[fm_byte_offset..bp]
                                .split_inclusive('\n')
                                .take(err.line)
                                .map(str::len)
                                .sum();
                            self.syntax_line(err.message, fm_byte_offset + prefix_len)
                        })?
                        .unwrap_or(false);
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

    /// Scan an opener and retain the exact source prefix before any opt-in marker.
    fn scan_code_fence(&mut self, m: &FenceMatch) -> Result<(), MdsError> {
        let opener = self.byte_pos(self.pos);
        let tail = &self.chars[m.info_start..];
        let lf = tail.iter().position(|&ch| ch == '\n').unwrap_or(tail.len());
        let info = &tail[..lf];
        let info = info.strip_suffix(&['\r']).unwrap_or(info);
        let OpenerInfo { opted_in, kept_len } =
            parse_opener_info(info).map_err(|OpenerError::InvalidWord| {
                self.syntax_line("invalid code fence info word", opener)
            })?;
        if opted_in {
            if self.chars[self.pos..m.fence_start].contains(&'>') {
                return Err(self.syntax_line(QUOTED_OPT_IN, opener));
            }
            if info.iter().copied().any(is_control_char) {
                return Err(self.syntax_line(CONTROL_IN_INFO, opener));
            }
        }
        // Keep the legacy bare-CR split for unmarked output. The info check does
        // not treat an embedded CR as the end of an LF-delimited line.
        let line_end = self.fence_line_end(m.info_start);
        let kept_end = if opted_in {
            m.info_start + kept_len
        } else {
            line_end
        };
        self.push_fence(kept_end, line_end);
        self.code_fence = Some(OpenFence {
            ch: m.fence_char,
            count: m.fence_count,
            opener,
            conditionals: opted_in,
            open_ifs: Vec::new(),
        });
        Ok(())
    }

    fn closes_fence(&self, m: &FenceMatch) -> bool {
        self.code_fence
            .as_ref()
            .is_some_and(|fence| fence.closes(m, &self.chars))
    }

    /// Scan raw content up to a closer or a conditional line.
    fn scan_code_content(&mut self) {
        let start = self.byte_pos(self.pos);
        while self.pos < self.chars.len() {
            if is_line_start_chars(&self.chars, self.pos)
                && (try_scan_fence_at(&self.chars, self.pos).is_some_and(|m| self.closes_fence(&m))
                    || self.line_class(self.pos) != LineClass::Plain)
            {
                break;
            }
            self.pos += 1;
        }
        let end = self.byte_pos(self.pos);
        if start < end {
            self.tokens.push(Token::CodeContent(
                self.source[start..end].to_owned(),
                start,
            ));
        }
    }

    /// Scan a directive from its line start, including any indent.
    ///
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
            if c == '}' && self.pos + 1 < self.chars.len() && self.chars[self.pos + 1] == '}' {
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
        #[cfg(test)]
        if tests::STALL_SCAN_TEXT.replace(false) {
            return;
        }

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

            // Stop before another token at a line start.
            if is_line_start_chars(&self.chars, self.pos)
                && (try_scan_fence_at(&self.chars, self.pos).is_some()
                    || c == '@'
                    || self.line_class(self.pos) != LineClass::Plain)
            {
                break;
            }

            text.push(c);
            self.pos += 1;
        }
        if !text.is_empty() {
            self.tokens.push(Token::Text(text, start));
        }
    }

    fn step(&mut self) -> Result<(), MdsError> {
        if self.pos == 0 && (self.source.starts_with("---\n") || self.source.starts_with("---\r\n"))
        {
            return self.scan_frontmatter();
        }

        let at_line_start = is_line_start_chars(&self.chars, self.pos);
        if at_line_start {
            if let Some(m) = try_scan_fence_at(&self.chars, self.pos) {
                match &self.code_fence {
                    None => return self.scan_code_fence(&m),
                    Some(fence) if fence.closes(&m, &self.chars) => {
                        fence
                            .on_close()
                            .map_err(|offset| self.syntax_line(UNCLOSED_CONDITIONAL, offset))?;
                        let end = self.fence_line_end(m.info_start);
                        self.push_fence(end, end);
                        self.code_fence = None;
                        return Ok(());
                    }
                    Some(_) => (),
                }
            }
        }

        if self.code_fence.is_none() && at_line_start && self.chars[self.pos] == '@' {
            self.scan_directive();
            return Ok(());
        }

        match self.line_class(self.pos) {
            LineClass::BadIndent => {
                return Err(self.syntax_line(BAD_INDENT, self.byte_pos(self.pos)))
            }
            LineClass::Conditional(keyword) => {
                let offset = self.byte_pos(self.pos);
                if let Some(fence) = &mut self.code_fence {
                    fence
                        .on_conditional(keyword, offset)
                        .map_err(|message| self.syntax_line(message, offset))?;
                }
                self.scan_directive();
            }
            LineClass::Plain if self.code_fence.is_some() => self.scan_code_content(),
            LineClass::Plain => return self.step_text(),
        }
        Ok(())
    }

    fn step_text(&mut self) -> Result<(), MdsError> {
        if self.scan_escape() {
            return Ok(());
        }
        if self.chars[self.pos..].starts_with(&['{', '{']) {
            return self.scan_interpolation();
        }
        self.scan_text();
        Ok(())
    }

    /// Drive each step to a later position or return an error.
    fn run(mut self) -> Result<Vec<Token>, MdsError> {
        while self.pos < self.chars.len() {
            let before = self.pos;
            self.step()?;
            if self.pos <= before {
                return Err(MdsError::internal());
            }
        }

        // An unclosed fence takes precedence over its open conditionals at EOF.
        if let Some(fence) = self.code_fence {
            return Err(MdsError::syntax_at(
                "unclosed code fence",
                self.file,
                self.source,
                fence.opener,
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

struct FenceMatch {
    fence_char: char,
    fence_count: usize,
    fence_start: usize,
    info_start: usize,
}

/// Accept a `[ \t>]*` prefix before at least three backticks or tildes.
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
    Some(FenceMatch {
        fence_char,
        fence_count,
        fence_start,
        info_start: fence_start + fence_count,
    })
}

#[derive(Debug, PartialEq, Eq)]
struct OpenerInfo {
    opted_in: bool,
    kept_len: usize,
}

#[derive(Debug, PartialEq, Eq)]
enum OpenerError {
    InvalidWord,
}

fn is_blank(ch: char) -> bool {
    matches!(ch, ' ' | '\t')
}

fn without_trailing_blanks(info: &[char]) -> &[char] {
    let len = info
        .iter()
        .rposition(|&ch| !is_blank(ch))
        .map_or(0, |last| last + 1);
    &info[..len]
}

fn parse_opener_info(info: &[char]) -> Result<OpenerInfo, OpenerError> {
    let trimmed = without_trailing_blanks(info);
    let last = trimmed
        .rsplit(|&ch| is_blank(ch))
        .next()
        .unwrap_or_default();
    let opted_in = last == ['+', 'm', 'd', 's'];
    let kept = if opted_in {
        let marker = trimmed.len() - last.len();
        without_trailing_blanks(&info[..marker])
    } else {
        info
    };

    let mut after_plus = false;
    for word in kept
        .split(|&ch| is_blank(ch))
        .filter(|word| !word.is_empty())
    {
        if matches!(word, ['+', ch, ..] if ch.is_ascii_alphabetic() || !ch.is_ascii())
            || word.windows(4).any(|window| ascii_case_eq(window, "+mds"))
            || (after_plus && ascii_case_eq(word, "mds"))
        {
            return Err(OpenerError::InvalidWord);
        }
        after_plus = word == ['+'];
    }
    Ok(OpenerInfo {
        opted_in,
        kept_len: kept.len(),
    })
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

    thread_local! {
        pub(super) static STALL_SCAN_TEXT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    #[test]
    fn stalled_text_scanner_returns_internal_error() {
        STALL_SCAN_TEXT.set(true);
        let result = tokenize("plain text", "test.mds");
        STALL_SCAN_TEXT.set(false);
        assert_eq!(result.unwrap_err().serialize().code, "mds::internal");
    }

    #[test]
    fn opted_in_fence_strips_marker_and_lexes_whole_conditional_lines() {
        let source = "    ~~~míd \t+mds \t\r\n \t@if true:\r\n\t@elseif false:\r\n    \tkeep\r\n  @else:\r\n\t@end\r\n  ~~~~ \t\r\n";
        let at = |line| source.find(line).unwrap();
        assert_eq!(
            tokenize(source, "test.mds").unwrap(),
            [
                Token::CodeFence("    ~~~míd".into(), 0),
                Token::Directive(" \t@if true:".into(), at(" \t@if")),
                Token::Directive("\t@elseif false:".into(), at("\t@elseif")),
                Token::CodeContent("    \tkeep\r\n".into(), at("    \tkeep")),
                Token::Directive("  @else:".into(), at("  @else")),
                Token::Directive("\t@end".into(), at("\t@end")),
                Token::CodeFence("  ~~~~ \t".into(), at("  ~~~~")),
            ]
        );
    }

    #[test]
    fn opted_in_fence_keeps_other_lines_and_inner_markers_literal() {
        let content = "``` +mds\n``` +other\n{{x}} \\{{\n@include f\n@import f\n@for x\n@define f\n@message f\n@block f\n@export f\n@extends f\n@endnote\n@ifdef\n@Endpoint\n@if2\n@else_\n@end-\n> @if true:\n";
        let opener = "```` +mds\n";
        let closer = "````";
        let source = format!("{opener}{content}{closer}");
        assert_eq!(
            tokenize(&source, "test.mds").unwrap(),
            [
                Token::CodeFence("````".into(), 0),
                Token::CodeContent(content.into(), opener.len()),
                Token::CodeFence(closer.into(), opener.len() + content.len()),
            ]
        );
    }

    #[test]
    fn only_exact_marker_opts_in() {
        let content = "@if true:\n@end\n";
        for info in ["mds".to_owned(), format!("{}sdm+", '\u{202e}')] {
            let opener = format!("```{info}");
            let source = format!("{opener}\n{content}```\n");
            assert_eq!(
                tokenize(&source, "test.mds").unwrap(),
                [
                    Token::CodeFence(opener.clone(), 0),
                    Token::CodeContent(content.into(), opener.len() + 1),
                    Token::CodeFence("```".into(), opener.len() + 1 + content.len()),
                ]
            );
        }
    }

    #[test]
    fn rejects_reserved_words_in_any_opener() {
        for info in [
            "+foo",
            "+é",
            "mds +mds linenos",
            "+mds +mds",
            "+MDS",
            "py+MdS",
            "+ mDs",
            "+\tmds",
            "\u{a0}+mds",
            "+mds\u{a0}",
        ] {
            let opener = format!("  ```{info}");
            assert_lexer_error(
                &format!("{opener}\n```\n"),
                0,
                &opener,
                "invalid code fence info word",
            );
        }
    }

    #[test]
    fn accepts_other_plus_words_without_stripping_them() {
        for info in [
            "+",
            "+5",
            "+=",
            "++",
            "+\"",
            "title=\"a + b\"",
            "title=\"+5\"",
        ] {
            let opener = format!("~~~{info} \t");
            let source = format!("{opener}\n~~~\n");
            assert_eq!(
                tokenize(&source, "test.mds").unwrap()[0],
                Token::CodeFence(opener, 0)
            );
        }
    }

    #[test]
    fn rejects_marked_blockquote_and_marked_info_controls() {
        let quote = " > > ```py +mds";
        assert_lexer_error(
            &format!("{quote}\n```\n"),
            0,
            quote,
            "conditional code fence cannot have a blockquote prefix",
        );
        for control in [
            '\x1b', '\x7f', '\u{0085}', '\u{202e}', '\u{2028}', '\u{feff}', '\r',
        ] {
            let opener = format!("\t```py{control} +mds");
            assert_lexer_error(
                &format!("text\n{opener}\r\n```\n"),
                5,
                &opener,
                "conditional code fence info contains a control character",
            );
        }
    }

    #[test]
    fn bad_indent_after_code_content_is_an_error_at_that_line() {
        let prefix = "``` +mds\r\n@% literal\r\nkeep\r\n";
        for bad in ['\u{a0}', '\u{3000}', '\u{feff}', '\u{202e}', '\x1b', '\x0b'] {
            let line = format!(" {bad}\t@IF true:");
            let source = format!("{prefix}{line}\r\n```\r\n");
            assert_lexer_error(
                &source,
                prefix.len(),
                &line,
                "invalid indent for a conditional directive",
            );
        }
    }

    #[test]
    fn keyword_near_misses_reach_the_parser_instead_of_becoming_text() {
        let prefix = "``` +mds\n@if enabled:\n";
        for line in [
            "@else",
            "@elseif:",
            "@elseif",
            "@end extra",
            "@if x",
            "@end of the day",
            "@Else:",
            "@END",
            "@end.",
            "@if(x):",
        ] {
            let tail = if line == "@if x" || line == "@if(x):" {
                "@end\n"
            } else {
                ""
            };
            let end = if line.to_ascii_lowercase().starts_with("@end") {
                ""
            } else {
                "@end\n"
            };
            let source = format!("{prefix}\t{line}\n{tail}{end}```\n");
            let tokens = tokenize(&source, "test.mds").unwrap();
            assert_eq!(
                tokens[2],
                Token::Directive(format!("\t{line}"), prefix.len())
            );
            let document = format!("---\nenabled: true\n---\n{source}");
            assert_eq!(
                crate::compile_str(&document).unwrap_err().serialize().code,
                "mds::syntax"
            );
        }
    }

    #[test]
    fn closer_refuses_innermost_open_if_but_unclosed_fence_wins_at_eof() {
        let prefix = "head\n``` +mds\n@if true:\n";
        let inner = "\t@if false:";
        let source = format!("{prefix}{inner}\n```\n@end\n@end\n");
        assert_lexer_error(
            &source,
            prefix.len(),
            inner,
            "conditional directive must end before the code fence closes",
        );
        assert_lexer_error(
            &format!("{prefix}{inner}\n"),
            5,
            "```",
            "unclosed code fence",
        );
    }

    #[test]
    fn in_fence_terminators_cannot_use_an_outer_or_previous_if() {
        for prefix in [
            "@if true:\n",
            "@if false:\n@else:\n",
            "@for x in xs:\n",
            "@define f():\n",
            "@message user:\n",
            "@block f:\n",
            "``` +mds\n@if true:\n@end\n```\n",
        ] {
            let before = format!("{prefix}``` +mds\n");
            for line in ["  @end", "\t@else:", " @elseif true:"] {
                let source = format!("{before}{line}\n```\n@end\n");
                assert_lexer_error(
                    &source,
                    before.len(),
                    line,
                    "conditional directive needs an open if in this code fence",
                );
            }
        }
    }

    #[test]
    fn accepts_64_nested_ifs_and_rejects_the_65th_in_the_lexer() {
        let open = "  @if enabled:";
        let with_depth = |depth| {
            format!(
                "``` +mds\n{}keep\n{}```\n",
                format!("{open}\n").repeat(depth),
                "  @end\n".repeat(depth)
            )
        };
        let source = format!("---\nenabled: true\n---\n{}", with_depth(64));
        let result = crate::compile_str(&source).unwrap();
        assert!(
            matches!(result.output, crate::CompiledOutput::Markdown(output) if output.ends_with("```\nkeep\n```\n"))
        );
        assert_lexer_error(
            &with_depth(65),
            "``` +mds\n".len() + 64 * (open.len() + 1),
            open,
            "nesting depth exceeds maximum of 64",
        );
    }

    fn assert_lexer_error(source: &str, offset: usize, span_text: &str, expected: &str) {
        let error = tokenize(source, "partial.mds").unwrap_err();
        let serialized = error.serialize();
        assert_eq!(serialized.code, "mds::syntax");
        let span = serialized.span.unwrap();
        assert_eq!((span.offset, span.length), (offset, span_text.len()));
        let MdsError::Syntax {
            message,
            src: Some(src),
            ..
        } = error
        else {
            panic!("expected a syntax error with a source, got {error:?}");
        };
        assert_eq!(message, expected);
        assert_eq!(src.name(), "partial.mds");
    }

    #[test]
    fn opener_info_counts_characters_and_checks_ascii_words() {
        for (info, opted_in, kept_len) in [
            ("", false, 0),
            ("é \t", false, 3),
            ("é \t+mds \t", true, 1),
            (" \t+mds", true, 0),
        ] {
            let chars: Vec<_> = info.chars().collect();
            assert_eq!(
                parse_opener_info(&chars),
                Ok(OpenerInfo { opted_in, kept_len })
            );
        }
        for info in ["+letter", "a+MdS", "+ \tMdS", "+mds tail", "+mds +mds"] {
            let chars: Vec<_> = info.chars().collect();
            assert_eq!(parse_opener_info(&chars), Err(OpenerError::InvalidWord));
        }
    }

    #[test]
    fn indented_directives_file_key_selects_only_conditionals_outside_plain_fences() {
        let body = "lead\n    @if enabled:\n\t@elseif fallback:\n  @else:\n @end\n  @import lib\n\t@include lib\n  > @if enabled:\n```\n  @if enabled:\n  @end\n```\n";
        for (raw, count) in [
            ("enabled: true", 0),
            ("indented-directives: false \t # off", 0),
            ("indented-directives: \ttrue  # on", 4),
        ] {
            let source = format!("---\n{raw}\n---\n{body}");
            let tokens = tokenize(&source, "test.mds").unwrap();
            let lines: Vec<_> = tokens
                .iter()
                .filter_map(|token| match token {
                    Token::Directive(line, _) => Some(line.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(
                lines,
                [
                    "    @if enabled:",
                    "\t@elseif fallback:",
                    "  @else:",
                    " @end"
                ][..count]
            );
        }
    }

    #[test]
    fn indented_directives_classifies_near_misses_for_the_parser_to_reject() {
        for line in [
            "  @IF enabled:",
            "  @if.",
            "  @elseif:",
            "  @else",
            "  @end!",
        ] {
            let prefix = "---\nindented-directives: true\n---\nlead\n";
            let source = format!("{prefix}{line}\n");
            assert_eq!(
                tokenize(&source, "test.mds").unwrap().last(),
                Some(&Token::Directive(line.to_owned(), prefix.len())),
            );
        }
    }

    #[test]
    fn indented_directives_bad_indent_errors_only_with_the_key() {
        for indent in ["\u{a0}", " \r ", "\t\x1b"] {
            let line = format!("{indent}@if enabled:");
            let prefix = "---\nindented-directives: true\n---\nlead\n";
            assert_lexer_error(
                &format!("{prefix}{line}\n"),
                prefix.len(),
                &line,
                BAD_INDENT,
            );
            assert_eq!(
                tokenize(&line, "test.mds").unwrap(),
                [Token::Text(line.clone(), 0)],
            );
        }
    }

    #[test]
    fn indented_directives_malformed_key_reports_the_original_line() {
        let prefix = "---\r\nname: café\r\n";
        for line in [
            "indented-directives: True",
            "indented-directives: yes",
            "indented-directives: 'true'",
            "indented-directives : true",
            "indented-directives\t: true",
            "indented-directives: true text",
            "indented-directives: true#comment",
        ] {
            assert_lexer_error(
                &format!("{prefix}{line}\r\n---\r\n"),
                prefix.len(),
                line,
                "expected indented-directives: true or indented-directives: false",
            );
        }
    }

    #[test]
    fn indented_directives_duplicate_key_reports_the_second_line() {
        let prefix = "---\r\nindented-directives: true\r\n";
        let line = "indented-directives: false";
        assert_lexer_error(
            &format!("{prefix}{line}\r\n---\r\n"),
            prefix.len(),
            line,
            "indented-directives must appear only once",
        );
    }

    const ALPHABET: [&str; 20] = [
        " ", "\t", "@", "if", "elseif", "else", "end", ":", "\r", "\n", "```", ">", "\u{a0}",
        "\u{202e}", "+mds", "+x", "{{", "}}", "\\", "x",
    ];

    #[test]
    fn lexer_progress_for_exhaustive_and_seeded_inputs() {
        let start = std::time::Instant::now();
        let mut hits = std::collections::BTreeMap::new();
        let mut body = String::new();
        let mut exhaustive = 0;
        for len in 0..=4 {
            for mut word in 0..ALPHABET.len().pow(len) {
                body.clear();
                for _ in 0..len {
                    body.push_str(ALPHABET[word % ALPHABET.len()]);
                    word /= ALPHABET.len();
                }
                if body.bytes().filter(|&b| b == b'\n').count() <= 2 {
                    exhaustive += 1;
                    probe_progress_states(&body, &mut hits);
                }
            }
        }
        assert_eq!(exhaustive, 168_343);

        let mut seed = 0xa452_0721_u32;
        let mut draw = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as usize
        };
        for _ in 0..100_000 {
            body.clear();
            for _ in 0..draw() % 17 {
                body.push_str(ALPHABET[draw() % ALPHABET.len()]);
            }
            probe_progress_states(&body, &mut hits);
        }

        // The alphabet cannot form frontmatter or reach the depth limit.
        probe_progress("---\nk: v\n---\n", &mut hits);
        probe_progress("---\nk: v\n", &mut hits);
        probe_progress(&format!("``` +mds\n{}", "@if x:\n".repeat(65)), &mut hits);
        // Give rare paths fixed witnesses too.
        probe_progress("> ``` +mds\n```", &mut hits);
        probe_progress("```x\x1b +mds\n```", &mut hits);
        probe_progress("``` +mds\n@if x:\n@end\n```", &mut hits);

        for path in [
            "plain line",
            "conditional line",
            "indented conditional line",
            "bad indent",
            "frontmatter",
            "fence opener",
            "fence closer",
            "code content",
            "plain directive",
            "escaped brace",
            "interpolation",
            "text",
            "unclosed frontmatter",
            "unclosed interpolation",
            "unclosed fence",
            "bad info word",
            "quoted opt-in",
            "control in info",
            "stray conditional",
            "unclosed conditional",
            "depth cap",
        ] {
            assert!(
                hits.get(path).copied().unwrap_or_default() > 0,
                "no hit: {path}"
            );
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(60),
            "enumeration exceeds 60 seconds"
        );
    }

    type ProgressHits = std::collections::BTreeMap<&'static str, usize>;

    fn probe_progress_states(body: &str, hits: &mut ProgressHits) {
        probe_progress(body, hits);
        for enabled in [true, false] {
            probe_progress(
                &format!("---\nindented-directives: {enabled}\n---\n{body}"),
                hits,
            );
        }
        probe_progress(&format!("``` +mds\n{body}\n```\n"), hits);
    }

    fn probe_progress(source: &str, hits: &mut ProgressHits) {
        let mut hit = |path| *hits.entry(path).or_default() += 1;
        let tokens = match tokenize(source, "enumerate.mds") {
            Ok(tokens) => tokens,
            Err(MdsError::Syntax { message, .. }) => {
                hit(match message.as_str() {
                    "unclosed frontmatter (missing closing ---)" => "unclosed frontmatter",
                    m if m.starts_with("unclosed `{{` interpolation") => "unclosed interpolation",
                    "unclosed code fence" => "unclosed fence",
                    "invalid code fence info word" => "bad info word",
                    QUOTED_OPT_IN => "quoted opt-in",
                    CONTROL_IN_INFO => "control in info",
                    BAD_INDENT => "bad indent",
                    STRAY_CONDITIONAL => "stray conditional",
                    UNCLOSED_CONDITIONAL => "unclosed conditional",
                    "nesting depth exceeds maximum of 64" => "depth cap",
                    _ => panic!("unexpected syntax error for {source:?}: {message}"),
                });
                return;
            }
            Err(error) => panic!("unexpected error for {source:?}: {error}"),
        };
        let mut last = None;
        let mut fence = None;
        for token in tokens {
            let (offset, path) = match token {
                Token::Text(ref text, offset) | Token::CodeContent(ref text, offset) => {
                    assert_eq!(source.get(offset..offset + text.len()), Some(text.as_str()));
                    if fence == Some(true) {
                        assert!(matches!(token, Token::CodeContent(..)));
                        let first_is_line_start =
                            offset == 0 || source.as_bytes()[offset - 1] == b'\n';
                        for line in text.split('\n').skip(usize::from(!first_is_line_start)) {
                            assert!(
                                !is_keyword_line(line),
                                "keyword inside code content: {source:?}"
                            );
                            hit("plain line");
                        }
                    }
                    let path = if matches!(token, Token::CodeContent(..)) {
                        "code content"
                    } else {
                        "text"
                    };
                    (offset, path)
                }
                Token::Directive(line, offset) if fence == Some(true) => {
                    assert!(
                        is_keyword_line(&line),
                        "ordinary code became a directive: {source:?}"
                    );
                    (offset, "conditional line")
                }
                Token::Directive(line, offset) if line.starts_with([' ', '\t']) => {
                    assert!(source.starts_with("---\nindented-directives: true\n---\n"));
                    assert!(is_keyword_line(&line));
                    (offset, "indented conditional line")
                }
                Token::Directive(_, offset) => (offset, "plain directive"),
                Token::CodeFence(_, offset) if fence.is_some() => {
                    fence = None;
                    (offset, "fence closer")
                }
                Token::CodeFence(_, offset) => {
                    let opener = source[offset..].split('\n').next().unwrap();
                    let info = opener.trim_end_matches('\r').trim_end_matches([' ', '\t']);
                    let last = info.rsplit([' ', '\t', '`', '~']).next();
                    fence = Some(last == Some("+mds"));
                    (offset, "fence opener")
                }
                Token::Interpolation(_, offset) => (offset, "interpolation"),
                Token::EscapedBrace(offset) => (offset, "escaped brace"),
                Token::FrontmatterFence(offset) | Token::FrontmatterContent(_, offset) => {
                    (offset, "frontmatter")
                }
            };
            assert!(
                last.is_none_or(|prev| prev < offset),
                "offsets do not rise: {source:?}"
            );
            last = Some(offset);
            hit(path);
        }
    }

    fn is_keyword_line(line: &str) -> bool {
        let before_cr = line.split('\r').next().unwrap();
        let leading =
            before_cr.trim_start_matches(|ch: char| ch.is_whitespace() || is_control_char(ch));
        let Some(word) = leading.strip_prefix('@') else {
            return false;
        };
        let end = word
            .find(|ch: char| !ch.is_ascii_alphabetic())
            .unwrap_or(word.len());
        ["if", "elseif", "else", "end"]
            .iter()
            .any(|name| word[..end].eq_ignore_ascii_case(name))
            && !matches!(word.as_bytes().get(end), Some(b'0'..=b'9' | b'_' | b'-'))
    }

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
        assert!(!tokens
            .iter()
            .any(|t| matches!(t, Token::Interpolation(_, _))));
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
        assert!(!tokens
            .iter()
            .any(|t| matches!(t, Token::Interpolation(_, _))));
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
        assert!(!tokens
            .iter()
            .any(|t| matches!(t, Token::Interpolation(_, _))));
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
        assert!(!tokens
            .iter()
            .any(|t| matches!(t, Token::Interpolation(_, _))));
    }

    #[test]
    fn crlf_inside_interpolation_preserved() {
        // Multi-line interpolation (would be unusual but must not crash)
        let tokens = tokenize("{{name}}\r\n", "test.mds").unwrap();
        assert!(tokens
            .iter()
            .any(|t| matches!(t, Token::Interpolation(s, _) if s == "name")));
    }
}
