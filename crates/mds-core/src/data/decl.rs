//! Parse and check data declarations before replay or any I/O.

use std::mem::size_of;

use serde_yaml_ng::{Mapping, Value};

use super::{refs::references, DataLabel};
use crate::evaluator::budget::{MemBudget, Part};
use crate::limits::MAX_DATA_ENTRIES_PER_MODULE;
use crate::parser::is_valid_identifier;
use crate::value::MAX_VALUE_DEPTH;
use crate::{escape_path_for_message, MdsError};

// SYNC POINT: each reserved frontmatter key is a reserved entry name. Audit
// `RESERVED_OUTPUT_KEYS` in lib.rs and the sites that it names.
const RESERVED_NAMES: [&str; 6] = [
    "prompt",
    "imports",
    "type",
    "extends",
    "data",
    "indented-directives",
];
const RENAME_HELP: &str = "`data` is reserved for data sources; rename the variable";

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Source {
    File(String),
    /// The paths that the declaration references, in URL, header, then body order.
    Url(Vec<String>),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) source: Source,
}

/// Keep only the local path or the reference paths that the graph needs.
/// The caller supplies the `data` value from the bounded frontmatter parser.
#[must_use = "data declaration analysis can fail"]
pub(crate) fn parse_declarations(
    data: &Value,
    module: &str,
    mem: &mut MemBudget,
) -> Result<Vec<Entry>, MdsError> {
    let Value::Mapping(mapping) = data else {
        return Err(MdsError::data(
            module,
            None,
            None,
            "expected a mapping of data entries",
            Some(RENAME_HELP.into()),
        ));
    };
    if mapping.len() > MAX_DATA_ENTRIES_PER_MODULE {
        return Err(MdsError::resource_limit(format!(
            "\"{}\": too many data entries (max {MAX_DATA_ENTRIES_PER_MODULE} per module)",
            escape_path_for_message(module)
        )));
    }

    let mark = mem.mark();
    let result = parse_mapping(mapping, module, mem);
    if result.is_ok() {
        mem.transfer(Part::Temp, Part::Buffers, mem.mark() - mark);
    } else {
        mem.release_to(mark);
    }
    result
}

/// Check claims in the final scope, after any entry merge. Omit runtime variables.
#[must_use = "a data entry can collide with another name in scope"]
pub(crate) fn check_collisions<'a>(
    entries: impl IntoIterator<Item = &'a Entry>,
    mut is_claimed: impl FnMut(&str) -> bool,
) -> Result<(), MdsError> {
    for entry in entries {
        if is_claimed(&entry.name) {
            return Err(MdsError::name_collision(&entry.name));
        }
    }
    Ok(())
}

fn parse_mapping(
    mapping: &Mapping,
    module: &str,
    mem: &mut MemBudget,
) -> Result<Vec<Entry>, MdsError> {
    mem.charge(Part::Temp, mapping.len().checked_mul(size_of::<Entry>()))?;
    let mut entries = Vec::with_capacity(mapping.len());
    for (key, value) in mapping {
        let Value::String(name) = key else {
            return Err(MdsError::data(
                module,
                None,
                None,
                "data entry name must be an identifier",
                None,
            ));
        };
        let label = DataLabel {
            module,
            entry: name,
            file: None,
        };
        if RESERVED_NAMES.contains(&name.as_str()) {
            return Err(label.error(None, "data entry name is reserved"));
        }
        if !is_valid_identifier(name) {
            return Err(label.error(None, "data entry name must be an identifier"));
        }
        let source = label.parse_source(value, mem)?;
        mem.charge(Part::Temp, Some(name.len()))?;
        entries.push(Entry {
            name: name.clone(),
            source,
        });
    }
    Ok(entries)
}

