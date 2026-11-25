//! The network of `mds build`: the `--allow-net` flags, the `mds.json` `network` section
//! that can only narrow them, and the driver loop of step compiles over the native sender.
//!
//! With no `--allow-net` the network stays off: each compile is a plain compile, and a
//! URL entry with no runtime value fails with `mds::network_denied`, whatever `mds.json`
//! holds. `mds watch`, `mds check`, `mds lint` and `mds fmt` have no network flags and
//! never send.
//!
//! The driver prints nothing. Each refusal is an `MdsError` that names a flag, a field
//! or a file, never a secret value; the core builds every message about a request.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use mds::{
    AllowItem, CompileOptions, CompileResult, FetchContext, FetchState, MdsError, NarrowError,
    NativeFs, NetworkPolicy, Request, Response, Secret, SecretValue, Step, TransportFailure, Value,
    COMPILE_DEADLINE, MAX_PARALLEL_SENDS, MAX_REQUESTS_PER_COMPILE, MAX_REQUESTS_PER_RUN,
    MAX_RESPONSE_BYTES,
};
use mds_fetch::{CertificateDer, Fetcher, SetupError};

use super::config::ProjectConfig;
use crate::output::{io_cause, safe_inline, safe_path};

/// The largest `--tls-root` file: 1 MiB, the cap of `mds.json`.
const MAX_ROOT_FILE_BYTES: u64 = 1024 * 1024;

/// The network flags of `mds build`, as typed.
#[derive(Debug, Default)]
pub(crate) struct NetFlags {
    /// `--allow-net`: the allow items. Empty keeps the network off.
    pub(crate) allow: Vec<String>,
    /// `--allow-net-imports`: URL entries of imported modules can send.
    pub(crate) allow_imports: bool,
    /// `--secret NAME=env:VAR@ORIGIN`, each as typed.
    pub(crate) secrets: Vec<String>,
    /// `--tls-root`: PEM files of extra trust roots.
    pub(crate) tls_roots: Vec<PathBuf>,
}

/// The network grant of one `mds build` run: the policy and the per-run key of every
/// step compile, the sender, and the count of the requests that the run sent.
pub(crate) struct NetRun {
    ctx: FetchContext,
    fetcher: Fetcher,
    sent: AtomicUsize,
}

impl NetRun {
    /// The network of a run: `None` with no `--allow-net`. Otherwise the flag grant,
    /// narrowed by the `network` section of the `mds.json` in force, with the host roots
    /// of `--tls-root` and a new run key.
    ///
    /// # Errors
    ///
    /// `mds::invalid_options` for a flag or a `network` key that the narrowing rule
    /// refuses, and `mds::io` or `mds::resource_limit` for a root file that can not be
    /// read.
    pub(crate) fn from_flags(
        flags: &NetFlags,
        config: Option<&ProjectConfig>,
    ) -> Result<Option<Self>, MdsError> {
        let Some(policy) = effective_policy(flags, config)? else {
            return Ok(None);
        };
        let (roots, owners) = read_roots(&flags.tls_roots)?;
        let fetcher = Fetcher::new(Arc::new(policy.clone()), &roots)
            .map_err(|reason| root_error(&flags.tls_roots, &owners, reason))?;
        let key = mds_fetch::run_key().map_err(|reason| MdsError::Io {
            message: format!("cannot start the network sender: {reason}"),
        })?;
        Ok(Some(Self {
            ctx: FetchContext::new(policy, key),
            fetcher,
            sent: AtomicUsize::new(0),
        }))
    }

