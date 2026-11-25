//! The step engine: the requests of URL data entries and the binding of their responses.
//!
//! A step compile runs the whole resolver once per round. For each URL entry, in level
//! order, it derives the request, checks it against the network policy and looks it up
//! in the responses of the host. An entry with a response binds its value; an entry
//! with no response yet adds its request to the needed list and stays pending, and so
//! does each entry that references a pending entry. The host sends the needed requests
//! and runs the next round with every response it has, so the core keeps no state
//! between rounds.
//!
//! # Sinks of request text
//!
//! The expanded URL, a header value, a secret value and response text reach no sink but
//! the needed [`Request`] list, which is for the sender only. Each other sink has one
//! builder:
//!
//! - Error messages: [`request_label`] builds every message about a request
//!   (`mds::network`, `mds::network_denied`, the redirect limit) from the module, the
//!   entry, the URL as declared and the origin, through `request_location`. The help of
//!   a refusal is the fixed help of its check. A value error of a response names the
//!   entry and never a key path (`DataLabel::error`), and an envelope error names the
//!   field `headers` only.
//! - Warnings: a step compile adds none. The pending warning of the tooling roles names
//!   the module and the entry only.
//! - `Debug`: `Request` and `CheckedUrl` show the URL as declared and the origin, a text
//!   header value shows no text, [`Response`] and [`FetchState`] show sizes only, and a
//!   secret value and the run key show no value.
//! - Result fields: [`RemoteInput`] holds the template form, and `dependencies` holds
//!   files only.
//! - Lint JSON: the data rules read the declarations, never a response, and name the
//!   entry and the header name, never a value.
//! - CLI stderr: the CLI prints the messages above.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use indexmap::IndexMap;

use super::decl::{is_sender_header, HeaderSource, RequestTemplate};
use super::encode::{self, FieldLabel, Target};
use super::graph::Node;
use super::policy::{HopError, Layer, NetworkPolicy, Refusal, ADDRESS_REFUSED};
use super::request::RunKey;
use super::request::{identity_bytes, HeaderValue, Method, Request, RequestHeader, RequestId};
use super::url::{self, CheckedUrl, Host, Origin};
use super::{convert, envelope, Accept, DataInput, DataLabel};
use crate::error::request_location;
use crate::evaluator::budget::{MemBudget, Part};
use crate::limits::{
    MAX_HEADER_VALUE_BYTES, MAX_REDIRECTS, MAX_REQUESTS_PER_COMPILE, MAX_RESPONSE_BYTES,
    MAX_RESPONSE_HEADER_BYTES, MAX_URL_BYTES,
};
use crate::scope::VarLayer;
use crate::value::{Value, MAX_VALUE_DEPTH};
use crate::{CompileResult, MdsError};

/// The answer of the host to one request. The core checks every cap of the answer
/// again when a step compile reads it. Its `Debug` shows the status and the sizes only.
///
/// This enum is `#[non_exhaustive]`: obtain answers from [`Response::http`] and
/// [`Response::failed`].
#[non_exhaustive]
#[derive(Clone)]
pub enum Response {
    /// An HTTP answer: the status, the headers in received order and the body.
    #[non_exhaustive]
    Http {
        status: u16,
        headers: Vec<(String, Vec<u8>)>,
        body: Vec<u8>,
    },
    /// The request got no HTTP answer.
    #[non_exhaustive]
    Failed(TransportFailure),
}

impl Response {
    /// An HTTP answer: the status, the headers in received order and the body. The
    /// sender follows no redirect: a redirect answer is given as it is.
    #[must_use]
    pub fn http(status: u16, headers: Vec<(String, Vec<u8>)>, body: Vec<u8>) -> Self {
        Self::Http {
            status,
            headers,
            body,
        }
    }

    /// A request that got no HTTP answer.
    #[must_use]
    pub fn failed(failure: TransportFailure) -> Self {
        Self::Failed(failure)
    }
}

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Response::Http {
                status,
                headers,
                body,
            } => f
                .debug_struct("Http")
                .field("status", status)
                .field("headers", &headers.len())
                .field("body_bytes", &body.len())
                .finish(),
            Response::Failed(failure) => f.debug_tuple("Failed").field(failure).finish(),
        }
    }
}