impl DataLabel<'_> {
    fn unknown_form(self) -> MdsError {
        MdsError::data(
            self.module,
            Some(self.entry),
            None,
            "expected a dot-relative path, an HTTPS URL or a request map",
            Some(RENAME_HELP.into()),
        )
    }

    fn check_quote(self, value: &Value, field: Option<&str>) -> Result<(), MdsError> {
        if let Value::Mapping(map) = value {
            if map.len() == 1 && matches!(map.iter().next(), Some((Value::Mapping(_), Value::Null)))
            {
                return Err(MdsError::data(
                    self.module,
                    Some(self.entry),
                    field,
                    "a YAML map is not a reference",
                    Some("quote the value that contains `{{…}}`".into()),
                ));
            }
        }
        Ok(())
    }

    fn parse_source(self, value: &Value, mem: &mut MemBudget) -> Result<Source, MdsError> {
        self.check_quote(value, None)?;
        let mut refs = Vec::new();
        match value {
            Value::String(path) if path.starts_with("./") || path.starts_with("../") => {
                if path.contains("{{") {
                    return Err(self.error(Some("path"), "file paths cannot contain `{{`"));
                }
                mem.charge(Part::Temp, Some(path.len()))?;
                return Ok(Source::File(path.clone()));
            }
            Value::String(url) if has_scheme(url) || url.contains("://") => {
                self.parse_url(url, &mut refs, mem)?;
            }
            Value::Mapping(fields) => self.parse_request(fields, &mut refs, mem)?,
            _ => return Err(self.unknown_form()),
        }
        Ok(Source::Url(refs))
    }

    fn parse_request(
        self,
        fields: &Mapping,
        refs: &mut Vec<String>,
        mem: &mut MemBudget,
    ) -> Result<(), MdsError> {
        let Some(url) = fields.get("url") else {
            return if fields.contains_key("method")
                || fields.contains_key("accept")
                || fields.contains_key("headers")
                || fields.contains_key("body")
                || fields.contains_key("cache")
            {
                Err(self.error(Some("url"), "request map requires a URL"))
            } else {
                Err(self.unknown_form())
            };
        };
        self.check_quote(url, Some("url"))?;
        let Value::String(url) = url else {
            return Err(self.error(Some("url"), "expected a URL string"));
        };
        self.parse_url(url, refs, mem)?;
        for (key, value) in fields {
            let Value::String(field) = key else {
                return Err(self.error(None, "request field name must be a string"));
            };
            let reason = match (field.as_str(), value) {
                ("url" | "headers" | "body", _) => continue,
                ("method", Value::String(s))
                    if matches!(
                        s.as_str(),
                        "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS"
                    ) =>
                {
                    continue
                }
                ("accept", Value::String(s)) if matches!(s.as_str(), "json" | "yaml" | "text") => {
                    continue
                }
                ("cache", Value::String(s)) if valid_cache(s) => continue,
                ("method", _) => "expected GET, HEAD, POST, PUT, PATCH, DELETE or OPTIONS",
                ("accept", _) => "expected json, yaml or text",
                ("cache", _) => "expected a duration in s, m, h or d, forever or off",
                _ => "unknown request field",
            };
            return Err(self.error(Some(field), reason));
        }
        if let Some(headers) = fields.get("headers") {
            self.parse_headers(headers, refs, mem)?;
        }
        if let Some(body) = fields.get("body") {
            self.check_quote(body, Some("body"))?;
            match body {
                Value::Mapping(_) => self.parse_body_tree(body, 0, refs, mem)?,
                Value::String(s) if references(s).next().is_none() => (),
                Value::String(_) => {
                    return Err(self.error(Some("body"), "a string body cannot contain references"))
                }
                _ => return Err(self.error(Some("body"), "expected a mapping or a literal string")),
            }
        }
        Ok(())
    }

    fn parse_url(
        self,
        url: &str,
        refs: &mut Vec<String>,
        mem: &mut MemBudget,
    ) -> Result<(), MdsError> {
        let field = Some("url");
        let Some(rest) = url.strip_prefix("https://") else {
            return Err(self.error(field, "expected the literal https scheme"));
        };
        if rest.contains(['\\', '#']) {
            return Err(self.error(field, "URL cannot contain a backslash or fragment"));
        }
        let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, suffix) = rest.split_at(authority_end);
        let host = if authority.starts_with('[') {
            authority
        } else {
            authority.split(':').next().unwrap_or_default()
        };
        if host.is_empty()
            || host.ends_with('.')
            || authority.contains("{{")
            || authority.contains(['@', '%'])
            || authority
                .chars()
                .any(|ch| ch.is_whitespace() || ch.is_control())
        {
            return Err(self.error(
                field,
                "expected a literal URL authority with no trailing host dot",
            ));
        }
        if suffix.starts_with('?') {
            return Err(self.error(field, "URL with a query requires a path"));
        }
        self.add_refs(suffix, "url", refs, mem)
    }

    fn parse_headers(
        self,
        headers: &Value,
        refs: &mut Vec<String>,
        mem: &mut MemBudget,
    ) -> Result<(), MdsError> {
        self.check_quote(headers, Some("headers"))?;
        let Value::Mapping(headers) = headers else {
            return Err(self.error(Some("headers"), "expected a header mapping"));
        };
        for (key, value) in headers {
            let Value::String(name) = key else {
                return Err(self.error(Some("headers"), "header name must be a literal token"));
            };
            if !header_token(name) {
                return Err(self.error(Some(name), "header name must be a literal token"));
            }
            self.check_quote(value, Some(name))?;
            match value {
                Value::String(s) => self.add_refs(s, name, refs, mem)?,
                Value::Mapping(map) if valid_secret(map) => (),
                _ => {
                    return Err(self.error(
                        Some(name),
                        "expected a string or a secret name with a literal prefix",
                    ))
                }
            }
        }
        Ok(())
    }

    fn parse_body_tree(
        self,
        value: &Value,
        depth: usize,
        refs: &mut Vec<String>,
        mem: &mut MemBudget,
    ) -> Result<(), MdsError> {
        let field = Some("body");
        if depth > MAX_VALUE_DEPTH {
            return Err(self.error(field, "body exceeds the maximum value depth"));
        }
        self.check_quote(value, field)?;
        match value {
            Value::Mapping(map) => {
                for (key, value) in map {
                    if !matches!(key, Value::String(_)) {
                        return Err(self.error(field, "body map keys must be strings"));
                    }
                    self.parse_body_tree(value, depth + 1, refs, mem)?;
                }
            }
            Value::Sequence(items) => {
                for item in items {
                    self.parse_body_tree(item, depth + 1, refs, mem)?;
                }
            }
            Value::String(s) => self.add_refs(s, "body", refs, mem)?,
            Value::Tagged(_) => return Err(self.error(field, "body cannot contain YAML tags")),
            Value::Null | Value::Bool(_) | Value::Number(_) => (),
        }
        Ok(())
    }

    fn add_refs(
        self,
        text: &str,
        field: &str,
        refs: &mut Vec<String>,
        mem: &mut MemBudget,
    ) -> Result<(), MdsError> {
        for path in references(text) {
            let path = path.map_err(|reason| self.error(Some(field), reason))?;
            if refs.len() == refs.capacity() {
                let extra = refs.capacity().max(4);
                mem.charge(Part::Temp, extra.checked_mul(size_of::<String>()))?;
                refs.reserve_exact(extra);
            }
            mem.charge(Part::Temp, Some(path.len()))?;
            refs.push(path.to_owned());
        }
        Ok(())
    }
}

fn has_scheme(text: &str) -> bool {
    text.split_once(':').is_some_and(|(scheme, _)| {
        let mut chars = scheme.bytes();
        chars.next().is_some_and(|b| b.is_ascii_alphabetic())
            && chars.all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))
    })
}

fn valid_cache(text: &str) -> bool {
    if matches!(text, "forever" | "off") {
        return true;
    }
    let Some((count, unit)) = text.split_at_checked(text.len().saturating_sub(1)) else {
        return false;
    };
    let scale = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        _ => return false,
    };
    count.bytes().all(|b| b.is_ascii_digit())
        && count
            .parse::<u64>()
            .ok()
            .and_then(|n| n.checked_mul(scale))
            .is_some()
}

fn header_token(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

fn valid_secret(map: &Mapping) -> bool {
    matches!(
        (map.len(), map.get("secret"), map.get("prefix")),
        (2, Some(Value::String(name)), Some(Value::String(prefix)))
            if is_valid_identifier(name) && !prefix.contains("{{")
    )
}

#[cfg(test)]
#[path = "decl_tests.rs"]
mod tests;