    /// Run step compiles to the end: each round sends the requests that the compile
    /// needs and runs the compile again with every response. At most one round per
    /// request of a compile and one more. One deadline binds the network work of the
    /// compile from its first send; each request is sent once, with no retry. The state
    /// counts the raw bodies of the compile against one total, and each send reads at
    /// most its share of the bytes left.
    fn drive(
        &self,
        mut step: impl FnMut(&FetchContext, &FetchState) -> Result<Step, MdsError>,
    ) -> Result<CompileResult, MdsError> {
        let mut deadline = None;
        let mut state = FetchState::new();
        for _ in 0..=MAX_REQUESTS_PER_COMPILE {
            let requests = match step(&self.ctx, &state)? {
                Step::Done(result) => return Ok(*result),
                Step::Need(requests) if !requests.is_empty() => requests,
                // A round that needs nothing and is not done can not end.
                _ => return Err(MdsError::Internal),
            };
            let deadline = match deadline {
                Some(deadline) => deadline,
                None => *deadline.insert(
                    Instant::now()
                        .checked_add(COMPILE_DEADLINE)
                        .ok_or(MdsError::Internal)?,
                ),
            };
            self.reserve(requests.len())?;
            state = self.send_round(&requests, deadline, state)?;
        }
        // The core needs at most one round per request of a compile and one more.
        Err(MdsError::Internal)
    }

    /// Count `count` more requests of the run. Past the run cap, the round sends
    /// nothing and fails.
    fn reserve(&self, count: usize) -> Result<(), MdsError> {
        let before = self.sent.fetch_add(count, Ordering::SeqCst);
        if before.saturating_add(count) > MAX_REQUESTS_PER_RUN {
            return Err(MdsError::ResourceLimit {
                message: format!("one run sends at most {MAX_REQUESTS_PER_RUN} requests"),
            });
        }
        Ok(())
    }

    /// Send the requests of one round, at most [`MAX_PARALLEL_SENDS`] at a time, and add
    /// their responses to `state` in request order. Each group enters the state before
    /// the next group starts, and each send of a group reads at most its share of the
    /// body bytes that the state can still take, so a group in flight can not take the
    /// compile past its body total.
    ///
    /// # Errors
    ///
    /// The refusals of [`FetchState::with_response`] and of [`add_group`], and
    /// `MdsError::Internal` when a send thread panics. In `mds` the panic hook ends the
    /// process with the internal code first, as on any thread that catches nothing, so
    /// the error is the way out only where no hook is set.
    fn send_round(
        &self,
        requests: &[Request],
        deadline: Instant,
        mut state: FetchState,
    ) -> Result<FetchState, MdsError> {
        for group in requests.chunks(MAX_PARALLEL_SENDS) {
            let share = body_share(state.body_bytes_left(), group.len());
            let fetcher = self.fetcher.clone().with_body_cap(share);
            let fetcher = &fetcher;
            let mut responses = Vec::with_capacity(group.len());
            thread::scope(|scope| {
                let sends: Vec<_> = group
                    .iter()
                    .map(|request| {
                        thread::Builder::new()
                            .spawn_scoped(scope, move || send(fetcher, request, deadline))
                            .map_err(|_| request)
                    })
                    .collect();
                for send_thread in sends {
                    responses.push(match send_thread {
                        // Each thread is joined, so the scope itself never panics.
                        Ok(handle) => handle.join().map_err(|_| MdsError::Internal),
                        // With no thread for it, the request is sent on this thread.
                        Err(request) => Ok(send(fetcher, request, deadline)),
                    });
                }
            });
            state = add_group(state, group, responses, share)?;
        }
        Ok(state)
    }
}

/// Send one request before `deadline`. With no time left, it is not sent.
fn send(fetcher: &Fetcher, request: &Request, deadline: Instant) -> Response {
    if Instant::now() >= deadline {
        // The `mds::` path keeps this call apart from the `failed` methods of
        // mds-cli's lint sinks, which tests/panic_hook.rs follows by name.
        return mds::Response::failed(TransportFailure::Timeout);
    }
    fetcher.send(request, deadline)
}

/// The body cap of each of `sends` sends of one group: an equal share of the `left` body
/// bytes of the compile, at most the cap of one response. The shares of a group add up
/// to at most `left`.
fn body_share(left: usize, sends: usize) -> u64 {
    let share = left.checked_div(sends).unwrap_or(0);
    u64::try_from(share).map_or(MAX_RESPONSE_BYTES, |share| share.min(MAX_RESPONSE_BYTES))
}