/// Why a request got no HTTP answer. The core gives each kind its own fixed message.
///
/// This enum is `#[non_exhaustive]`: new kinds may be added in minor releases.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportFailure {
    Timeout,
    Connect,
    Tls,
    /// An address that the name lookup gave is in a refused range: see
    /// [`NetworkPolicy::check_address`]. One such address fails the request.
    AddressRefused,
    /// The body is longer than 10 MiB.
    BodyTooLarge,
    /// The status line and the headers are longer than 16 KiB.
    HeadersTooLarge,
    /// The answer is not valid HTTP.
    Protocol,
}

impl TransportFailure {
    /// The fixed text of the failure. It never holds response text.
    fn kind(self) -> String {
        match self {
            TransportFailure::Timeout => "the request timed out".into(),
            TransportFailure::Connect => "the connection failed".into(),
            TransportFailure::Tls => "the TLS handshake failed".into(),
            TransportFailure::Protocol => "the response is not valid HTTP".into(),
            TransportFailure::BodyTooLarge => {
                format!("the response body exceeds {MAX_RESPONSE_BYTES} bytes")
            }
            TransportFailure::HeadersTooLarge => {
                format!("the response headers exceed {MAX_RESPONSE_HEADER_BYTES} bytes")
            }
            TransportFailure::AddressRefused => ADDRESS_REFUSED.reason().into(),
        }
    }
}

/// One URL entry that a step compile bound, in template form: the module, the entry, the
/// URL as declared and its origin. It never holds the expanded URL.
///
/// This type is `#[non_exhaustive]`: obtain values from [`CompileResult::remote_inputs`].
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RemoteInput {
    /// The project-relative label of the declaring module.
    pub module: String,
    pub entry: String,
    /// The URL as declared, with its `{{…}}` text.
    pub template: String,
    /// The origin of the URL as declared.
    pub origin: String,
}

/// The network grant of one run of step compiles: the policy and the per-run key.
///
/// This type is `#[non_exhaustive]`: obtain values from [`FetchContext::new`].
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct FetchContext {
    policy: Arc<NetworkPolicy>,
    key: Arc<RunKey>,
}

impl FetchContext {
    #[must_use]
    pub fn new(policy: NetworkPolicy, key: RunKey) -> Self {
        Self {
            policy: Arc::new(policy),
            key: Arc::new(key),
        }
    }

    /// The policy, for the address check of a sender.
    #[must_use]
    pub fn policy(&self) -> &NetworkPolicy {
        &self.policy
    }

    /// The network state of one step compile with the responses of `state`.
    pub(crate) fn start(&self, state: &FetchState) -> StepCtx {
        StepCtx::new(
            Arc::clone(&self.policy),
            Arc::clone(&self.key),
            Arc::clone(&state.responses),
        )
    }
}

/// The responses that a host has for the requests of a step compile, keyed by request
/// ID. It holds responses only: each step compile derives every request again and runs
/// every check again. Its `Debug` shows the response count only.
///
/// This type is `#[non_exhaustive]`: obtain values from [`FetchState::new`].
#[non_exhaustive]
#[derive(Clone, Default)]
pub struct FetchState {
    responses: Arc<IndexMap<RequestId, Response>>,
}

impl FetchState {
    /// The start state, with no response.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The state with the response to the request `id` added. A second response for one
    /// ID fails with `mds::invalid_options`, and a response past the request cap of a
    /// compile (64) fails with `mds::resource_limit`.
    #[must_use = "a response can be refused"]
    pub fn with_response(mut self, id: RequestId, response: Response) -> Result<Self, MdsError> {
        if self.responses.contains_key(&id) {
            return Err(MdsError::invalid_options(
                "the fetch state already holds a response for this request",
            ));
        }
        if self.responses.len() >= MAX_REQUESTS_PER_COMPILE {
            return Err(MdsError::resource_limit(format!(
                "too many responses in the fetch state (max {MAX_REQUESTS_PER_COMPILE})"
            )));
        }
        Arc::make_mut(&mut self.responses).insert(id, response);
        Ok(self)
    }

    /// The number of responses.
    #[must_use]
    pub fn len(&self) -> usize {
        self.responses.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.responses.is_empty()
    }
}

