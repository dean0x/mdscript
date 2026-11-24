use crate::lexer::find_interpolation_close;
use crate::limits::MAX_DOT_SEGMENTS;
use crate::parser::is_valid_identifier;

/// Visit each unescaped reference in the original text. Do not scan inserted text.
pub(super) fn references(text: &str) -> impl Iterator<Item = Result<&str, &'static str>> {
    let mut rest = text;
    std::iter::from_fn(move || {
        let (start, _) = rest
            .match_indices("{{")
            .find(|&(index, _)| index == 0 || rest.as_bytes()[index - 1] != b'\\')?;
        let after_open = &rest[start + 2..];
        let Some(close) = find_interpolation_close(after_open.char_indices()) else {
            rest = "";
            return Some(Err("unclosed `{{` reference"));
        };
        rest = &after_open[close + 2..];
        Some(parse_reference(after_open[..close].trim()))
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