/// Add the responses of one group, sent with a body cap of `share`, to `state` in
/// request order. A body past a share below the cap of one response would take the
/// compile past its body total, so it fails with the error of that total.
fn add_group(
    mut state: FetchState,
    group: &[Request],
    responses: Vec<Result<Response, MdsError>>,
    share: u64,
) -> Result<FetchState, MdsError> {
    for (request, response) in group.iter().zip(responses) {
        let response = response?;
        // A `#[non_exhaustive]` tuple variant of another crate matches in brace form.
        if share < MAX_RESPONSE_BYTES
            && matches!(
                response,
                Response::Failed {
                    0: TransportFailure::BodyTooLarge,
                    ..
                }
            )
        {
            return Err(MdsError::response_bodies_exceeded());
        }
        state = state.with_response(request.id(), response)?;
    }
    Ok(state)
}

/// Compile the native entry `input` with the filesystem that `fs` gives each compile.
/// With `net`, step compiles send the requests of the URL entries; without it, the
/// network stays off.
pub(crate) fn compile_native(
    fs: impl Fn() -> Result<NativeFs, MdsError>,
    input: &Path,
    runtime_vars: Option<HashMap<String, Value>>,
    opts: CompileOptions,
    net: Option<&NetRun>,
) -> Result<CompileResult, MdsError> {
    match net {
        None => mds::compile_native_fs(fs()?, input, runtime_vars, opts),
        Some(net) => net.drive(|ctx, state| {
            mds::compile_step_native_fs(
                fs()?,
                input,
                runtime_vars.clone(),
                opts.clone(),
                ctx,
                state,
            )
        }),
    }
}

/// Compile `source` (stdin) with the working directory as its base, as
/// [`compile_native`] compiles a file.
pub(crate) fn compile_source_text(
    source: &str,
    runtime_vars: Option<HashMap<String, Value>>,
    opts: CompileOptions,
    net: Option<&NetRun>,
) -> Result<CompileResult, MdsError> {
    match net {
        None => mds::compile_str_with_deps_opts(source, None, runtime_vars, opts),
        Some(net) => net.drive(|ctx, state| {
            mds::compile_step_str(source, None, runtime_vars.clone(), opts.clone(), ctx, state)
        }),
    }
}

/// The policy of a run: `None` with no `--allow-net`, so `mds.json` never opens the
/// network. Otherwise the flag grant, narrowed by the `network` section of `config`.
fn effective_policy(
    flags: &NetFlags,
    config: Option<&ProjectConfig>,
) -> Result<Option<NetworkPolicy>, MdsError> {
    if flags.allow.is_empty() {
        return Ok(None);
    }
    let mut items = Vec::with_capacity(flags.allow.len());
    for text in &flags.allow {
        match AllowItem::parse(text) {
            Ok(item) => items.push(item),
            Err(reason) => {
                return Err(refused(format!(
                    "--allow-net item \"{}\": {reason}",
                    safe_inline(text)
                )))
            }
        }
    }
    let mut policy = NetworkPolicy::new(items.clone())
        .map_err(|reason| refused(format!("--allow-net: {reason}")))?
        .with_imports(flags.allow_imports);
    for (index, text) in flags.secrets.iter().enumerate() {
        let (name, secret) = parse_secret(index, text)?;
        policy = policy.with_secret(secret).map_err(|reason| {
            refused(format!(
                "--secret[{index}] \"{}\": {reason}",
                safe_inline(name)
            ))
        })?;
    }
    let Some((section, shown)) = config.and_then(|config| {
        let section = config.config.network.as_ref()?;
        Some((section, safe_path(&config.shown_file())))
    }) else {
        return Ok(Some(policy));
    };
    let narrower = section.allow.clone().unwrap_or(items);
    let imports = section.allow_imports.unwrap_or(flags.allow_imports);
    policy
        .narrow(narrower, imports)
        .map(Some)
        .map_err(|reason| {
            refused(match reason {
                NarrowError::ItemNotAllowed { index } => format!(
                    "{shown} network.allow[{index}]: the item permits a request that the \
                 --allow-net list refuses"
                ),
                NarrowError::ImportsNotAllowed => {
                    format!("{shown} network.allow_imports: true needs --allow-net-imports")
                }
                other => format!("{shown} network.allow: {other}"),
            })
        })
}