impl fmt::Debug for FetchState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FetchState")
            .field("responses", &self.responses.len())
            .finish()
    }
}

/// The result of one step compile.
///
/// This enum is `#[non_exhaustive]`: new variants may be added in minor releases.
#[non_exhaustive]
#[derive(Debug)]
pub enum Step {
    /// Every URL entry is bound and the compile is complete.
    Done(Box<CompileResult>),
    /// The host sends these requests, adds each response to the [`FetchState`] and runs
    /// the next step.
    Need(Vec<Request>),
}

/// The network state of one step compile.
pub(crate) struct StepCtx {
    /// The engine that binds a URL entry. A compile reaches the engine through this
    /// pointer only, and only [`StepCtx::new`] sets it. So a build that never starts a
    /// step compile, such as the WASM build, keeps none of the engine code.
    bind: BindFn,
    policy: Arc<NetworkPolicy>,
    key: Arc<RunKey>,
    responses: Arc<IndexMap<RequestId, Response>>,
    /// The requests with no response yet, in derive order, each one once.
    needed: IndexMap<RequestId, Request>,
    /// The IDs of the responses that this compile used.
    consumed: HashSet<RequestId>,
    /// The value of each request of a bound chain, for the other entries of the compile,
    /// with the envelope flag of the entry: the request ID does not hold the flag, and
    /// one response gives a body value and an envelope value of two shapes.
    memo: HashMap<(RequestId, bool), Arc<Value>>,
    remote: Vec<RemoteInput>,
}

/// The result of a step compile.
#[derive(Debug)]
pub(crate) enum Outcome {
    /// Every URL entry is bound.
    Done(Vec<RemoteInput>),
    /// The host must send these requests and run the next round.
    Need(Vec<Request>),
}

impl StepCtx {
    pub(crate) fn new(
        policy: Arc<NetworkPolicy>,
        key: Arc<RunKey>,
        responses: Arc<IndexMap<RequestId, Response>>,
    ) -> Self {
        Self {
            bind: bind_with_engine,
            policy,
            key,
            responses,
            needed: IndexMap::new(),
            consumed: HashSet::new(),
            memo: HashMap::new(),
            remote: Vec::new(),
        }
    }

    /// The outcome of a compile that ended with `pending` data. A response that the
    /// compile did not use fails, so a host cannot give a value for a request that the
    /// core did not derive.
    pub(crate) fn finish(self, pending: bool) -> Result<Outcome, MdsError> {
        if self.responses.keys().any(|id| !self.consumed.contains(id)) {
            return Err(MdsError::invalid_options(
                "the fetch state holds a response that this compile did not request",
            ));
        }
        match (pending, self.needed.is_empty()) {
            (false, true) => Ok(Outcome::Done(self.remote)),
            (true, false) => Ok(Outcome::Need(self.needed.into_values().collect())),
            // Pending data with no request, or a request with no pending entry.
            _ => Err(MdsError::internal()),
        }
    }

    /// The requests that the compile needs so far, in derive order.
    #[cfg(test)]
    pub(crate) fn needed(&self) -> impl Iterator<Item = &Request> {
        self.needed.values()
    }
}

impl StepCtx {
    /// Count one new request ID against `MAX_REQUESTS_PER_COMPILE`, before it is added.
    fn count_new(&self) -> Result<(), MdsError> {
        if self.consumed.len() + self.needed.len() >= MAX_REQUESTS_PER_COMPILE {
            return Err(MdsError::resource_limit(format!(
                "too many requests (max {MAX_REQUESTS_PER_COMPILE} per compile)"
            )));
        }
        Ok(())
    }
}

/// The scope that the references of one entry read, and the facts of its module.
pub(crate) struct EntryScope<'s> {
    pub(crate) runtime: &'s VarLayer,
    /// The frontmatter variables and the entries that are already bound.
    pub(crate) vars: &'s VarLayer,
    /// The entries of this scope that are pending.
    pub(crate) pending: &'s HashSet<String>,
    /// The entry belongs to the module that the compile starts from.
    pub(crate) entry_module: bool,
}

