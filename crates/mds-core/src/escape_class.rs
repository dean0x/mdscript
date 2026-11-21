//! Characters that need escape or replacement at a diagnostic output boundary.

/// C0 controls except tab and LF, DEL, C1 controls and Unicode format hazards.
#[inline]
pub(crate) fn is_control_char(ch: char) -> bool {
    (ch < '\u{0020}' && ch != '\n' && ch != '\t')
        || ch == '\u{007F}'
        || ('\u{0080}'..='\u{009F}').contains(&ch)
        || is_two_byte_format_hazard(ch)
        || is_three_byte_format_hazard(ch)
}

// The diagnostic renderer uses the UTF-8 widths to preserve the source byte offsets.
#[inline]
pub(crate) fn is_two_byte_format_hazard(ch: char) -> bool {
    ch == '\u{061C}' // Arabic Letter Mark
}

#[inline]
pub(crate) fn is_three_byte_format_hazard(ch: char) -> bool {
    matches!(ch,
        '\u{200E}' | '\u{200F}' // Bidi marks
        | '\u{2028}' | '\u{2029}' // Line and paragraph separators
        | '\u{202A}'..='\u{202E}' // Bidi embeddings and overrides
        | '\u{2066}'..='\u{2069}' // Bidi isolates
        | '\u{FEFF}' // Byte order mark
    )
}