/// One `--secret NAME=env:VAR@ORIGIN` at `index`: the host secret `NAME`, with the
/// value of the environment variable `VAR`, for requests to `ORIGIN` only. A message
/// names the index, `NAME` and `VAR`, never the value, and never the flag text, which
/// can hold a value typed by mistake.
fn parse_secret(index: usize, text: &str) -> Result<(&str, Secret), MdsError> {
    let malformed = || refused(format!("--secret[{index}]: expected NAME=env:VAR@ORIGIN"));
    let (name, rest) = text.split_once('=').ok_or_else(malformed)?;
    let (var, origin) = rest
        .strip_prefix("env:")
        .and_then(|rest| rest.split_once('@'))
        .ok_or_else(malformed)?;
    let at = |what: String| {
        refused(format!(
            "--secret[{index}] \"{}\": {what}",
            safe_inline(name)
        ))
    };
    // A text that is not a variable name can be a value typed in place of VAR, so the
    // message does not show it.
    if !is_env_name(var) {
        return Err(at(
            "the text after env: is not an environment variable name".to_owned(),
        ));
    }
    let origin = AllowItem::parse(origin)
        .map_err(|reason| at(format!("the origin: {reason}")))?
        .origin();
    let value = std::env::var_os(var)
        .ok_or_else(|| at(format!("the environment variable {var} is not set")))?;
    let value = value
        .into_string()
        .ok()
        .and_then(|value| SecretValue::new(value).ok())
        .ok_or_else(|| {
            at(format!(
                "the environment variable {var} must hold one or more visible ASCII characters"
            ))
        })?;
    let secret = Secret::new(name, value, origin).map_err(|reason| at(format!("{reason}")))?;
    Ok((name, secret))
}