impl<'s> EntryScope<'s> {
    /// The value of a reference path: the root from the runtime layer, else from the
    /// module variables, then each field.
    fn value(&self, path: &str) -> Option<&'s Value> {
        let mut segments = path.split('.');
        let root = segments.next()?;
        let mut value = self.runtime.value(root).or_else(|| self.vars.value(root))?;
        for segment in segments {
            let Value::Object(fields) = value else {
                return None;
            };
            value = fields.get(segment)?;
        }
        Some(value)
    }
}

/// What one URL entry binds.
#[derive(Debug)]
pub(crate) enum Bound {
    Value(Arc<Value>),
    /// The entry waits for a response, of its own request or of an entry it references.
    Pending,
}

/// The message site of one request of a URL entry: the module, the entry, the URL as
/// declared and the origin of the request.
pub(crate) struct RequestLabel<'a> {
    module: &'a str,
    entry: &'a str,
    template: &'a str,
    /// The origin of the request that the message is about.
    origin: String,
    /// The origin of the URL as declared.
    declared: &'a Origin,
}

/// The message site of a request to `origin` for the URL entry of `node`. Every message
/// about a URL entry request is built from it.
pub(crate) fn request_label<'a>(
    node: Node<'a>,
    template: &'a RequestTemplate,
    origin: &'a Origin,
) -> RequestLabel<'a> {
    RequestLabel {
        module: node.module,
        entry: &node.entry.name,
        template: &template.url,
        origin: origin.to_string(),
        declared: origin,
    }
}

impl RequestLabel<'_> {
    fn refused(&self, refusal: Refusal, secret: Option<&str>) -> MdsError {
        MdsError::refused_request(
            self.module,
            self.entry,
            self.template,
            &self.origin,
            refusal,
            secret,
        )
    }

    fn network(&self, kind: &str) -> MdsError {
        MdsError::network(self.module, self.entry, self.template, &self.origin, kind)
    }

    fn limit(&self, what: &str) -> MdsError {
        MdsError::resource_limit(format!(
            "{}: {what}",
            request_location(self.module, self.entry, self.template, &self.origin)
        ))
    }
}

/// Bind the URL entry of `node` from the responses of `ctx`, or add its request to the
/// needed list. `admit_bytes` counts the body bytes of a response against the data byte
/// cap of the compile. A request that fails a check never enters the needed list.
pub(crate) fn bind_url_entry(
    node: Node<'_>,
    template: &RequestTemplate,
    scope: &EntryScope<'_>,
    ctx: &mut StepCtx,
    mem: &mut MemBudget,
    mut admit_bytes: impl FnMut(usize) -> Result<(), MdsError>,
) -> Result<Bound, MdsError> {
    let bind = ctx.bind;
    bind(node, template, scope, ctx, mem, &mut admit_bytes)
}

/// The type of [`StepCtx::bind`].
type BindFn = fn(
    Node<'_>,
    &RequestTemplate,
    &EntryScope<'_>,
    &mut StepCtx,
    &mut MemBudget,
    &mut dyn FnMut(usize) -> Result<(), MdsError>,
) -> Result<Bound, MdsError>;

/// The body of [`bind_url_entry`], through [`StepCtx::bind`].
fn bind_with_engine(
    node: Node<'_>,
    template: &RequestTemplate,
    scope: &EntryScope<'_>,
    ctx: &mut StepCtx,
    mem: &mut MemBudget,
    admit_bytes: &mut dyn FnMut(usize) -> Result<(), MdsError>,
) -> Result<Bound, MdsError> {
    let mark = mem.mark();
    let bound = derive(node, template, scope, ctx, mem, admit_bytes);
    mem.release_to(mark);
    bound
}

