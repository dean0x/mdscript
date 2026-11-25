//! Parse and check data declarations before replay or any I/O.

use std::mem::size_of;

use serde_yaml_ng::{Mapping, Value};

use super::encode::{check_header_value, EncodeRefusal};
use super::refs::{pieces, references, Piece};
use super::url::{self, UrlRefusal};
use super::{Accept, DataLabel};
use crate::error::data_location;
use crate::evaluator::budget::{MemBudget, Part};
use crate::limits::{
    MAX_DATA_ENTRIES_PER_MODULE, MAX_DATA_REFS_PER_ENTRY, MAX_REQUEST_HEADERS, MAX_URL_BYTES,
};
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
const METHOD_GRAMMAR: &str = "expected GET, HEAD, POST, PUT, PATCH, DELETE or OPTIONS";
const ACCEPT_GRAMMAR: &str = "expected json, yaml or text";
const HEADER_GRAMMAR: &str = "expected a string or a secret name with a literal prefix";

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Source {
    File(String),
    Url(Box<RequestTemplate>),
}

/// A network request as declared: literal text and `{{…}}` templates, never values.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RequestTemplate {
    /// The URL as declared, with its `{{…}}` text.
    pub(crate) url: String,
    pub(crate) method: DeclaredMethod,
    /// `json` when the declaration does not name a format.
    pub(crate) accept: Accept,
    /// Bind the status, the headers and the body of the response, not the body only.
    pub(crate) envelope: bool,
    /// In declaration order.
    pub(crate) headers: Vec<HeaderTemplate>,
    /// The body is parsed for its references only. The body encoder comes later.
    pub(crate) has_body: bool,
    /// The paths that the declaration references, in URL, header, then body order:
    /// at most `MAX_DATA_REFS_PER_ENTRY`.
    pub(crate) refs: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct HeaderTemplate {
    /// The header name as declared.
    pub(crate) name: String,
    pub(crate) value: HeaderSource,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HeaderSource {
    /// Header text with `{{…}}` references.
    Text(String),
    /// A host secret: the request sends the literal prefix, then the secret value.
    Secret { name: String, prefix: String },
}

/// The method of a request map. GET when the map does not name one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeclaredMethod {
    Get,
    Head,
    Post,
    Put,
    Patch,
    Delete,
    Options,
}

impl DeclaredMethod {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "GET" => Self::Get,
            "HEAD" => Self::Head,
            "POST" => Self::Post,
            "PUT" => Self::Put,
            "PATCH" => Self::Patch,
            "DELETE" => Self::Delete,
            "OPTIONS" => Self::Options,
            _ => return None,
        })
    }
}

/// The header names that carry a credential, in lower case. A declaration can set them
/// through the secret form only, and a redirect to another origin drops them.
pub(crate) const CREDENTIAL_HEADERS: [&str; 4] = [
    "authorization",
    "proxy-authorization",
    "cookie",
    "x-api-key",
];

/// `name` is one of [`CREDENTIAL_HEADERS`] in any ASCII letter case.
pub(crate) fn is_credential_header(name: &str) -> bool {
    CREDENTIAL_HEADERS
        .iter()
        .any(|known| name.eq_ignore_ascii_case(known))
}

/// The declaration form of a URL. Only a request map can name `http://`.
#[derive(Clone, Copy)]
enum UrlForm {
    Text,
    Map,
}