/// `var` is a portable environment variable name: an ASCII letter or `_`, then ASCII
/// letters, digits or `_`.
fn is_env_name(var: &str) -> bool {
    let mut bytes = var.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// The roots of the `--tls-root` files, and for each root the index of its file.
fn read_roots(paths: &[PathBuf]) -> Result<(Vec<CertificateDer<'static>>, Vec<usize>), MdsError> {
    let mut roots = Vec::new();
    let mut owners = Vec::new();
    for (index, path) in paths.iter().enumerate() {
        let bytes = read_root_file(path)?;
        let parsed = mds_fetch::parse_pem_roots(&bytes)
            .map_err(|reason| refused(format!("--tls-root file {}: {reason}", safe_path(path))))?;
        owners.extend(std::iter::repeat_n(index, parsed.len()));
        roots.extend(parsed);
    }
    Ok((roots, owners))
}

/// The refusal of the sender setup: a root that is not a valid trust anchor names its
/// file.
fn root_error(paths: &[PathBuf], owners: &[usize], reason: SetupError) -> MdsError {
    let file = match reason {
        SetupError::Root { index } => owners.get(index).and_then(|owner| paths.get(*owner)),
        _ => None,
    };
    match file {
        Some(path) => refused(format!(
            "--tls-root file {}: a root is not a valid trust anchor",
            safe_path(path)
        )),
        None => refused(format!("--tls-root: {reason}")),
    }
}

/// The bytes of one `--tls-root` file, read as the `--vars` file is: a forbidden path
/// character and a symlink are refused, the file must be a regular file, and the read
/// stops one byte past the cap.
fn read_root_file(path: &Path) -> Result<Vec<u8>, MdsError> {
    let shown = safe_path(path);
    let canonical = NativeFs::check_symlink(path).map_err(|e| match e {
        MdsError::ImportError { .. } => MdsError::Io {
            message: format!("--tls-root file must not be a symlink: \"{shown}\""),
        },
        other => other,
    })?;
    let cannot_read = |e: std::io::Error| MdsError::Io {
        message: format!(
            "cannot read --tls-root file {shown}: {}",
            safe_inline(io_cause(&e))
        ),
    };
    let not_regular = || MdsError::Io {
        message: format!("--tls-root file {shown} is not a regular file"),
    };
    // A named pipe would block the open, so the type is checked first, and again on
    // the opened file.
    if !std::fs::symlink_metadata(&canonical)
        .map_err(cannot_read)?
        .is_file()
    {
        return Err(not_regular());
    }
    let mut file = std::fs::File::open(&canonical).map_err(cannot_read)?;
    let metadata = file.metadata().map_err(cannot_read)?;
    if !metadata.is_file() {
        return Err(not_regular());
    }
    let too_large = || MdsError::ResourceLimit {
        message: format!(
            "--tls-root file {shown} exceeds maximum size of {MAX_ROOT_FILE_BYTES} bytes"
        ),
    };
    if metadata.len() > MAX_ROOT_FILE_BYTES {
        return Err(too_large());
    }
    let bytes = mds::read_at_most(&mut file, MAX_ROOT_FILE_BYTES + 1, metadata.len())
        .map_err(cannot_read)?;
    if bytes.len() as u64 > MAX_ROOT_FILE_BYTES {
        return Err(too_large());
    }
    Ok(bytes)
}

/// A refused network option: `mds::invalid_options`, exit 2. The message is fixed text,
/// a flag or field name, and flag text escaped through `safe_inline`.
fn refused(message: String) -> MdsError {
    MdsError::invalid_options(&message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(allow: &[&str]) -> NetFlags {
        NetFlags {
            allow: allow.iter().map(|item| (*item).to_owned()).collect(),
            ..NetFlags::default()
        }
    }

    fn code(error: &MdsError) -> String {
        error.serialize().code
    }

    /// The network stays off with no `--allow-net`, also when `mds.json` names an
    /// origin and the import opt-in. Control: with the flag the run has a sender.
    #[test]
    fn no_flag_keeps_the_network_off() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("mds.json"),
            r#"{"network": {"allow": ["api.example"], "allow_imports": true}}"#,
        )
        .unwrap();
        let config = super::super::load_config(dir.path()).unwrap();
        assert!(NetRun::from_flags(&flags(&[]), config.as_ref())
            .unwrap()
            .is_none());
        let control = NetRun::from_flags(
            &NetFlags {
                allow_imports: true,
                ..flags(&["api.example"])
            },
            config.as_ref(),
        );
        assert!(
            control.unwrap().is_some(),
            "control: the flag opens the network"
        );
    }

    /// A run sends at most 512 requests: request 513 fails with `mds::resource_limit`
    /// before it is sent. Control: 512 pass.
    #[test]
    fn the_run_cap_refuses_request_513() {
        let run = NetRun::from_flags(&flags(&["api.example"]), None)
            .unwrap()
            .unwrap();
        assert!(
            run.reserve(MAX_REQUESTS_PER_RUN).is_ok(),
            "control: 512 pass"
        );
        let error = run.reserve(1).unwrap_err();
        assert_eq!(code(&error), "mds::resource_limit");
        assert!(
            error.to_string().contains("at most 512 requests"),
            "{error}"
        );
    }

    /// Each send of a group reads at most its share of the body bytes that the compile
    /// can still take, so the shares of a group add up to at most the bytes left.
    /// Control: with room, each send keeps the cap of one response.
    #[test]
    fn each_send_reads_at_most_its_share_of_the_bytes_left() {
        let total = FetchState::new().body_bytes_left();
        assert_eq!(
            body_share(total, MAX_PARALLEL_SENDS),
            MAX_RESPONSE_BYTES,
            "control: with room, a send keeps the cap of one response"
        );
        assert_eq!(
            body_share(40 * 1024 * 1024, MAX_PARALLEL_SENDS),
            5 * 1024 * 1024
        );
        for left in [0, 7, 8 * 1024, 79 * 1024 * 1024, total] {
            for sends in 1..=MAX_PARALLEL_SENDS {
                let share = body_share(left, sends);
                assert!(share <= MAX_RESPONSE_BYTES, "{left} bytes, {sends} sends");
                let shares = share * u64::try_from(sends).unwrap();
                assert!(
                    shares <= u64::try_from(left).unwrap(),
                    "{left} bytes, {sends} sends"
                );
            }
        }
    }

    /// A body past a share below the cap of one response fails with the body total error
    /// of the core, not the 10 MiB network text. Control: at the full cap the failure
    /// enters the state, and the core reports it for its entry.
    #[test]
    fn a_body_past_its_share_fails_with_the_body_total_error() {
        let policy = NetworkPolicy::new(vec![AllowItem::parse("api.example").unwrap()]).unwrap();
        let ctx = FetchContext::new(policy, mds::RunKey::from_bytes([7; 32]));
        let source = "---\ndata:\n  a: https://api.example/x\n---\n{{a}}\n";
        let step = mds::compile_step_str(
            source,
            None,
            None,
            CompileOptions::default(),
            &ctx,
            &FetchState::new(),
        )
        .unwrap();
        let Step::Need(requests) = step else {
            panic!("expected the request of the entry");
        };
        assert_eq!(requests.len(), 1);
        let too_large = || vec![Ok(mds::Response::failed(TransportFailure::BodyTooLarge))];
        let error = add_group(
            FetchState::new(),
            &requests,
            too_large(),
            MAX_RESPONSE_BYTES - 1,
        )
        .unwrap_err();
        assert_eq!(code(&error), "mds::resource_limit");
        assert_eq!(
            error.to_string(),
            "resource limit exceeded: response bodies exceed 209715200 bytes per compile"
        );
        let state = add_group(
            FetchState::new(),
            &requests,
            too_large(),
            MAX_RESPONSE_BYTES,
        );
        assert_eq!(
            state.unwrap().len(),
            1,
            "control: at the full cap the failure enters the state"
        );
    }

    /// A refused `--allow-net` item fails with `mds::invalid_options` and its fixed
    /// reason. Control: a valid item passes.
    #[test]
    fn a_refused_allow_item_names_the_flag() {
        let error = effective_policy(&flags(&["api.example", "HTTP://x"]), None).unwrap_err();
        assert_eq!(code(&error), "mds::invalid_options");
        assert!(
            error.to_string().contains("--allow-net item \"HTTP://x\""),
            "{error}"
        );
        assert!(effective_policy(&flags(&["api.example"]), None).is_ok());
    }

    /// A `--secret` with no variable set names the secret and the variable; a value typed
    /// in place of `env:VAR` is never shown. Each assertion message is fixed text, and the
    /// check for the value comes first, so a failure can not show the value.
    #[test]
    fn a_secret_flag_names_the_secret_and_the_variable_never_a_value() {
        let unset = NetFlags {
            secrets: vec!["TOKEN=env:MDS_TEST_UNSET_VARIABLE_405@https://api.example".into()],
            ..flags(&["api.example"])
        };
        let error = effective_policy(&unset, None).unwrap_err();
        assert_eq!(
            code(&error),
            "mds::invalid_options",
            "an unset variable is a refused option"
        );
        let text = error.to_string();
        assert!(
            text.contains("--secret[0] \"TOKEN\""),
            "the message must name the secret"
        );
        assert!(
            text.contains("MDS_TEST_UNSET_VARIABLE_405 is not set"),
            "the message must name the variable"
        );

        let typed_value = NetFlags {
            secrets: vec!["TOKEN=planted-secret-value".into()],
            ..flags(&["api.example"])
        };
        let Err(error) = effective_policy(&typed_value, None) else {
            panic!("a value typed in place of env:VAR must be refused");
        };
        let text = error.to_string();
        assert!(
            !text.contains("planted-secret-value"),
            "the message must not show the value"
        );
        assert!(
            text.contains("--secret[0]: expected NAME=env:VAR@ORIGIN"),
            "the message must name the flag form"
        );

        let value_as_var = NetFlags {
            secrets: vec!["TOKEN=env:planted-secret-value@https://api.example".into()],
            ..flags(&["api.example"])
        };
        let Err(error) = effective_policy(&value_as_var, None) else {
            panic!("a value typed in place of VAR must be refused");
        };
        let text = error.to_string();
        assert!(
            !text.contains("planted-secret-value"),
            "the message must not show the value"
        );
        assert_eq!(
            code(&error),
            "mds::invalid_options",
            "a value in place of VAR is a refused option"
        );
        assert!(
            text.contains(
                "--secret[0] \"TOKEN\": the text after env: is not an environment variable name"
            ),
            "the message must name the secret and the reason"
        );
    }
}