fn derive(
    node: Node<'_>,
    template: &RequestTemplate,
    scope: &EntryScope<'_>,
    ctx: &mut StepCtx,
    mem: &mut MemBudget,
    admit_bytes: impl FnMut(usize) -> Result<(), MdsError>,
) -> Result<Bound, MdsError> {
    let module = node.module;
    let entry = node.entry.name.as_str();
    let has_secret = template
        .headers
        .iter()
        .any(|header| matches!(header.value, HeaderSource::Secret { .. }));
    if has_secret && !scope.entry_module {
        return Err(MdsError::data(
            module,
            Some(entry),
            Some("headers"),
            "a secret header is allowed in the entry module only",
            None,
        ));
    }
    if let Some(header) = template
        .headers
        .iter()
        .find(|header| is_sender_header(&header.name))
    {
        return Err(MdsError::data(
            module,
            Some(entry),
            Some(&header.name),
            "a request cannot set this header",
            None,
        ));
    }
    let literal = literal_origin(&template.url, mem)?;
    mem.charge(Part::Temp, Some(origin_text_bytes(&literal)))?;
    let label = request_label(node, template, &literal);
    let method = ctx
        .policy
        .check_method(template.method)
        .map_err(|refusal| label.refused(refusal, None))?;
    let blocked = template.refs.iter().any(|path| {
        let root = path.split_once('.').map_or(path.as_str(), |(root, _)| root);
        scope.pending.contains(root)
    });
    if blocked {
        return Ok(Bound::Pending);
    }

    let url_field = FieldLabel {
        module,
        entry,
        field: "url",
    };
    // The writer can grow its buffer to twice the cap before the cap check refuses.
    mem.charge(Part::Temp, Some(2 * MAX_URL_BYTES))?;
    let text = encode::expand(&template.url, Target::Url, url_field, |path| {
        reference(scope, url_field, path)
    })?;
    mem.charge(Part::Temp, Some(text.len()))?;
    let url = url::parse_absolute(&text)
        .map_err(|refusal| label.refused(Refusal::new(Layer::UrlParser, refusal.reason()), None))?;
    if url.origin() != literal {
        return Err(label.refused(
            Refusal::new(Layer::UrlParser, "the expanded URL has another origin"),
            None,
        ));
    }
    ctx.policy
        .check_origin(&literal, scope.entry_module)
        .map_err(|refusal| label.refused(refusal, None))?;
    let headers = request_headers(template, scope, &label, &ctx.policy, mem)?;
    let first = new_request(method, url, template.accept, headers, ctx, mem, &label)?;
    let shape = Shape {
        accept: template.accept,
        envelope: template.envelope,
    };
    follow(first, shape, node, label, ctx, mem, admit_bytes)
}

/// The headers of the first request of a URL entry, in declaration order: each text
/// header expanded through the header encoder, and each secret header bound to the
/// declared origin and bounded by `MAX_HEADER_VALUE_BYTES`, as a descriptor only.
fn request_headers(
    template: &RequestTemplate,
    scope: &EntryScope<'_>,
    label: &RequestLabel<'_>,
    policy: &NetworkPolicy,
    mem: &mut MemBudget,
) -> Result<Vec<RequestHeader>, MdsError> {
    mem.charge(
        Part::Temp,
        template
            .headers
            .len()
            .checked_mul(size_of::<RequestHeader>()),
    )?;
    let mut headers = Vec::with_capacity(template.headers.len());
    for header in &template.headers {
        let field = FieldLabel {
            module: label.module,
            entry: label.entry,
            field: &header.name,
        };
        let value = match &header.value {
            HeaderSource::Text(text) => {
                mem.charge(Part::Temp, Some(2 * MAX_HEADER_VALUE_BYTES))?;
                HeaderValue::Text(encode::expand(text, Target::Header, field, |path| {
                    reference(scope, field, path)
                })?)
            }
            HeaderSource::Secret { name, prefix } => {
                let secret = policy
                    .bind_secret(name, label.declared)
                    .map_err(|refusal| label.refused(refusal, Some(name)))?;
                if prefix.len() + secret.expose_for_send().len() > MAX_HEADER_VALUE_BYTES {
                    return Err(field.limit("header value", MAX_HEADER_VALUE_BYTES));
                }
                mem.charge(Part::Temp, name.len().checked_add(prefix.len()))?;
                HeaderValue::Secret {
                    name: name.clone(),
                    prefix: prefix.clone(),
                }
            }
        };
        mem.charge(Part::Temp, Some(header.name.len()))?;
        headers.push(RequestHeader::new(header.name.clone(), value));
    }
    Ok(headers)
}

/// The value form of a URL entry: the body format and the envelope flag.
#[derive(Clone, Copy)]
struct Shape {
    accept: Accept,
    envelope: bool,
}

