use crate::lexer::find_interpolation_close;
use crate::limits::MAX_DOT_SEGMENTS;
use crate::parser::is_valid_identifier;

/// One piece of the original text of a URL, header or body template.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Piece<'a> {
    /// Text as written. An escaped `\{{` keeps its backslash here.
    Literal(&'a str),
    /// The trimmed path of an unescaped `{{…}}` reference.
    Reference(&'a str),
}

/// Split the original text into literal text and unescaped references, in order. An
/// unclosed reference ends the pieces with an error. Do not scan inserted text.
pub(super) fn pieces(text: &str) -> impl Iterator<Item = Result<Piece<'_>, &'static str>> {
    let mut rest = text;
    let mut next = None;
    std::iter::from_fn(move || {
        if let Some(reference) = next.take() {
            return Some(reference);
        }
        if rest.is_empty() {
            return None;
        }
        let Some((start, _)) = rest
            .match_indices("{{")
            .find(|&(index, _)| index == 0 || rest.as_bytes()[index - 1] != b'\\')
        else {
            return Some(Ok(Piece::Literal(std::mem::take(&mut rest))));
        };
        let literal = &rest[..start];
        let after_open = &rest[start + 2..];
        let reference = match find_interpolation_close(after_open.char_indices()) {
            Some(close) => {
                rest = &after_open[close + 2..];
                parse_reference(after_open[..close].trim()).map(Piece::Reference)
            }
            None => {
                rest = "";
                Err("unclosed `{{` reference")
            }
        };
        if literal.is_empty() {
            Some(reference)
        } else {
            next = Some(reference);
            Some(Ok(Piece::Literal(literal)))
        }
    })
}

/// Visit each unescaped reference in the original text. Do not scan inserted text.
pub(super) fn references(text: &str) -> impl Iterator<Item = Result<&str, &'static str>> {
    pieces(text).filter_map(|piece| match piece {
        Ok(Piece::Literal(_)) => None,
        Ok(Piece::Reference(path)) => Some(Ok(path)),
        Err(reason) => Some(Err(reason)),
    })
}

fn parse_reference(path: &str) -> Result<&str, &'static str> {
    if matches!(path, "true" | "false" | "null") {
        return Err("expected a reference name or dotted path");
    }
    for (index, segment) in path.split('.').enumerate() {
        if index == MAX_DOT_SEGMENTS {
            return Err("reference exceeds the member-access segment limit");
        }
        if !is_valid_identifier(segment) {
            return Err("expected a reference name or dotted path");
        }
    }
    Ok(path)
}