/// The text of `text` as an owned string, charged before the copy.
fn owned(text: &str, mem: &mut MemBudget) -> Result<String, MdsError> {
    mem.charge(Part::Temp, Some(text.len()))?;
    Ok(text.to_owned())
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
            input: crate::data::DataInput::Declaration,
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
        let template = match value {
            Value::String(path) if path.starts_with("./") || path.starts_with("../") => {
                if path.contains("{{") {
                    return Err(self.error(Some("path"), "file paths cannot contain `{{`"));
                }
                return Ok(Source::File(owned(path, mem)?));
            }
            Value::String(url) if has_scheme(url) || url.contains("://") => {
                let mut refs = Vec::new();
                self.parse_url(url, UrlForm::Text, &mut refs, mem)?;
                RequestTemplate {
                    url: owned(url, mem)?,
                    method: DeclaredMethod::Get,
                    accept: Accept::Json,
                    envelope: false,
                    headers: Vec::new(),
                    has_body: false,
                    refs,
                }
            }
            Value::Mapping(fields) => self.parse_request(fields, mem)?,
            _ => return Err(self.unknown_form()),
        };
        mem.charge(Part::Temp, Some(size_of::<RequestTemplate>()))?;
        Ok(Source::Url(Box::new(template)))
    }

    fn parse_request(
        self,
        fields: &Mapping,
        mem: &mut MemBudget,
    ) -> Result<RequestTemplate, MdsError> {
        let Some(url) = fields.get("url") else {
            return if fields.contains_key("method")
                || fields.contains_key("accept")
                || fields.contains_key("headers")
                || fields.contains_key("body")
                || fields.contains_key("cache")
                || fields.contains_key("envelope")
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
        let mut refs = Vec::new();
        self.parse_url(url, UrlForm::Map, &mut refs, mem)?;
        let mut method = DeclaredMethod::Get;
        let mut accept = Accept::Json;
        let mut envelope = false;
        for (key, value) in fields {
            let Value::String(field) = key else {
                return Err(self.error(None, "request field name must be a string"));
            };
            let reason = match (field.as_str(), value) {
                ("url" | "headers" | "body", _) => continue,
                ("method", Value::String(text)) => match DeclaredMethod::parse(text) {
                    Some(parsed) => {
                        method = parsed;
                        continue;
                    }
                    None => METHOD_GRAMMAR,
                },
                ("accept", Value::String(text)) => match parse_accept(text) {
                    Some(parsed) => {
                        accept = parsed;
                        continue;
                    }
                    None => ACCEPT_GRAMMAR,
                },
                ("cache", Value::String(s)) if valid_cache(s) => continue,
                ("envelope", Value::Bool(flag)) => {
                    envelope = *flag;
                    continue;
                }
                ("method", _) => METHOD_GRAMMAR,
                ("accept", _) => ACCEPT_GRAMMAR,
                ("cache", _) => "expected a duration in s, m, h or d, forever or off",
                ("envelope", _) => "expected true or false",
                _ => "unknown request field",
            };
            return Err(self.error(Some(field), reason));
        }
        let headers = match fields.get("headers") {
            Some(headers) => self.parse_headers(headers, &mut refs, mem)?,
            None => Vec::new(),
        };
        let has_body = match fields.get("body") {
            Some(body) => {
                self.check_quote(body, Some("body"))?;
                match body {
                    Value::Mapping(_) => self.parse_body_tree(body, 0, &mut refs, mem)?,
                    Value::String(s) if references(s).next().is_none() => (),
                    Value::String(_) => {
                        return Err(
                            self.error(Some("body"), "a string body cannot contain references")
                        )
                    }
                    _ => {
                        return Err(
                            self.error(Some("body"), "expected a mapping or a literal string")
                        )
                    }
                }
                true
            }
            None => false,
        };
        if has_body && matches!(method, DeclaredMethod::Get | DeclaredMethod::Head) {
            return Err(self.error(Some("body"), "a GET or HEAD request cannot have a body"));
        }
        if method == DeclaredMethod::Head && !envelope {
            // A HEAD response has no body, so its data is in the envelope only.
            return Err(self.error(Some("envelope"), "a HEAD request requires `envelope: true`"));
        }
        Ok(RequestTemplate {
            url: owned(url, mem)?,
            method,
            accept,
            envelope,
            headers,
            has_body,
            refs,
        })
    }

    fn parse_url(
        self,
        url: &str,
        form: UrlForm,
        refs: &mut Vec<String>,
        mem: &mut MemBudget,
    ) -> Result<(), MdsError> {
        let field = Some("url");
        let rest = match (
            url.strip_prefix("https://"),
            url.strip_prefix("http://"),
            form,
        ) {
            (Some(rest), _, _) | (None, Some(rest), UrlForm::Map) => rest,
            (_, _, UrlForm::Text) => {
                return Err(self.error(field, "expected the literal https scheme"))
            }
            (_, _, UrlForm::Map) => {
                return Err(self.error(field, "expected the literal https or http scheme"))
            }
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
        self.add_refs(suffix, "url", refs, mem)?;
        self.check_url_shape(url, mem)
    }

    /// Parse the literal URL with the strict URL parser, each reference as the shortest
    /// text that it can expand to: one byte in a path segment and none in the query. A
    /// host or port form that the parser refuses, or a URL that is already past the
    /// length cap, then fails here, on every declaration, before any request exists.
    fn check_url_shape(self, url: &str, mem: &mut MemBudget) -> Result<(), MdsError> {
        let mark = mem.mark();
        // A reference is at least five bytes and its stand-in at most one.
        mem.charge(Part::Temp, Some(url.len()))?;
        let mut probe = String::with_capacity(url.len());
        let mut in_query = false;
        for piece in pieces(url) {
            match piece {
                Ok(Piece::Literal(text)) => {
                    in_query |= text.contains('?');
                    probe.push_str(text);
                }
                Ok(Piece::Reference(_)) if in_query => {}
                Ok(Piece::Reference(_)) => probe.push('x'),
                Err(reason) => {
                    mem.release_to(mark);
                    return Err(self.error(Some("url"), reason));
                }
            }
        }
        let parsed = url::parse_absolute(&probe);
        mem.release_to(mark);
        match parsed {
            Ok(_) => Ok(()),
            Err(UrlRefusal::TooLong) => Err(MdsError::resource_limit(format!(
                "{}: URL exceeds the maximum length of {MAX_URL_BYTES} bytes",
                data_location(self.module, Some(self.entry), Some("url"))
            ))),
            Err(refusal) => Err(self.error(Some("url"), refusal.reason())),
        }
    }

    fn parse_headers(
        self,
        headers: &Value,
        refs: &mut Vec<String>,
        mem: &mut MemBudget,
    ) -> Result<Vec<HeaderTemplate>, MdsError> {
        self.check_quote(headers, Some("headers"))?;
        let Value::Mapping(headers) = headers else {
            return Err(self.error(Some("headers"), "expected a header mapping"));
        };
        if headers.len() > MAX_REQUEST_HEADERS {
            return Err(MdsError::resource_limit(format!(
                "{}: too many headers (max {MAX_REQUEST_HEADERS} per request)",
                data_location(self.module, Some(self.entry), Some("headers"))
            )));
        }
        mem.charge(
            Part::Temp,
            headers.len().checked_mul(size_of::<HeaderTemplate>()),
        )?;
        let mut kept = Vec::with_capacity(headers.len());
        for (key, value) in headers {
            let Value::String(name) = key else {
                return Err(self.error(Some("headers"), "header name must be a literal token"));
            };
            if !header_token(name) {
                return Err(self.error(Some(name), "header name must be a literal token"));
            }
            self.check_quote(value, Some(name))?;
            let source = match value {
                Value::String(_) if is_credential_header(name) => {
                    return Err(self.error(Some(name), "use the secret form for this header"))
                }
                Value::String(text) => {
                    self.add_refs(text, name, refs, mem)?;
                    HeaderSource::Text(owned(text, mem)?)
                }
                Value::Mapping(map) => match secret_parts(map) {
                    Some((_, prefix)) if check_header_value(prefix).is_err() => {
                        return Err(self.error(Some(name), EncodeRefusal::HeaderByte.reason()))
                    }
                    Some((secret, prefix)) => HeaderSource::Secret {
                        name: owned(secret, mem)?,
                        prefix: owned(prefix, mem)?,
                    },
                    None => return Err(self.error(Some(name), HEADER_GRAMMAR)),
                },
                _ => return Err(self.error(Some(name), HEADER_GRAMMAR)),
            };
            kept.push(HeaderTemplate {
                name: owned(name, mem)?,
                value: source,
            });
        }
        Ok(kept)
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
            // The graph check does work for each reference, so refuse here, before it.
            if refs.len() == MAX_DATA_REFS_PER_ENTRY {
                return Err(MdsError::resource_limit(format!(
                    "{}: too many references (max {MAX_DATA_REFS_PER_ENTRY} per entry)",
                    data_location(self.module, Some(self.entry), None)
                )));
            }
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

pub(super) fn has_scheme(text: &str) -> bool {
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

/// The secret name and the literal prefix of the exact `{secret, prefix}` shape.
fn secret_parts(map: &Mapping) -> Option<(&str, &str)> {
    match (map.len(), map.get("secret"), map.get("prefix")) {
        (2, Some(Value::String(name)), Some(Value::String(prefix)))
            if is_valid_identifier(name) && !prefix.contains("{{") =>
        {
            Some((name, prefix))
        }
        _ => None,
    }
}

fn parse_accept(text: &str) -> Option<Accept> {
    match text {
        "json" => Some(Accept::Json),
        "yaml" => Some(Accept::Yaml),
        "text" => Some(Accept::Text),
        _ => None,
    }
}

#[cfg(test)]
#[path = "decl_tests.rs"]
mod tests;