/// The value of the reference `path` of a template field. A path that names no value
/// fails: a request never sends a guessed text.
fn reference<'s>(
    scope: &EntryScope<'s>,
    field: FieldLabel<'_>,
    path: &str,
) -> Result<&'s Value, MdsError> {
    scope.value(path).ok_or_else(|| {
        MdsError::data(
            field.module,
            Some(field.entry),
            Some(field.field),
            &format!(
                "the reference \"{}\" names no value",
                crate::sanitize_control_chars_wire(path)
            ),
            None,
        )
    })
}

/// Follow the redirect chain of `request` through the responses of `ctx`, at most
/// `MAX_REDIRECTS` hops. Each hop is a new request with its own ID and checks. With the
/// envelope, the final answer binds as `{status, headers, body}` for any status from
/// 200 to 599; without it, a status that is not 2xx fails.
fn follow(
    mut request: Request,
    shape: Shape,
    node: Node<'_>,
    mut label: RequestLabel<'_>,
    ctx: &mut StepCtx,
    mem: &mut MemBudget,
    mut admit_bytes: impl FnMut(usize) -> Result<(), MdsError>,
) -> Result<Bound, MdsError> {
    let responses = Arc::clone(&ctx.responses);
    let mut chain = [None; MAX_REDIRECTS + 1];
    for hop in 0..=MAX_REDIRECTS {
        let id = request.id();
        chain[hop] = Some(id);
        if let Some(value) = ctx.memo.get(&(id, shape.envelope)) {
            let value = Arc::clone(value);
            return bind_value(value, &chain[..=hop], shape, node, &label, ctx, mem);
        }
        if ctx.needed.contains_key(&id) {
            return Ok(Bound::Pending);
        }
        let Some(response) = responses.get(&id) else {
            ctx.count_new()?;
            mem.charge(Part::Buffers, retained_bytes(&request))?;
            ctx.needed.insert(id, request);
            return Ok(Bound::Pending);
        };
        if !ctx.consumed.contains(&id) {
            ctx.count_new()?;
            mem.charge(Part::Buffers, Some(2 * size_of::<RequestId>()))?;
            ctx.consumed.insert(id);
        }
        let (status, headers, body) = match response {
            Response::Failed(TransportFailure::AddressRefused) => {
                return Err(label.refused(ADDRESS_REFUSED, None));
            }
            Response::Failed(failure) => return Err(label.network(&failure.kind())),
            Response::Http {
                status,
                headers,
                body,
            } => (*status, headers, body),
        };
        let header_bytes = headers.iter().try_fold(0usize, |sum, (name, value)| {
            sum.checked_add(name.len())?.checked_add(value.len() + 4)
        });
        if header_bytes.is_none_or(|bytes| bytes > MAX_RESPONSE_HEADER_BYTES) {
            return Err(label.network(&TransportFailure::HeadersTooLarge.kind()));
        }
        let bound = match status {
            301 | 302 | 303 | 307 | 308 => {
                let next = redirect(&request, shape.accept, headers, hop, &label, ctx, mem)?;
                let origin = next.url().origin();
                mem.charge(Part::Temp, Some(origin_text_bytes(&origin)))?;
                label.origin = origin.to_string();
                request = next;
                continue;
            }
            200..=299 => true,
            300..=599 => shape.envelope,
            _ => false,
        };
        if !bound {
            return Err(label.network(&format!("the server answered with HTTP status {status}")));
        }
        if u64::try_from(body.len()).map_or(true, |len| len > MAX_RESPONSE_BYTES) {
            return Err(label.network(&TransportFailure::BodyTooLarge.kind()));
        }
        let data = DataLabel {
            module: node.module,
            entry: &node.entry.name,
            input: DataInput::Response,
        };
        // A HEAD answer has no body, so its envelope holds `body: null`.
        let value = if request.method() == Method::Head {
            Value::Null
        } else {
            admit_bytes(body.len())?;
            convert(body, shape.accept, data, mem)?
        };
        let value = if shape.envelope {
            envelope::envelope(status, headers, value, data, mem)?
        } else {
            value
        };
        if let Some(secret) = scan_secrets(&value, &ctx.policy)? {
            return Err(label.refused(response_check(), Some(secret)));
        }
        mem.charge(
            Part::Buffers,
            Some(size_of::<Value>() + 2 * size_of::<usize>()),
        )?;
        return bind_value(
            Arc::new(value),
            &chain[..=hop],
            shape,
            node,
            &label,
            ctx,
            mem,
        );
    }
    // The hop check refuses the hop past `MAX_REDIRECTS`, so the loop always returns.
    Err(MdsError::internal())
}

/// The next request of a redirect response to `request`, after the location, the hop
/// and the header checks.
fn redirect(
    request: &Request,
    accept: Accept,
    headers: &[(String, Vec<u8>)],
    hop: usize,
    label: &RequestLabel<'_>,
    ctx: &StepCtx,
    mem: &mut MemBudget,
) -> Result<Request, MdsError> {
    let mut locations = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("location"));
    let (Some((_, location)), None) = (locations.next(), locations.next()) else {
        return Err(label.refused(
            Refusal::new(
                Layer::RedirectCheck,
                "a redirect needs exactly one location header",
            ),
            None,
        ));
    };
    let Ok(location) = std::str::from_utf8(location) else {
        return Err(label.refused(
            Refusal::new(
                Layer::UrlParser,
                "URL can contain only visible ASCII characters",
            ),
            None,
        ));
    };
    if let Some(secret) = find_secret(location, &ctx.policy) {
        return Err(label.refused(response_check(), Some(secret)));
    }
    mem.charge(
        Part::Temp,
        location
            .len()
            .checked_add(origin_text_bytes(&request.url().origin()))
            .and_then(|bytes| bytes.checked_mul(2)),
    )?;
    let next = url::parse_location(request.url(), location)
        .map_err(|refusal| label.refused(Refusal::new(Layer::UrlParser, refusal.reason()), None))?;
    let rule = ctx
        .policy
        .check_hop(request.url(), &next, hop + 1)
        .map_err(|error| match error {
            HopError::TooMany => label.limit(&format!(
                "too many redirects (max {MAX_REDIRECTS} per entry)"
            )),
            HopError::Refused(refusal) => label.refused(refusal, None),
        })?;
    let mut kept = Vec::with_capacity(request.headers().len());
    for header in request.headers() {
        let secret = matches!(header.value(), HeaderValue::Secret { .. });
        if rule.keeps_header(header.name(), secret) {
            mem.charge(Part::Temp, header_bytes(header))?;
            kept.push(header.clone());
        }
    }
    new_request(request.method(), next, accept, kept, ctx, mem, label)
}

/// Build a request after a charge of the bound of its identity text.
fn new_request(
    method: Method,
    url: CheckedUrl,
    accept: Accept,
    headers: Vec<RequestHeader>,
    ctx: &StepCtx,
    mem: &mut MemBudget,
    label: &RequestLabel<'_>,
) -> Result<Request, MdsError> {
    mem.charge(Part::Temp, identity_bytes(&url, &headers))?;
    mem.charge(Part::Temp, Some(label.template.len()))?;
    Request::new(
        method,
        label.template,
        url,
        accept,
        headers,
        &ctx.key,
        &ctx.policy,
    )
    .map_err(|refusal| label.refused(refusal, None))
}

/// Bind `value` for the entry of `node`: keep it for each request ID of the chain and
/// add the remote input of the entry.
fn bind_value(
    value: Arc<Value>,
    chain: &[Option<RequestId>],
    shape: Shape,
    node: Node<'_>,
    label: &RequestLabel<'_>,
    ctx: &mut StepCtx,
    mem: &mut MemBudget,
) -> Result<Bound, MdsError> {
    for id in chain.iter().flatten() {
        if let Entry::Vacant(slot) = ctx.memo.entry((*id, shape.envelope)) {
            mem.charge(
                Part::Buffers,
                Some(2 * size_of::<((RequestId, bool), Arc<Value>, usize)>()),
            )?;
            slot.insert(Arc::clone(&value));
        }
    }
    let template = label.template;
    mem.charge(Part::Temp, Some(origin_text_bytes(label.declared)))?;
    let origin = label.declared.to_string();
    let bytes = node
        .module
        .len()
        .checked_add(node.entry.name.len())
        .and_then(|sum| sum.checked_add(template.len()))
        .and_then(|sum| sum.checked_add(origin.len()))
        .and_then(|sum| sum.checked_add(2 * size_of::<RemoteInput>()));
    mem.charge(Part::Buffers, bytes)?;
    ctx.remote.push(RemoteInput {
        module: node.module.to_owned(),
        entry: node.entry.name.clone(),
        template: template.to_owned(),
        origin,
    });
    Ok(Bound::Value(value))
}

/// The name of the first host secret whose value is in `value`, in a string or in an
/// object key, as an exact byte match. A changed form of the value (base64, percent
/// encoding, a split over two strings) is not found. The walk is bounded by
/// `MAX_VALUE_DEPTH`; a deeper value fails closed.
pub(crate) fn scan_secrets<'p>(
    value: &Value,
    policy: &'p NetworkPolicy,
) -> Result<Option<&'p str>, MdsError> {
    fn walk<'p>(
        value: &Value,
        policy: &'p NetworkPolicy,
        depth: usize,
    ) -> Result<Option<&'p str>, MdsError> {
        if depth > MAX_VALUE_DEPTH {
            return Err(MdsError::internal());
        }
        match value {
            Value::String(text) => Ok(find_secret(text, policy)),
            Value::Array(items) => {
                for item in items {
                    if let Some(name) = walk(item, policy, depth + 1)? {
                        return Ok(Some(name));
                    }
                }
                Ok(None)
            }
            Value::Object(fields) => {
                for (key, item) in fields {
                    if let Some(name) = find_secret(key, policy) {
                        return Ok(Some(name));
                    }
                    if let Some(name) = walk(item, policy, depth + 1)? {
                        return Ok(Some(name));
                    }
                }
                Ok(None)
            }
            Value::Number(_) | Value::Boolean(_) | Value::Null => Ok(None),
        }
    }
    walk(value, policy, 0)
}

/// The name of the first host secret whose value is in `text`, as an exact byte match.
fn find_secret<'p>(text: &str, policy: &'p NetworkPolicy) -> Option<&'p str> {
    policy
        .secrets()
        .find(|(_, secret)| text.contains(secret.expose_for_send()))
        .map(|(name, _)| name)
}

fn response_check() -> Refusal {
    Refusal::new(
        Layer::ResponseCheck,
        "the response holds the value of a host secret",
    )
}

/// The origin of the literal URL of a template: the declaration parser keeps the
/// authority literal, so a failure here is an internal error.
fn literal_origin(template: &str, mem: &mut MemBudget) -> Result<Origin, MdsError> {
    let scheme = if template.starts_with("https://") {
        "https://".len()
    } else if template.starts_with("http://") {
        "http://".len()
    } else {
        return Err(MdsError::internal());
    };
    let end = template[scheme..]
        .find(['/', '?'])
        .map_or(template.len(), |index| scheme + index);
    mem.charge(Part::Temp, Some(end + 1))?;
    url::parse_absolute(&template[..end])
        .map(|url| url.origin())
        .map_err(|_| MdsError::internal())
}

/// An upper bound of the text of `origin`: the scheme, the host and a port.
fn origin_text_bytes(origin: &Origin) -> usize {
    let host = match &origin.host {
        Host::Name(name) => name.len(),
        Host::V4(_) | Host::V6(_) => 47,
    };
    host + "https://:65535".len()
}

/// An upper bound of the bytes of one header.
fn header_bytes(header: &RequestHeader) -> Option<usize> {
    let value = match header.value() {
        HeaderValue::Text(text) => text.len(),
        HeaderValue::Secret { name, prefix } => name.len().checked_add(prefix.len())?,
    };
    header
        .name()
        .len()
        .checked_add(value)?
        .checked_add(size_of::<RequestHeader>())
}

/// An upper bound of the bytes that a request in the needed list holds.
fn retained_bytes(request: &Request) -> Option<usize> {
    let url = request.url();
    let host = match &url.host {
        Host::Name(name) => name.len(),
        Host::V4(_) | Host::V6(_) => 0,
    };
    let mut bytes = host
        .checked_add(url.path().len())?
        .checked_add(request.template().len())?
        .checked_add(url.query().map_or(0, str::len))?
        .checked_add(2 * size_of::<(RequestId, Request, usize)>())?;
    for header in request.headers() {
        bytes = bytes.checked_add(header_bytes(header)?)?;
    }
    Some(bytes)
}
