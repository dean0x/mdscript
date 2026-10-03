//! Build subcommand implementation and shared compilation helpers.
//!
//! All helpers in this module are `pub(crate)` so that `watch.rs` can reuse them
//! without duplicating logic or bypassing resource limits.
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};

use crate::output::WriteTarget;
use crate::write::{atomic_write_file, Durability, NotRemoved, Parents, Removal};
use mds::{
    effective_parent, CompiledOutput, MdsError, MAX_FILE_SIZE, MAX_TRAVERSAL_DEPTH,
    STRING_SOURCE_MAP_LABEL,
};
use miette::Result;
use serde::Deserialize;

// ── Project config (mds.json) ─────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
pub(crate) struct MdsConfig {
    #[serde(default)]
    pub(crate) build: BuildConfig,
    /// Loaded (and validated — a malformed `fmt` section still fails config
    /// loading) for forward-compatibility, but not yet consulted by `mds fmt`
    /// — see `FmtConfig`.
    #[allow(
        dead_code,
        reason = "scaffolding for a rule not implemented until a future version"
    )]
    #[serde(default)]
    pub(crate) fmt: FmtConfig,
    /// Per-rule severity overrides for `mds lint` (AC-F-17).
    ///
    /// Unknown severity VALUES fail config loading loudly (see [`LintCliConfig`]).
    /// Unknown rule NAMES: only `mds lint` warns on stderr and continues — single-file
    /// mode via `load_lint_config`, directory mode via `LintDirCtx::config_for`.
    /// `mds build`, `check`, `fmt`, and `watch` load this field but do not emit the
    /// warning — an accepted asymmetry, not an oversight (see CHANGELOG).
    #[serde(default)]
    pub(crate) lint: LintCliConfig,
}

/// mds.json `lint` section: per-rule severity overrides.
///
/// Mirrors the core `LintConfig` shape but lives in the CLI so it can be
/// loaded alongside `BuildConfig` / `FmtConfig` as part of `MdsConfig`.
///
/// Unknown severity VALUES (e.g. `"banana"`) fail config loading (exit 1), because
/// `Severity` is a closed enum with no sensible fallback. Unknown rule NAMES: only
/// `mds lint` warns on stderr and continues — single-file mode via
/// `load_lint_config`, directory mode via `LintDirCtx::config_for`. `mds build`,
/// `check`, `fmt`, and `watch` deserialize this struct but do not emit the
/// warning — an accepted asymmetry, not an oversight (see CHANGELOG).
#[derive(Debug, Default, Deserialize)]
pub(crate) struct LintCliConfig {
    #[serde(default, deserialize_with = "deserialize_lint_rules")]
    pub(crate) rules: HashMap<String, mds::Severity>,
}

/// `lint.rules` read by [`mds::parse_rule_severities`], the reader the napi, WASM and
/// Python bindings use for their `rules` option (#175, #418): a value that is not one
/// of the four severity spellings fails with its error, which names the rule and the
/// value — `lint.rules["<name>"]: unknown severity "<value>"; …`, or `lint.rules["<name>"]
/// must be a severity string, got <type>` — each WIRE-escaped as the message is built.
/// serde_json appends the line and column it had reached once the map was read.
fn deserialize_lint_rules<'de, D>(
    deserializer: D,
) -> std::result::Result<HashMap<String, mds::Severity>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let rules = serde_json::Map::<String, serde_json::Value>::deserialize(deserializer)?;
    mds::parse_rule_severities(rules, "lint.rules").map_err(serde::de::Error::custom)
}

impl LintCliConfig {
    /// Convert to the core `LintConfig` consumed by `mds::lint_*` functions,
    /// returning any unknown rule names alongside it.
    ///
    /// Uses [`mds::LintConfig::from_rules_checked`] so the caller receives both
    /// the config and the unknowns report in one step, rather than building the
    /// config and optionally invoking a separate check. This closes the gap
    /// identified in the review finding for config.rs:104 — a consumer could
    /// previously skip detection silently by not invoking the separate check.
    /// Note: `#[must_use]` on `from_rules_checked` fires only when the entire
    /// return value is dropped; `let (config, _) = …` silently discards the
    /// `Option<mds::UnknownRuleNames>` and is the caller's own choice (as
    /// config.rs:249 states: the `#[must_use]` only warns "if the return value
    /// is discarded entirely").
    pub(crate) fn into_core_config(self) -> (mds::LintConfig, Option<mds::UnknownRuleNames>) {
        mds::LintConfig::from_rules_checked(self.rules)
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct BuildConfig {
    pub(crate) output_dir: Option<String>,
    /// Enable source-map generation for all builds (equivalent to `--source-map`).
    #[serde(default)]
    pub(crate) source_map: bool,
    /// Embed source text in the source map (equivalent to `--embed-sources`).
    #[serde(default)]
    pub(crate) embed_sources: bool,
}

/// Forward-compatibility scaffolding for `mds fmt` configuration.
///
/// `sort_frontmatter_keys` is not wired into any formatting behavior yet — the
/// v1 ruleset (R1-R4, see `mds-core`'s `formatter` module) deliberately defers
/// frontmatter key sorting to a future version, and there is intentionally no
/// matching CLI flag (a no-op flag would be a clippy/UX liability). The field
/// exists now purely so `{"fmt": {"sort_frontmatter_keys": false}}` parses
/// cleanly today and this won't need a breaking `mds.json` schema change once
/// the rule ships.
#[derive(Debug, Deserialize)]
pub(crate) struct FmtConfig {
    #[allow(
        dead_code,
        reason = "scaffolding for a rule not implemented until a future version"
    )]
    #[serde(default = "default_sort_frontmatter_keys")]
    pub(crate) sort_frontmatter_keys: bool,
}

impl Default for FmtConfig {
    fn default() -> Self {
        Self {
            sort_frontmatter_keys: default_sort_frontmatter_keys(),
        }
    }
}

fn default_sort_frontmatter_keys() -> bool {
    true
}

/// Maximum allowed size for `mds.json` (1 MiB) to prevent runaway memory use.
const MAX_CONFIG_SIZE: u64 = 1024 * 1024;

/// A loaded `mds.json` and the directory that contains it, in two forms.
pub(crate) struct ProjectConfig {
    pub(crate) config: MdsConfig,
    /// The directory, canonical: relative `output_dir` values are resolved against it.
    pub(crate) dir: PathBuf,
    /// The same directory as the start path leads to it — `.`, `sub/..`, one `..` per
    /// step up: the only form a message names it, or a path below it, by (#265, #390).
    pub(crate) shown_dir: PathBuf,
}

impl ProjectConfig {
    /// `mds.json` itself, as a message names it: `./mds.json`, `sub/../mds.json`.
    pub(crate) fn shown_file(&self) -> PathBuf {
        self.shown_dir.join("mds.json")
    }
}

/// Walk up from `start` looking for `mds.json`.
///
/// Returns `Ok(Some(config))` when found, `Ok(None)` when no `mds.json` exists in the
/// hierarchy, or `Err(...)` when a file is found but contains invalid JSON.
///
/// Every error names the file by the path `start` leads to it — `./mds.json`,
/// `sub/../mds.json` — escaped by [`crate::output::safe_path`], never by the
/// canonical path the walk uses: that one is absolute, which the caller did not
/// type, and it can carry a forbidden character from a hostile-named directory above
/// the project, since the config loads before the input is validated (#265). The
/// returned [`ProjectConfig`] carries that directory as well, so a later message names
/// an output below it the same way (#390).
pub(crate) fn load_config(start: &Path) -> Result<Option<ProjectConfig>> {
    // Walk upward from `start` (which may be a file; begin at its parent).
    // avoids PF-006: a relative start_dir (e.g. "" or ".") causes current.parent()
    // to return None after just 1–2 iterations, making grandparent mds.json
    // unreachable even when MAX_TRAVERSAL_DEPTH would allow it.  Canonicalize
    // to an absolute path first so every parent() step advances one real directory.
    let raw_start_dir = if start.is_dir() {
        start.to_path_buf()
    } else {
        effective_parent(start).to_path_buf()
    };

    // The directory as `start` names it, one `..` per step up: what an error shows.
    let mut shown_dir = raw_start_dir.clone();
    let mut current = match raw_start_dir.canonicalize() {
        Ok(p) => p,
        Err(_) => raw_start_dir,
    };
    // Cap prevents unbounded traversal on unusual filesystems.
    for _ in 0..MAX_TRAVERSAL_DEPTH {
        let candidate = current.join("mds.json");
        if candidate.is_file() {
            let shown = crate::output::safe_path(&shown_dir.join("mds.json"));
            // The size is taken from the opened file, and the read stops one byte past
            // the cap into a buffer that never grows past it, so an oversized mds.json
            // — or one that grows while it is read — is never held in memory whole
            // (#428).
            let cannot_read = |e: std::io::Error| {
                miette::miette!(
                    "cannot read {shown}: {}",
                    crate::output::safe_inline(crate::output::io_cause(&e))
                )
            };
            let too_large = |size: u64| {
                miette::miette!("mds.json at {shown} is too large ({size} bytes; maximum is 1 MiB)")
            };
            let mut file = std::fs::File::open(&candidate).map_err(cannot_read)?;
            let size = file.metadata().map_err(cannot_read)?.len();
            if size > MAX_CONFIG_SIZE {
                return Err(too_large(size));
            }
            let bytes =
                mds::read_at_most(&mut file, MAX_CONFIG_SIZE + 1, size).map_err(cannot_read)?;
            if bytes.len() as u64 > MAX_CONFIG_SIZE {
                return Err(too_large(bytes.len() as u64));
            }
            let raw = String::from_utf8(bytes).map_err(|e| {
                miette::miette!(
                    "invalid UTF-8 in {shown}: {}",
                    crate::output::safe_inline(&e)
                )
            })?;
            let config: MdsConfig = serde_json::from_str(&raw).map_err(|e| {
                miette::miette!(
                    "invalid mds.json at {shown}: {}",
                    crate::output::safe_inline(&e)
                )
            })?;
            // #265: a `build.output_dir` carrying a forbidden path character — as
            // written, or in the form it resolves to under the config directory (a
            // symlink into a hostile-named directory) — is refused
            // here, at load — `mds::io`, exit 2 — so it never reaches output-path
            // derivation. `load_config` is shared, so this fails every run that loads
            // mds.json, not only the ones that write output: `build`, `watch`, `lint`
            // (every input mode) and `fmt` directory mode. `check` does not load
            // mds.json.
            if let Some(output_dir) = &config.build.output_dir {
                let typed = std::ffi::OsStr::new(output_dir);
                crate::output::reject_forbidden_output_path("mds.json build.output_dir", typed)?;
                crate::output::reject_forbidden_resolved_output_path(
                    "mds.json build.output_dir",
                    &current.join(output_dir),
                    typed,
                )?;
            }
            return Ok(Some(ProjectConfig {
                config,
                dir: current,
                shown_dir,
            }));
        }
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => break,
        }
        shown_dir.push("..");
    }
    Ok(None)
}

// ── Output path resolution ────────────────────────────────────────────────────

/// The output kind, derived intrinsically from the compiled output variant.
///
/// This is separate from `CompiledOutput` so callers can carry the kind
/// through the single-file path-derivation pipeline without the actual content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputKind {
    Markdown,
    Messages,
}

impl OutputKind {
    /// File extension for this kind (without leading `.`).
    pub(crate) fn extension(self) -> &'static str {
        match self {
            OutputKind::Markdown => "md",
            OutputKind::Messages => "json",
        }
    }

    /// Extension of the *other* kind — used to identify a stale sibling to remove
    /// after a format flip (Markdown → remove stale `.json`; Messages → remove stale `.md`).
    ///
    /// Kept adjacent to `extension()` so the two stay in sync when a new kind is added.
    pub(crate) fn stale_extension(self) -> &'static str {
        match self {
            OutputKind::Markdown => "json", // we just wrote .md → stale is .json
            OutputKind::Messages => "md",   // we just wrote .json → stale is .md
        }
    }
}

/// Human-readable label for an [`OutputKind`], for the extension-mismatch warning.
///
/// Returns one of two `&'static str` literals and carries no runtime data — which is
/// why `kind_label(kind)` is allowlisted in `tests/print_discipline.rs` instead of
/// being wrapped in a sanitizer.
fn kind_label(kind: OutputKind) -> &'static str {
    match kind {
        OutputKind::Markdown => "markdown (.md)",
        OutputKind::Messages => "messages JSON (.json)",
    }
}

impl From<&CompiledOutput> for OutputKind {
    fn from(output: &CompiledOutput) -> Self {
        match output {
            CompiledOutput::Markdown(_) => OutputKind::Markdown,
            CompiledOutput::Messages(_) => OutputKind::Messages,
            // `CompiledOutput` is `#[non_exhaustive]`; update this match when new variants land.
            _ => unreachable!("unknown CompiledOutput variant"),
        }
    }
}

/// Derive the output filename by replacing the extension with the kind-appropriate extension.
///
/// - Markdown: `foo.mds` → `foo.md`
/// - Messages: `foo.mds` → `foo.json`
pub(crate) fn derive_output_filename_for_kind(input: &Path, kind: OutputKind) -> OsString {
    let stem = input.file_stem().unwrap_or(input.as_os_str());
    let mut name = OsString::from(stem);
    name.push(".");
    name.push(kind.extension());
    name
}

/// Compute `dir/<derived-name>.<ext>` WITHOUT creating the directory.
///
/// `input_path` drives the filename: if `Some`, the stem is reused (e.g. `foo.mds` → `foo.md`);
/// if `None` (stdin), the fallback name is `output.md` for markdown, `output.json` for messages.
///
/// [`write_output`] creates the directory, just before the write.
pub(crate) fn compute_output_dir_path_for_kind(
    dir: &Path,
    input_path: Option<&Path>,
    kind: OutputKind,
) -> PathBuf {
    let filename = input_path
        .map(|p| derive_output_filename_for_kind(p, kind))
        .unwrap_or_else(|| {
            OsString::from(match kind {
                OutputKind::Markdown => "output.md",
                OutputKind::Messages => "output.json",
            })
        });
    dir.join(filename)
}

/// Resolve the output path according to the precedence chain (kind-aware variant).
///
/// Nothing is created: [`write_output`] creates the output's directory just before the
/// write, so an output [`admit_output`] refuses as the entry file itself (#425) leaves
/// no directory behind — `--out-dir newdir/..` included.
///
/// Precedence:
/// 1. `-o -`                         → stdout (returns `None`)
/// 2. `-o <path>`                    → that exact path (verbatim; warn on ext mismatch)
/// 3. Stdin with no -o / --out-dir   → stdout (returns `None`)
/// 4. `--out-dir <dir>`              → `<dir>/<name>.<ext>` (ext from kind)
/// 5. `mds.json`                     → `<config_dir>/<output_dir>/<name>.<ext>` (ext from kind)
/// 6. Default                        → source dir + `<name>.<ext>` (ext from kind)
///
/// For rules 4–6 the extension is derived from `kind` (markdown → `.md`, messages → `.json`).
/// For rule 2 (`-o <path>`), the path is used verbatim, whatever its extension; once the
/// write is certain, [`warn_output_extension_mismatch`] warns when it conflicts with `kind`.
///
/// The output's shown form is fixed here too (#390). The file name comes from the input's
/// `canonical` form in every rule that derives one. Rules 2 and 4 build the path from
/// what was typed and show it as built; rule 5 writes below the canonical config
/// directory and is shown below the directory `mds.json` was reached by (`./dist/x.md`,
/// `sub/../dist/x.md`), as a config error names `mds.json` itself; rule 6 writes beside
/// the input's `canonical` form and is shown beside its `typed` one (`./x.md` for a bare
/// `x.mds`). `mds build` passes its input as typed in both forms ([`EntryPaths`]).
///
/// The write's anchor is resolved by path when it is written (#160): in rules 2, 4 and 6
/// it is the directory the output goes in — `-o`'s typed parent, `--out-dir` as typed, the
/// input's directory; in rule 5 it is the config directory, with `build.output_dir`'s own
/// directories below it, since the repository names them and the user does not. An
/// absolute `build.output_dir` is refused.
pub(crate) fn resolve_output_path_for_kind(
    input: Option<EntryPaths<'_>>,
    output: &Option<String>,
    out_dir: &Option<PathBuf>,
    config: &Option<ProjectConfig>,
    kind: OutputKind,
) -> Result<Option<WriteTarget>> {
    // 1 & 2. Explicit `-o` flag: `-` means stdout, anything else is a literal path.
    match output.as_deref() {
        Some("-") => return Ok(None),
        Some(o) => return Ok(Some(WriteTarget::as_typed(PathBuf::from(o)))),
        None => {}
    }

    // Derive the output filename from the input path (needed for steps 3-6).
    // Treat stdin ("-") as None so we fall back to "output.md/json" instead of "-.md".
    let input = input.filter(|entry| entry.typed != Path::new("-"));
    let input_path = input.map(|entry| entry.canonical);

    // 3. Stdin input with no explicit output destination → stdout.
    //    But if --out-dir is set, fall through so the user's explicit CLI flag
    //    is honored (using "output.md/json" as the derived filename).
    if input_path.is_none() && out_dir.is_none() {
        return Ok(None);
    }

    // 4. `--out-dir <dir>`
    if let Some(dir) = out_dir {
        return Ok(Some(WriteTarget::as_typed(
            compute_output_dir_path_for_kind(dir, input_path, kind),
        )));
    }

    // 5. `mds.json` output_dir
    if let Some(ProjectConfig {
        config,
        dir,
        shown_dir,
    }) = config
    {
        if let Some(ref output_dir) = config.build.output_dir {
            // Refuse an absolute `output_dir` and one with a `..` component (exit 2). A
            // forbidden character was already refused by `load_config`.
            crate::output::reject_output_dir_escape(output_dir)?;
            // The repository names `output_dir`, the user does not: its directories lie
            // below the config directory, the anchor, with the file (#160).
            return Ok(Some(WriteTarget::below(
                dir,
                shown_dir,
                &compute_output_dir_path_for_kind(Path::new(output_dir), input_path, kind),
            )));
        }
    }

    // 6. Default: file next to source, with kind-derived extension — written beside the
    //    input's canonical form, named beside it as typed (#390).
    match input {
        Some(entry) => {
            let filename = derive_output_filename_for_kind(entry.canonical, kind);
            // effective_parent maps "" (bare filename) to "." — avoids PF-006.
            Ok(Some(WriteTarget::new(
                effective_parent(entry.canonical).join(&filename),
                effective_parent(entry.typed).join(filename),
            )))
        }
        // Should not reach here (auto-detect always sets Some), but stdout as safe fallback.
        None => Ok(None),
    }
}

/// Warn when an explicit `-o <path>`'s extension contradicts the compiled `kind`
/// (AC-FUNC-11): the output is still written to the path as given. The warning
/// announces that write, so it is emitted only once the write is certain — by
/// [`admit_output`], after the #425 refusal has passed, never for an output that is
/// refused; `mds build -` (stdin), which has no entry file to refuse, emits it directly.
fn warn_output_extension_mismatch(output: &Option<String>, kind: OutputKind, quiet: bool) {
    let Some(o) = output.as_deref().filter(|o| *o != "-") else {
        return;
    };
    if quiet {
        return;
    }
    if let Some(ext) = Path::new(o).extension().and_then(|e| e.to_str()) {
        if ext != kind.extension() {
            // The `-o` value and the extension derived from it occupy a diagnostic
            // `file` field on a status line: WIRE (spec §7.5 per-field rule).
            // The escape call is repeated rather than bound to a local so it is visible
            // at each interpolation — the print-discipline guard reads call sites, not
            // bindings.
            crate::output::ewriteln!(
                "warning: output path '{}' has extension '.{}' but compiled \
                 output is {}; writing to '{}' anyway",
                crate::output::safe_inline(o),
                crate::output::safe_inline(ext),
                kind_label(kind),
                crate::output::safe_inline(o)
            );
        }
    }
}

// ── Key-value parsing ─────────────────────────────────────────────────────────

pub(crate) fn parse_key_value(s: &str) -> std::result::Result<(String, String), String> {
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid KEY=VALUE: no '=' found in '{s}'"))?;
    Ok((s[..pos].to_string(), s[pos + 1..].to_string()))
}

/// Coerce a CLI `--set KEY=VALUE` string to the most specific typed Value.
///
/// Matches the ergonomics of YAML frontmatter parsing: `true`/`false` become
/// booleans, integer and float literals become numbers, `null` becomes Null,
/// and bracket-delimited lists become arrays.  Everything else stays a string.
pub(crate) fn parse_cli_value(val: String) -> mds::Value {
    // Keywords first.
    match val.as_str() {
        "true" => return mds::Value::Boolean(true),
        "false" => return mds::Value::Boolean(false),
        "null" => return mds::Value::Null,
        _ => {}
    }

    // Integer — parse as i64 so we don't accept "1e3" (scientific notation) here;
    // then widen to f64 for storage.
    if let Ok(n) = val.parse::<i64>() {
        return mds::Value::Number(n as f64);
    }

    // Float — accept decimal fractions like "3.14".
    // Reject non-finite values (NaN, Infinity, -Infinity) — fall through to string.
    if let Ok(f) = val.parse::<f64>() {
        if f.is_finite() {
            return mds::Value::Number(f);
        }
    }

    // Simple bracket-list: "[a, b, c]" → Array of strings.
    // Only handles flat lists of unquoted tokens; does not recurse.
    if val.starts_with('[') && val.ends_with(']') {
        let inner = &val[1..val.len() - 1];
        if inner.trim().is_empty() {
            return mds::Value::Array(vec![]);
        }
        let items: Vec<mds::Value> = inner
            .split(',')
            .map(|s| mds::Value::String(s.trim().to_string()))
            .collect();
        return mds::Value::Array(items);
    }

    mds::Value::String(val)
}

/// Map an error to a categorized exit code.
///
/// Exit codes:
/// - 0: success (never returned here — handled by happy path)
/// - 1: logical/syntax error (undefined variable, arity mismatch, recursion, etc.)
/// - 2: I/O or file-system error (file not found, not an MDS file, I/O failure)
/// - 3: resource limit exceeded (output too large, too many iterations)
///
/// Errors created via `miette::miette!()` do NOT downcast to `MdsError`
/// and correctly fall through to exit code 1. Only `MdsError` values converted via
/// `.map_err(miette::Error::from)` are categorized.
pub(crate) fn exit_code(err: &miette::Error) -> i32 {
    // AD-211-5: `StdinRelabeledError` is a render-only wrapper around an `MdsError`.
    // It must be unwrapped here or wrapping an error to fix its DISPLAY label would
    // silently change its EXIT CODE (a wrapped FileNotFound would fall through to 1
    // instead of 2). The label swap is not allowed to have behavioural side effects.
    let mds_err = err.downcast_ref::<MdsError>().or_else(|| {
        err.downcast_ref::<crate::output::StdinRelabeledError>()
            .map(crate::output::StdinRelabeledError::inner)
    });
    if let Some(mds_err) = mds_err {
        match mds_err {
            MdsError::Io { .. } | MdsError::FileNotFound { .. } | MdsError::NotMdsFile { .. } => 2,
            MdsError::ResourceLimit { .. } => 3,
            // R4: --set/--set-string collision is a usage error over runtime
            // VARIABLES — the logical/content error class (exit 1).  Explicit
            // arm so the classification is a decision, not an accident of the
            // fall-through below.
            MdsError::VarConflict { .. } => 1,
            _ => 1,
        }
    } else {
        1
    }
}

// ── Input-validation helpers ──────────────────────────────────────────────────

/// Validate `path` for single-file fmt/lint: a forbidden path character is
/// refused first (→ `mds::io`, exit 2, #265), then existence is checked (→
/// `mds::file_not_found`, exit 2) and then the `.mds` extension (→
/// `mds::not_mds`, exit 2).
///
/// Every error names the path as the user typed it, escaped with
/// [`mds::escape_path_for_message`] (#417). The refusal comes first and is worded
/// like `NativeFs::check_symlink`'s, so a hostile file argument reports the same
/// error whether or not the file exists.
///
/// Existence-before-extension ordering is required so that a user pointing at a
/// non-existent path without `.mds` receives a "file not found" error rather than
/// the confusing "not an MDS file" error (C4/F6).
pub(crate) fn ensure_existing_mds_file(path: &Path) -> Result<(), MdsError> {
    crate::output::reject_forbidden_output_path("path", path.as_os_str())?;
    let lossy = path.to_string_lossy();
    let shown = mds::escape_path_for_message(&lossy);
    let exists = path.try_exists().map_err(|e| MdsError::Io {
        message: format!(
            "cannot check {shown}: {}",
            crate::output::safe_inline(crate::output::io_cause(&e))
        ),
    })?;
    if !exists {
        return Err(MdsError::FileNotFound {
            path: shown.into_owned(),
            span: None,
            src: None,
        });
    }
    if path.extension().and_then(|e| e.to_str()) != Some("mds") {
        return Err(MdsError::NotMdsFile {
            path: shown.into_owned(),
        });
    }
    Ok(())
}

// ── Runtime vars helpers ──────────────────────────────────────────────────────

/// Bundled runtime-variable arguments from the CLI.
///
/// Groups `--vars`, `--set`, and `--set-string` together so they can be passed
/// as a single unit through CLI dispatch functions.
pub(crate) struct RuntimeVarArgs {
    /// Optional JSON vars file (`--vars`).
    pub(crate) vars: Option<PathBuf>,
    /// Auto-coerced `--set KEY=VALUE` overrides (bool/number/null/array/string).
    pub(crate) set_vars: Vec<(String, String)>,
    /// String-forced `--set-string KEY=VALUE` overrides (always string, no coercion).
    pub(crate) set_string_vars: Vec<(String, String)>,
}

/// Load vars from an optional file path, returning None if no file was given.
pub(crate) fn load_optional_vars_file(path: Option<PathBuf>) -> Result<Option<mds::VarsLoad>> {
    path.map(|p| mds::load_vars_file_reporting_duplicates(&p).map_err(miette::Error::from))
        .transpose()
}

/// Result of [`build_runtime_vars`]: the resolved map plus intra-flag duplicate-key
/// lists for warning emission via [`emit_duplicate_var_warnings`].
#[derive(Debug)]
pub(crate) struct RuntimeVars {
    /// The merged variable map (`None` when no vars were given at all).
    pub(crate) vars: Option<HashMap<String, mds::Value>>,
    /// Keys that appeared more than once inside `--set` (first-occurrence order,
    /// one entry per key regardless of how many times it repeated).
    pub(crate) duplicate_set_keys: Vec<String>,
    /// Keys that appeared more than once inside `--set-string` (same contract).
    pub(crate) duplicate_set_string_keys: Vec<String>,
    /// Key paths (dotted/bracketed, e.g. `x.a`, `x[2].a`) that appeared more than
    /// once in the `--vars` JSON file, at any depth (#326). Empty when no `--vars`
    /// file was given, or when the file had no duplicates.
    pub(crate) duplicate_vars_file_keys: Vec<String>,
    /// Count of distinct duplicate key paths beyond `mds::VarsLoad`'s cap that were
    /// not individually recorded in `duplicate_vars_file_keys` (#326).
    pub(crate) duplicate_vars_file_keys_omitted: usize,
    /// The `--vars` file path as passed on the command line, for warning messages
    /// (#326). `None` when no `--vars` file was given.
    pub(crate) vars_file: Option<PathBuf>,
}

/// Collect keys that appear more than once in `pairs`, in first-occurrence order,
/// one entry per key regardless of repetition count.
fn duplicate_keys(pairs: &[(String, String)]) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut dupes: Vec<String> = Vec::new();
    for (k, _) in pairs {
        if !seen.insert(k.as_str()) && !dupes.iter().any(|d: &String| d == k) {
            dupes.push(k.clone());
        }
    }
    dupes
}

/// Merge a `--vars` file with `--set` and `--set-string` overrides into a single map.
///
/// Processing order: file vars < `--set`/`--set-string` overrides.
/// Within each flag group, later flags win (last-wins). Cross-flag duplicate keys
/// (a key present in both `--set` and `--set-string`) are rejected with an error.
///
/// Intra-flag duplicates (a key repeated within `--set` or within `--set-string`)
/// are allowed; the last value wins, and the caller is responsible for warning via
/// [`emit_duplicate_var_warnings`].
pub(crate) fn build_runtime_vars(args: RuntimeVarArgs) -> Result<RuntimeVars> {
    let RuntimeVarArgs {
        vars,
        set_vars,
        set_string_vars,
    } = args;

    // Cross-flag hard error must come BEFORE duplicate detection (test U5).
    // Collect unique keys used by --set for cross-flag duplicate detection.
    let set_keys: HashSet<&str> = set_vars.iter().map(|(k, _)| k.as_str()).collect();

    // Reject any key that appears in both --set and --set-string.
    // R4: typed as MdsError::VarConflict (code mds::var_conflict, exit 1 on every
    // subcommand — see exit_code() and the lint carve-out in lint.rs::do_lint).
    // Struct-literal construction; the key is sanitized at serialize/render time,
    // not here (ADR-008 — one-place sanitization).
    for (key, _) in &set_string_vars {
        if set_keys.contains(key.as_str()) {
            return Err(miette::Error::from(MdsError::VarConflict {
                key: key.clone(),
            }));
        }
    }

    // Detect intra-flag duplicates AFTER the cross-flag check.
    let duplicate_set_keys = duplicate_keys(&set_vars);
    let duplicate_set_string_keys = duplicate_keys(&set_string_vars);

    // Clone the path BEFORE load_optional_vars_file(vars) moves it (#326).
    let vars_file = vars.clone();
    let loaded = load_optional_vars_file(vars)?;
    let (mut runtime_vars, duplicate_vars_file_keys, duplicate_vars_file_keys_omitted) =
        match loaded {
            Some(mds::VarsLoad {
                vars,
                duplicate_keys,
                duplicate_keys_omitted,
                ..
            }) => (Some(vars), duplicate_keys, duplicate_keys_omitted),
            None => (None, Vec::new(), 0),
        };
    for (key, val) in set_vars {
        runtime_vars
            .get_or_insert_with(HashMap::new)
            .insert(key, parse_cli_value(val));
    }
    for (key, val) in set_string_vars {
        runtime_vars
            .get_or_insert_with(HashMap::new)
            .insert(key, mds::Value::String(val));
    }
    Ok(RuntimeVars {
        vars: runtime_vars,
        duplicate_set_keys,
        duplicate_set_string_keys,
        duplicate_vars_file_keys,
        duplicate_vars_file_keys_omitted,
        vars_file,
    })
}

/// Emit `warning: key '…' is set more than once in vars file …; the last value wins`
/// lines for every duplicate key path found in the `--vars` JSON file (#326), at
/// every depth, plus one tail line when the duplicate count exceeds
/// [`mds::VarsLoad`]'s cap.
///
/// AD-224-3: every untrusted value interpolated into `eprint_warning` must be wrapped
/// in `safe_inline(…)` / `safe_path(…)` **at the interpolation site** — not hoisted
/// into a `let` binding first. `key` is raw, untrusted text straight from the JSON
/// (D4); `Path` is not `Display`, so it goes through `safe_path`, not `safe_inline`.
///
/// AD-224-5: no-op when `quiet` is true.
pub(crate) fn emit_duplicate_vars_file_warnings(resolved: &RuntimeVars, quiet: bool) {
    if quiet {
        return;
    }
    let Some(path) = resolved.vars_file.as_deref() else {
        return;
    };
    for key in &resolved.duplicate_vars_file_keys {
        crate::output::eprint_warning(&format!(
            "warning: key '{}' is set more than once in vars file {}; the last value wins",
            crate::output::safe_inline(key),
            crate::output::safe_path(path)
        ));
    }
    if resolved.duplicate_vars_file_keys_omitted > 0 {
        crate::output::eprint_warning(&format!(
            "warning: {} more duplicate keys in vars file {} are not listed",
            crate::output::safe_inline(resolved.duplicate_vars_file_keys_omitted),
            crate::output::safe_path(path)
        ));
    }
}

/// Emit `warning: variable '…' is set more than once by --set/--set-string` lines
/// for any duplicate keys found by [`build_runtime_vars`], plus (D8 order: file →
/// `--set` → `--set-string`) the `--vars` file duplicate-key warnings (#326).
///
/// AD-224-3: every untrusted value interpolated into `eprint_warning` must be wrapped
/// in `safe_inline(…)` **at the interpolation site** — not hoisted into a `let` binding
/// first.  The call is repeated rather than bound so the print-discipline guard can see
/// it.  See `build.rs:318-320` for the same pattern.
///
/// AD-224-5: no-op when `quiet` is true.
pub(crate) fn emit_duplicate_var_warnings(resolved: &RuntimeVars, quiet: bool) {
    if quiet {
        return;
    }
    emit_duplicate_vars_file_warnings(resolved, quiet);
    for key in &resolved.duplicate_set_keys {
        crate::output::eprint_warning(&format!(
            "warning: variable '{}' is set more than once by --set; the last value wins",
            crate::output::safe_inline(key)
        ));
    }
    for key in &resolved.duplicate_set_string_keys {
        crate::output::eprint_warning(&format!(
            "warning: variable '{}' is set more than once by --set-string; the last value wins",
            crate::output::safe_inline(key)
        ));
    }
}

/// Read the source from stdin (see [`read_stdin_from`]).
///
/// A stdin source resolves its imports against the working directory. Callers pass
/// `None` as the base directory for that, never the absolute `current_dir()`: core
/// anchors `None` at the working directory itself, and a refusal of it (a forbidden
/// path character, #265) then names it `"."` — the caller typed no path, so no
/// message shows the absolute one.
pub(crate) fn read_stdin() -> Result<String, MdsError> {
    read_stdin_from(&mut std::io::stdin().lock())
}

/// Read a stdin source from `reader`, holding no more than one byte over
/// `MAX_FILE_SIZE` of it and reading no further (#428): [`mds::read_at_most`], as
/// mds-core reads a module file. More than the cap is refused before the bytes are
/// checked as UTF-8; bytes that are not UTF-8 keep the message `read_to_string` gave
/// them.
///
/// # Errors
///
/// More than the cap is `mds::resource_limit` (exit 3), as a file over the same cap is;
/// a read that fails and bytes that are not UTF-8 are `mds::io` (exit 2) (#157).
fn read_stdin_from(reader: &mut impl Read) -> Result<String, MdsError> {
    let bytes = mds::read_at_most(reader, MAX_FILE_SIZE + 1, 0).map_err(|e| MdsError::Io {
        message: format!(
            "cannot read stdin: {}",
            crate::output::safe_inline(crate::output::io_cause(&e))
        ),
    })?;
    if bytes.len() as u64 > MAX_FILE_SIZE {
        return Err(MdsError::ResourceLimit {
            message: "stdin input exceeds maximum size of 10 MiB".to_owned(),
        });
    }
    String::from_utf8(bytes).map_err(|_| MdsError::Io {
        message: "cannot read stdin: stream did not contain valid UTF-8".to_owned(),
    })
}

/// Write compiled output to a file or stdout.
///
/// When `target` is `Some`, writes the compiled string to `target.path`, creating the
/// directories it goes in, and prints `"Compiled to {target.shown}"` to stderr unless
/// `quiet` or `announce` is false (#390).  When `target` is `None`, writes the compiled
/// string to stdout with no trailing newline: a closed stdout is not an error (the reader
/// is gone, so nothing more is written and the run keeps its verdict), any other stdout
/// failure is `mds::io` (#157). This is where a single-file output's directory is
/// created — [`resolve_output_path_for_kind`] creates nothing — so an output refused
/// before the write leaves no directory behind (#425).
///
/// Set `announce = false` in watch-loop rebuilds so only the `"Recompiled …"`
/// summary line is emitted (not a redundant `"Compiled to …"` line).
/// Set `announce = true` for the initial/startup compile and for `mds build`.
///
/// The file write goes through [`atomic_write_file`] (#227, #160): a crash or write error
/// mid-way never leaves a truncated artifact — the previous output, if any, survives until
/// the rename — the anchor and the directories below it are created as needed, and a
/// symlink below the anchor or at the output path is refused. The stdout arm writes and
/// flushes the whole output at once ([`crate::output::write_stdout`]).
///
/// Compiled artifacts are written with [`Durability::RenameOnly`]: they are derived files
/// a rebuild reproduces, and `F_FULLFSYNC` per artifact tripled a 500-template watch
/// startup. Source rewrites (`fmt`, `lint --fix`) keep the fsync.
pub(crate) fn write_output(
    target: Option<&WriteTarget>,
    compiled: &str,
    quiet: bool,
    announce: bool,
) -> Result<()> {
    match target {
        Some(target) => {
            atomic_write_file(target, compiled, Durability::RenameOnly, Parents::Create)?;
            if !quiet && announce {
                crate::output::ewriteln!("Compiled to {}", crate::output::safe_path(&target.shown));
            }
        }
        None => crate::output::write_stdout(compiled.as_bytes()).into_batch_result()?,
    }
    Ok(())
}

/// Scan the working directory for `.mds` files.
///
/// Returns `Ok(name)` if exactly one `.mds` file is found — its bare file name, the
/// path relative to the working directory, so the run names it exactly as the same
/// command given that name would (`Building x.mds`), never by an absolute path the
/// user did not type (#390). An `Err` describes why auto-detection failed: zero files,
/// multiple files, or a working directory that cannot be listed — `mds::io` (exit 2),
/// naming it `.` (#390).
pub(crate) fn auto_detect_mds_file(subcommand: &str) -> Result<PathBuf> {
    let entries: Vec<PathBuf> = std::fs::read_dir(".")
        .map_err(|e| MdsError::Io {
            message: format!(
                "cannot read directory .: {}",
                crate::output::safe_inline(crate::output::io_cause(&e))
            ),
        })?
        .filter_map(|res| {
            let name = PathBuf::from(res.ok()?.file_name());
            (name.is_file() && name.extension().and_then(|e| e.to_str()) == Some("mds"))
                .then_some(name)
        })
        .collect();

    match entries.as_slice() {
        [] => Err(miette::miette!(
            "no .mds files found in current directory\n  \
             hint: run 'mds init' to create a starter template"
        )),
        [single] => Ok(single.clone()),
        several => Err(several_mds_files(subcommand, several)),
    }
}

/// The error [`auto_detect_mds_file`] gives for more than one `.mds` file in the working
/// directory: their file names, each escaped (#390), sorted. The user never typed them,
/// and a name may hold a newline.
fn several_mds_files(subcommand: &str, entries: &[PathBuf]) -> miette::Report {
    let mut names: Vec<String> = entries
        .iter()
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
        .map(crate::output::safe_file_display)
        .collect();
    names.sort();
    miette::miette!(
        "multiple .mds files found: {}\n  \
         hint: specify which file, e.g. 'mds {subcommand} {}'",
        names.join(", "),
        names.first().map(|s| s.as_str()).unwrap_or("<file>.mds"),
    )
}

// ── Shared compile-and-write helper (used by build and watch) ─────────────────

/// The compiled output content and its transitive dependency list.
///
/// Returned by [`compile_to_content`] so the watch loop can compare content before
/// deciding whether to write (content-based dedup — see watch.rs).
pub(crate) struct CompileOutput {
    /// The compiled string ready to write (markdown or pretty JSON depending on kind).
    pub(crate) content: String,
    /// The output kind (derived intrinsically from the compiled output).
    pub(crate) kind: OutputKind,
    /// Transitive dependency paths (empty when no `@import`s), as the compiler reports
    /// them (`CompileResult.dependencies`). `mds watch` alone reads them, and keys them by
    /// path itself (#390, #409).
    pub(crate) dependencies: Vec<String>,
    /// Source map if `opts.source_map` was `true` and the compilation produced one.
    pub(crate) source_map: Option<mds::SourceMap>,
}

/// Serialize `CompiledOutput` to the CLI wire format.
///
/// - Markdown: the rendered string as-is (moved, no copy).
/// - Messages: pretty-printed JSON array of `{role,content}` with a trailing newline (AC-FUNC-09).
///
/// Takes ownership so the markdown arm avoids an up-to-10 MiB clone (issue 2).
fn serialize_output(output: CompiledOutput) -> Result<String> {
    match output {
        CompiledOutput::Markdown(s) => Ok(s),
        CompiledOutput::Messages(msgs) => messages_json(&msgs).map_err(|e| {
            miette::miette!(
                "failed to serialize messages to JSON: {}",
                crate::output::safe_inline(&e)
            )
        }),
        // `CompiledOutput` is `#[non_exhaustive]`; update this match when new variants land.
        _ => unreachable!("unknown CompiledOutput variant"),
    }
}

/// `messages` as a messages output is written: `serde_json`'s pretty form and a newline.
/// A directory build writes a stale `.json` back through it to show that mds wrote the
/// file before it removes it (#160).
pub(crate) fn messages_json(messages: &[impl serde::Serialize]) -> serde_json::Result<String> {
    let mut json = serde_json::to_string_pretty(messages)?;
    json.push('\n');
    Ok(json)
}

/// Compile `input` and return the content + kind + deps WITHOUT writing any output.
///
/// The output kind (Markdown vs Messages) is determined intrinsically from the compiled
/// result — the caller does not specify it. This is the pure "compile" step used by the
/// watch loop for content-based dedup.
///
/// `build` calls this directly; `mds watch` calls it for its entry and every source
/// once it has checked that the path it compiles still leads to the file it watches.
///
/// Pass `opts = mds::CompileOptions::default()` from watch callers that do not want
/// source maps; the watch paths never emit maps so they always use the default.
///
/// # PF-004 compliance
/// All file reads go through `mds::compile_with_deps_opts` or
/// `mds::compile_str_with_deps_opts` (which use the resolver that enforces
/// MAX_FILE_SIZE). Stdin input is read through `read_stdin` which enforces the same cap.
/// There is no bare `std::fs::read_to_string`.
pub(crate) fn compile_to_content(
    input: &Path,
    runtime_vars: Option<HashMap<String, mds::Value>>,
    quiet: bool,
    opts: mds::CompileOptions,
) -> Result<CompileOutput> {
    let result = if input == Path::new("-") {
        // Stdin: compile from source string with the working directory as base_dir
        // (`None` — see `read_stdin`). read_stdin enforces MAX_FILE_SIZE (PF-004).
        let source = read_stdin()?;
        // AD-211-1 / AD-211-5: a string-source compile labels its errors `<source>`
        // (resolver's SOURCE_LABEL). Relabel to the uniform CLI sentinel here, at the
        // boundary that knows the input was stdin.
        mds::compile_str_with_deps_opts(&source, None, runtime_vars, opts)
            .map_err(|e| crate::output::relabel_stdin_error(&e, &source))?
    } else {
        // File path: compile_with_deps_opts routes through the resolver which enforces
        // MAX_FILE_SIZE and check_symlink (PF-004 compliance).
        mds::compile_with_deps_opts(input, runtime_vars, opts).map_err(miette::Error::from)?
    };

    if !quiet {
        for w in &result.warnings {
            crate::output::eprint_warning(w);
        }
    }

    let kind = OutputKind::from(&result.output);
    let source_map = result.source_map;
    // Move result.output into serialize_output so the Markdown arm avoids a clone
    // (the kind was already derived from the borrow above — issue 2).
    let content = serialize_output(result.output)?;
    Ok(CompileOutput {
        content,
        kind,
        dependencies: result.dependencies,
        source_map,
    })
}

/// A single-file entry in the two forms [`admit_output`] takes, always in
/// `(typed, canonical)` order, so the two cannot be passed swapped.
///
/// `typed` is the path as the user typed it: a refusal names the entry that way, never
/// by its canonical absolute path. `canonical` is the entry's identity, which
/// [`admit_output`] compares with the output's through [`file_identity`], canonical with
/// canonical — `typed` is never compared with it as text (#408). `mds watch` lends the
/// entry it watches in its two forms. `mds build` compiles its entry once, by the path as
/// typed, and never holds a canonical form, so it passes the typed path as both:
/// [`file_identity`] canonicalizes it.
#[derive(Clone, Copy)]
pub(crate) struct EntryPaths<'a> {
    pub(crate) typed: &'a Path,
    pub(crate) canonical: &'a Path,
}

impl<'a> EntryPaths<'a> {
    /// An entry held only as typed, as `mds build` holds it: the typed path as both.
    pub(crate) fn as_typed(path: &'a Path) -> Self {
        Self {
            typed: path,
            canonical: path,
        }
    }
}

/// The canonical path of the file `path` names, for comparing two paths as files, or
/// `None` when not even its directory resolves.
///
/// Its directory is resolved as [`write_output`] will leave it, created if it does not
/// exist yet ([`resolve_dir_as_created`]), and its name is looked up there. An existing
/// file is canonicalized, which respells its name the way the volume stores it — so
/// `PAGE.md` and `page.md` are one file on a case-insensitive volume (#408), in a
/// directory reached back out of one the write creates (`newdir/../PAGE.md`) as much as
/// in one that exists. A name that does not exist yet, or is a symlink, is kept as it
/// stands: the symlink itself is the directory entry a write would replace, not the
/// file it points to.
fn file_identity(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?;
    let file = resolve_dir_as_created(effective_parent(path))?.join(name);
    let is_link = std::fs::symlink_metadata(&file).is_ok_and(|m| m.file_type().is_symlink());
    if !is_link {
        if let Ok(canonical) = file.canonicalize() {
            return Some(canonical);
        }
    }
    Some(file)
}

/// The canonical path `dir` has once [`write_output`] has created it — the write creates
/// an output's directory, its anchor, with `create_dir_all` (#160) — found without
/// creating anything; `None` when not even the working directory
/// resolves, which a write of `dir` could not get past either.
///
/// An existing `dir` is canonicalized. Otherwise it is walked component by component, as
/// the system resolves it while creating it: an existing component is canonicalized —
/// a symlink is followed — and the first missing one is created as a plain directory,
/// so below it nothing exists yet: a name is appended as it stands, and a `..` leads back
/// to the parent, where the next name is looked up on disk again. So `newdir/..` is the
/// directory `newdir` would be created in, and `newdir/../lnk` follows `lnk`. The walk
/// visits each component of `dir` once, and a `..` never climbs above the root.
///
/// A relative `dir` is anchored at the working directory with [`std::path::absolute`].
/// On Windows that also collapses `..` lexically, which is how every Win32 file call
/// reads a path, so the walk and the write agree there too.
///
/// `mds watch` resolves its out-dir this way before each write, to tell the directory
/// the session started with from one the path now leads to (#160).
pub(crate) fn resolve_dir_as_created(dir: &Path) -> Option<PathBuf> {
    use std::path::Component;

    if let Ok(canonical) = dir.canonicalize() {
        return Some(canonical);
    }
    let absolute = std::path::absolute(dir).ok()?;
    let mut resolved = PathBuf::new();
    // How many trailing components of `resolved` the write creates.
    let mut created: usize = 0;
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => resolved.push(component),
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
                created = created.saturating_sub(1);
            }
            Component::Normal(name) => {
                let next = resolved.join(name);
                if created > 0 {
                    resolved = next;
                    created += 1;
                } else if let Ok(existing) = next.canonicalize() {
                    resolved = existing;
                } else {
                    resolved = next;
                    created = 1;
                }
            }
        }
    }
    Some(resolved)
}

/// Refuse to write the compiled entry over the entry file itself (#425): `mds::io`,
/// exit 2, naming the entry as `typed`, escaped.
///
/// A `.md` entry that declares `type: mds` compiles to Markdown, whose default output
/// name — the entry's stem plus `.md` — is the entry's own name; `--out-dir` or
/// `mds.json` `build.output_dir` naming the entry's directory, and `-o` naming the
/// entry itself (any extension), land on it too. Writing would replace the source with
/// its compiled form, which no longer declares `type: mds`, so the next build fails.
///
/// `output` and `entry.canonical` are compared as the files they name
/// ([`file_identity`]), canonical with canonical, never a path with a spelling of it
/// (#408) — an output whose directory does not exist yet as the write will create it, so
/// `newdir/../page.md` is `page.md`. `None` (stdout) is never the entry. It runs only
/// inside [`admit_output`], after the output path is resolved and before anything is
/// written.
fn refuse_output_over_entry(output: Option<&Path>, entry: EntryPaths<'_>) -> Result<(), MdsError> {
    let Some(output) = output else {
        return Ok(());
    };
    match (file_identity(output), file_identity(entry.canonical)) {
        (Some(output), Some(canonical)) if output == canonical => Err(MdsError::Io {
            message: format!(
                "output would overwrite the entry file: \"{}\"; \
                 write it elsewhere with -o <file> or --out-dir <dir>",
                mds::escape_path_for_message(&entry.typed.to_string_lossy())
            ),
        }),
        _ => Ok(()),
    }
}

/// Admit `output_path` as the destination of the compiled `entry`: refuse it when it is
/// the entry file itself ([`refuse_output_over_entry`], #425), and only then warn when
/// an explicit `-o` (`output_arg`) contradicts `kind` ([`warn_output_extension_mismatch`])
/// — the warning announces the write, so a refused output never gets one.
///
/// Every route to a compiled entry's output goes through it: `mds build` file mode,
/// `mds watch`'s startup compile, its startup fallback and every rebuild. A rebuild
/// passes no `output_arg`: the warning is a startup message, printed once for the path
/// every rebuild reuses.
pub(crate) fn admit_output(
    output_path: Option<&Path>,
    entry: EntryPaths<'_>,
    output_arg: &Option<String>,
    kind: OutputKind,
    quiet: bool,
) -> Result<(), MdsError> {
    refuse_output_over_entry(output_path, entry)?;
    warn_output_extension_mismatch(output_arg, kind, quiet);
    Ok(())
}

// ── Build args struct ─────────────────────────────────────────────────────────

pub(crate) struct BuildArgs {
    pub(crate) input: Option<PathBuf>,
    /// `-o/--output` as given; [`reject_forbidden_output_flags`] turns it into text.
    pub(crate) output: Option<OsString>,
    pub(crate) out_dir: Option<PathBuf>,
    pub(crate) vars: Option<PathBuf>,
    pub(crate) set_vars: Vec<(String, String)>,
    pub(crate) set_string_vars: Vec<(String, String)>,
    pub(crate) quiet: bool,
    /// `--source-map` CLI flag (requires `mds build`).
    pub(crate) source_map: bool,
    /// `--no-source-map` CLI flag (overrides `build.source_map` from mds.json).
    pub(crate) no_source_map: bool,
    /// `--inline` CLI flag: embed map as data URI comment in the output file.
    pub(crate) inline: bool,
    /// `--embed-sources` CLI flag: include source text in sourcesContent[].
    pub(crate) embed_sources: bool,
}

/// Resolve the input path: use the explicit value, or auto-detect from cwd.
///
/// `subcommand` is the CLI verb (e.g. `"build"`, `"lint"`, `"fmt"`, `"check"`, `"watch"`)
/// used in the auto-detect error hint so the user sees a correct example command.
///
/// Returns `(path, auto_detected)`.
pub(crate) fn resolve_input(input: Option<PathBuf>, subcommand: &str) -> Result<(PathBuf, bool)> {
    match input {
        Some(p) => Ok((p, false)),
        None => auto_detect_mds_file(subcommand).map(|p| (p, true)),
    }
}

// ── Source-map helpers ────────────────────────────────────────────────────────

/// Return the sidecar map path: append `.map` to the full output name.
///
/// `foo.md` → `foo.md.map`, `foo.json` → `foo.json.map`.
/// Never uses `with_extension` because that replaces the final component
/// (e.g. `foo.md.map` would become `foo.map`), not appends.
pub(crate) fn map_path_for(output_path: &Path) -> PathBuf {
    let mut s = output_path.as_os_str().to_owned();
    s.push(".map");
    PathBuf::from(s)
}

/// RFC-4648 standard base64 encode (no line wrapping).
///
/// Alphabet: `A-Za-z0-9+/` with `=` padding.  None of these characters are
/// `<`, `>`, or `-`, so the encoded payload cannot break out of an HTML comment
/// (AC-SEC-03).
pub(crate) fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Build the inline sourceMappingURL comment carrier for a source map JSON.
///
/// Format: `<!--# sourceMappingURL=data:application/json;base64,<B64> -->`
///
/// The `<!--# ... -->` syntax is the canonical format supported by browser
/// devtools and bundler tooling.  The base64 alphabet excludes `<`, `>`, and
/// `-` so the payload cannot break out of the HTML comment (AC-SEC-03).
pub(crate) fn carrier_line(map_json: &str) -> String {
    let b64 = base64_encode(map_json.as_bytes());
    format!("<!--# sourceMappingURL=data:application/json;base64,{b64} -->")
}

/// Strip a trailing inline sourceMappingURL carrier comment from `content`
/// (for idempotent re-embedding).
///
/// Looks for `<!--# sourceMappingURL=data:` as the last non-empty line.
fn strip_existing_carrier(content: &str) -> &str {
    // Scan from the end, skipping a final newline if present.
    let trimmed = content.trim_end_matches('\n');
    // Find the last newline boundary.
    if let Some(pos) = trimmed.rfind('\n') {
        let last_line = &trimmed[pos + 1..];
        if last_line.starts_with("<!--# sourceMappingURL=data:") {
            // Strip the last line plus its preceding newline.
            return &content[..pos + 1];
        }
    } else if trimmed.starts_with("<!--# sourceMappingURL=data:") {
        // The entire content is the carrier (single-line file).
        return "";
    }
    content
}

/// Embed an inline carrier comment at the end of `content`.
///
/// Strips any existing carrier first (idempotent), then appends a newline
/// separator (if needed) and the carrier line.
pub(crate) fn embed_carrier(content: String, map_json: &str) -> String {
    let base = strip_existing_carrier(&content);
    // Ensure there is exactly one newline before the carrier.
    let sep = if base.ends_with('\n') { "" } else { "\n" };
    format!("{base}{sep}{}\n", carrier_line(map_json))
}

/// Compute the directory to use as `source_map_base` in [`mds::CompileOptions`].
///
/// This is the map-file directory: the anchor against which core's
/// `relativize_source` relativizes every `sources[]` entry (ADR-005 / PF-004 —
/// single choke-point in core).  Must be called BEFORE constructing
/// `CompileOptions` so `source_map_base` can be set on the options struct.
///
/// The result is always absolutized against the working directory so that core's
/// root-containment check works correctly against the absolute project root. Only a
/// build that writes a source map needs it: core relativizes `sources[]` against it and
/// reads it for nothing else.
///
/// Mirrors the output-directory rules of [`resolve_output_path_for_kind`]:
/// - `-o -` or stdin-with-no-output → current working directory.
/// - `-o <file>` → directory of that file (absolutized if relative).
/// - `--out-dir <dir>` → that directory (absolutized if relative).
/// - mds.json `output_dir` → config-directory-relative.
/// - Default → beside the source file.
///
/// # Errors
///
/// A base that needs the working directory when it cannot be determined: `mds::io`, in
/// [`crate::output::current_dir`]'s words (#390). It is never anchored at `"."`
/// instead: core takes a relative base as an absolute one, so `sources[]` would be
/// computed against the wrong directory without a word.
fn compute_source_map_base(
    input: &Path,
    output: &Option<String>,
    out_dir: &Option<PathBuf>,
    config: &Option<ProjectConfig>,
) -> Result<PathBuf, MdsError> {
    use crate::output::current_dir;
    let abs = |p: PathBuf| -> Result<PathBuf, MdsError> {
        if p.is_absolute() {
            Ok(p)
        } else {
            Ok(current_dir()?.join(p))
        }
    };

    match output.as_deref() {
        Some("-") => {
            // -o - : stdout; relativize against CWD (PF-005: unconditional).
            current_dir()
        }
        Some(o) => {
            // -o <file>: map lives beside the output file.
            // effective_parent maps "" (bare filename) to "." — PF-006.
            abs(effective_parent(Path::new(o)).to_path_buf())
        }
        None => {
            if let Some(dir) = out_dir {
                // --out-dir <dir>: absolutize if relative.
                abs(dir.clone())
            } else if input == Path::new("-") {
                // Stdin with no -o or --out-dir → stdout → relativize against CWD.
                current_dir()
            } else if let Some(ProjectConfig { config, dir, .. }) = config {
                if let Some(ref output_dir) = config.build.output_dir {
                    // mds.json output_dir: config-directory-relative.  `dir`
                    // is canonical (load_config canonicalizes before walking up) so
                    // the join is already absolute in practice; `abs` makes the
                    // "result is always absolutized" contract above structural
                    // rather than incidental — a relative base would silently
                    // demote core's map-relative emission to root-relative.
                    abs(dir.join(output_dir))
                } else {
                    // Default: beside the source file.
                    abs(effective_parent(input).to_path_buf())
                }
            } else {
                // No config, no -o, no --out-dir: beside the source file.
                abs(effective_parent(input).to_path_buf())
            }
        }
    }
}

/// Set the SMv3 `file` field and relabel the stdin source in a [`mds::SourceMap`].
///
/// These are the CLI's two genuinely CLI-only post-processing jobs after core
/// has already applied `relativize_source` at both finalize sites (ADR-005 /
/// PF-004 — single choke-point in core):
///
/// 1. `sm.file = output_basename` — the SMv3 `file` field names the generated
///    artifact; core always emits `file: None` because it has no notion of the
///    output path.
/// 2. The `<stdin>` relabel: maps `STRING_SOURCE_MAP_LABEL` (`"input.mds"`) →
///    `"<stdin>"` for stdin builds.  Core canonicalizes the source entry at
///    `MapBuilder::new`/`source_index`, so the exact-string check here always
///    matches the canonicalized value.  This is a pure label swap — no path
///    logic.
pub(crate) fn apply_source_map_file_label(
    sm: &mut mds::SourceMap,
    output_path: Option<&Path>,
    stdin_label: bool,
) {
    // Job 1: set the `file` field for file output.
    if let Some(out) = output_path {
        sm.file = out.file_name().map(|n| n.to_string_lossy().into_owned());
    }
    // `sm.file` stays None for stdout output (no output filename to anchor).

    // Job 2: relabel the stdin source entry.
    if stdin_label {
        for src in &mut sm.sources {
            if src == STRING_SOURCE_MAP_LABEL {
                // AD-211-3: use the centralised sentinel from output.rs.
                *src = crate::output::STDIN_DISPLAY_LABEL.to_string();
            }
        }
    }
}

/// Delete the `.map` file at `map.path` when it is the sidecar an earlier
/// `--source-map` build wrote for the output named `expected_basename` (stale-map
/// reconciliation), and leave anything else in place, never clobbering a hand-authored
/// file that happens to share its name.
///
/// A missing file is skipped silently. Anything else that is not such a sidecar is left
/// in place with a warning (unless `quiet`): one that is not a regular file is never
/// opened — opening a FIFO with no writer blocks — and a regular file is recognised by
/// its first bytes alone ([`has_sidecar_head`]), so none is read whole (#428). The
/// removal goes through [`crate::write::remove_proven`] (#160): the map is looked at,
/// read and removed below its anchor, through no symlink there or at the map, and only
/// while it is the file read. Every message names the map by `map.shown` (#390).
///
/// # Errors
///
/// A map that cannot be read — so nothing is known of its content — and a sidecar that
/// is not removed — refused, or the removal failed — are `mds::io` (exit 2, #157).
pub(crate) fn verify_then_delete_map(
    map: &WriteTarget,
    expected_basename: &str,
    quiet: bool,
) -> Result<(), MdsError> {
    let proof = |file: &mut std::fs::File| has_sidecar_head(file, expected_basename);
    match crate::write::remove_proven(map, proof) {
        Ok(Removal::Removed) => {
            if !quiet {
                crate::output::ewriteln!(
                    "Removed stale map {}",
                    crate::output::safe_path(&map.shown)
                );
            }
            Ok(())
        }
        Ok(Removal::Missing) => Ok(()),
        Ok(Removal::Kept) | Err(NotRemoved::NotAFile) => {
            if !quiet {
                crate::output::ewriteln!(
                    "warning: leaving {} in place — not a tool-generated SMv3 map (version/file mismatch)",
                    crate::output::safe_path(&map.shown)
                );
            }
            Ok(())
        }
        Err(not_removed) => Err(crate::output::stale_removal_error(
            "stale map",
            &map.shown,
            &not_removed,
        )),
    }
}

/// Whether `reader` starts with the bytes every sidecar mds writes for the output named
/// `expected_basename` starts with: `{"version":3,"file":<the name as a JSON string>,`.
/// `SourceMap::to_json` writes `version`, `file` and then `sources` in that order, with
/// no whitespace, and a sidecar always carries the output's name as `file`, so a map it
/// wrote always starts so; a map formatted by hand does not, even with the same fields.
/// Only those bytes are read (#428); a read that fails is the error.
fn has_sidecar_head(reader: &mut impl Read, expected_basename: &str) -> std::io::Result<bool> {
    let Ok(name) = serde_json::to_string(expected_basename) else {
        return Ok(false);
    };
    let head = format!("{{\"version\":3,\"file\":{name},");
    let len = head.len() as u64;
    Ok(mds::read_at_most(reader, len, len)? == head.as_bytes())
}

/// Refuse `-o/--output` and `--out-dir` values that name no location mds can write
/// and show faithfully: `mds::io`, exit 2. Shared by `build` and `watch`, which both
/// call it before any other work, so a refused location is never created or written.
/// Returns the `-o` value as text, the form the rest of the run uses.
///
/// Each value is checked in the same order:
/// - A forbidden path character (#265), as typed.
/// - A value that is not valid UTF-8 (#390).
/// - A forbidden path character in the form the value resolves to (#265: a symlink into
///   a hostile-named directory) — for `-o`, unless it is `-`, stdout. A relative value
///   is resolved against the working directory, so one that cannot be determined
///   refuses it (#390): it names no directory, and is refused in
///   [`crate::output::current_dir`]'s words rather than resolved against `"."`.
pub(crate) fn reject_forbidden_output_flags(
    output: Option<&OsStr>,
    out_dir: Option<&Path>,
) -> Result<Option<String>> {
    use crate::output::{
        reject_forbidden_output_path, reject_forbidden_resolved_output_path,
        reject_non_utf8_output_path,
    };
    let output = match output {
        Some(o) => {
            reject_forbidden_output_path("-o/--output", o)?;
            let text = reject_non_utf8_output_path("-o/--output", o)?;
            // `-o -` is stdout, not a path.
            if text != "-" {
                reject_forbidden_resolved_output_path("-o/--output", Path::new(o), o)?;
            }
            Some(text.to_owned())
        }
        None => None,
    };
    if let Some(d) = out_dir {
        reject_forbidden_output_path("--out-dir", d.as_os_str())?;
        reject_non_utf8_output_path("--out-dir", d.as_os_str())?;
        reject_forbidden_resolved_output_path("--out-dir", d, d.as_os_str())?;
    }
    Ok(output)
}

pub(crate) fn run_build(args: BuildArgs) -> Result<()> {
    let BuildArgs {
        input,
        output,
        out_dir,
        vars,
        set_vars,
        set_string_vars,
        quiet,
        source_map: flag_source_map,
        no_source_map,
        inline,
        embed_sources: flag_embed_sources,
    } = args;
    // #265, #390: refuse a hostile output location before anything is read or compiled.
    let output = reject_forbidden_output_flags(output.as_deref(), out_dir.as_deref())?;
    let resolved = build_runtime_vars(RuntimeVarArgs {
        vars,
        set_vars,
        set_string_vars,
    })?;
    emit_duplicate_var_warnings(&resolved, quiet);
    let runtime_vars = resolved.vars;

    // Resolve the input: explicit path, or auto-detect from cwd.
    // When auto-detected, print a "Building {path}" banner so users know which file was selected.
    let (input, auto_detected) = resolve_input(input, "build")?;
    if auto_detected && !quiet {
        crate::output::ewriteln!("Building {}", crate::output::safe_path(&input));
    }

    // Directory mode: compile every non-partial .mds file in the tree.
    if input != Path::new("-") && input.is_dir() {
        // Reject -o/--output in directory mode: output goes to files, not a single destination.
        if output.is_some() {
            return Err(miette::miette!(
                "build directory mode does not support -o/--output; \
                 use --out-dir to specify an output directory"
            ));
        }
        // #413: the one directory-argument check every directory-mode subcommand makes
        // (a symlink, the filesystem root, a forbidden character — all `mds::io`).
        crate::input::resolve_directory_argument(&input).map_err(miette::Error::from)?;

        // Load project config to determine effective flags for directory mode.
        let dir_config = load_config(&input)?;
        let cfg_source_map = dir_config
            .as_ref()
            .map(|c| c.config.build.source_map)
            .unwrap_or(false);
        let cfg_embed_sources = dir_config
            .as_ref()
            .map(|c| c.config.build.embed_sources)
            .unwrap_or(false);
        let use_source_map = (flag_source_map || cfg_source_map) && !no_source_map;
        let use_embed_sources = flag_embed_sources || cfg_embed_sources;

        // C3/D6: embed_sources without source_map is a no-op (applies PF-004 — all paths).
        if use_embed_sources && !use_source_map && !quiet {
            crate::output::ewriteln!(
                "warning: embed_sources has no effect without source maps; \
                 set build.source_map=true or pass --source-map"
            );
        }

        return run_build_directory(
            &input,
            out_dir,
            runtime_vars,
            quiet,
            use_source_map,
            use_embed_sources,
            inline,
        );
    }

    // ── Stdin path ───────────────────────────────────────────────────────────────

    if input == Path::new("-") {
        // Stdin: no project config; effective flags from CLI only.
        let use_source_map = flag_source_map && !no_source_map;
        let use_embed_sources = flag_embed_sources;

        // C3/D6: embed_sources without source_map is a no-op; warn so users don't wonder
        // why their output contains no sourcesContent (applies PF-004 — both paths checked).
        if use_embed_sources && !use_source_map && !quiet {
            crate::output::ewriteln!(
                "warning: embed_sources has no effect without source maps; \
                 set build.source_map=true or pass --source-map"
            );
        }

        // AC-SEC-02: warn when shipping full source text in a distributable artifact.
        if use_embed_sources && inline && !quiet {
            crate::output::ewriteln!(
                "warning: --embed-sources with --inline ships full source text in the output \
                 (AC-SEC-02)"
            );
        }

        let source_map_base = use_source_map
            .then(|| compute_source_map_base(Path::new("-"), &output, &out_dir, &None))
            .transpose()?;
        let opts = mds::CompileOptions::default()
            .with_source_map(use_source_map)
            .with_include_sources_content(use_embed_sources)
            .with_source_map_base(source_map_base);

        let source = read_stdin()?;
        // AD-211-1 / AD-211-5: same stdin relabel as `compile_to_content`; `None` is the
        // working directory, shown as "." (see `read_stdin`).
        let result = mds::compile_str_with_deps_opts(&source, None, runtime_vars, opts)
            .map_err(|e| crate::output::relabel_stdin_error(&e, &source))?;
        if !quiet {
            for w in &result.warnings {
                crate::output::eprint_warning(w);
            }
        }

        let mut source_map = result.source_map;
        let kind = OutputKind::from(&result.output);
        let content = serialize_output(result.output)?;

        // Stdin: no project config; output path follows -o flag or defaults to stdout.
        // There is no entry file to refuse (#425), so the `-o` warning is printed now.
        let output_path = resolve_output_path_for_kind(
            Some(EntryPaths::as_typed(&input)),
            &output,
            &out_dir,
            &None,
            kind,
        )?;
        warn_output_extension_mismatch(&output, kind, quiet);

        if let Some(ref mut sm) = source_map {
            // Set `file` field and relabel source entry for stdin builds (AC-FUNC-12).
            apply_source_map_file_label(sm, output_path.as_ref().map(|t| t.path.as_path()), true);
        }

        if use_source_map {
            if inline {
                // Inline: embed carrier into the output content, then write.
                let map_json = source_map.as_ref().map(|sm| sm.to_json());
                let final_content = if let Some(ref json) = map_json {
                    embed_carrier(content, json)
                } else {
                    content
                };
                return write_output(output_path.as_ref(), &final_content, quiet, true);
            } else {
                // Sidecar: write output first (byte-identical to no-flag; ADR-002).
                match &output_path {
                    None => {
                        // Sidecar + stdout: three-case ladder (stdin has no config, so cases 1–2).
                        if source_map.is_none() {
                            // Case 1 — messages mode: compiler produced no map; write to stdout.
                            return write_output(None, &content, quiet, true);
                        }
                        // Case 2 — explicit --source-map flag: hard error.
                        return Err(miette::miette!(
                            "--source-map (sidecar) requires an output file; use -o <file> or \
                             --out-dir, --inline to embed the map for stdout, or --no-source-map \
                             to skip it"
                        ));
                    }
                    Some(out) => {
                        write_output(Some(out), &content, quiet, true)?;
                        if let Some(ref sm) = source_map {
                            let map = out.sibling(map_path_for);
                            // The sidecar's `file` / `sources` / `sourcesContent` are
                            // written VERBATIM, by decision — spec §7.5 "Carve-out:
                            // functional path references". They are resolved against the
                            // filesystem by devtools and bundlers, so a `\uXXXX`-escaped
                            // path would not exist. Consumers must treat them as
                            // untrusted; see the `mds::SourceMap` rustdoc. The status
                            // line below is a diagnostic surface and IS escaped.
                            let map_json = sm.to_json();
                            atomic_write_file(
                                &map,
                                &map_json,
                                Durability::RenameOnly,
                                Parents::Create,
                            )?;
                            if !quiet {
                                crate::output::ewriteln!(
                                    "Source map written to {}",
                                    crate::output::safe_path(&map.shown)
                                );
                            }
                        }
                        return Ok(());
                    }
                }
            }
        }

        return write_output(output_path.as_ref(), &content, quiet, true);
    }

    // ── File input path ──────────────────────────────────────────────────────────

    // File input: load project config and compute effective flags.
    let config = load_config(&input)?;
    let cfg_source_map = config
        .as_ref()
        .map(|c| c.config.build.source_map)
        .unwrap_or(false);
    let cfg_embed_sources = config
        .as_ref()
        .map(|c| c.config.build.embed_sources)
        .unwrap_or(false);

    let use_source_map = (flag_source_map || cfg_source_map) && !no_source_map;
    let use_embed_sources = flag_embed_sources || cfg_embed_sources;

    // C3/D6: embed_sources without source_map is a no-op; warn so users don't wonder
    // why their output contains no sourcesContent (applies PF-004 — both paths checked).
    if use_embed_sources && !use_source_map && !quiet {
        crate::output::ewriteln!(
            "warning: embed_sources has no effect without source maps; \
             set build.source_map=true or pass --source-map"
        );
    }

    // AC-SEC-02: warn when shipping full source text in a distributable artifact.
    if use_embed_sources && inline && !quiet {
        crate::output::ewriteln!(
            "warning: --embed-sources with --inline ships full source text in the output \
             (AC-SEC-02)"
        );
    }

    let source_map_base = use_source_map
        .then(|| compute_source_map_base(&input, &output, &out_dir, &config))
        .transpose()?;
    let opts = mds::CompileOptions::default()
        .with_source_map(use_source_map)
        .with_include_sources_content(use_embed_sources)
        .with_source_map_base(source_map_base);

    let compiled = compile_to_content(&input, runtime_vars, quiet, opts)?;
    // `mds build` holds only the typed path: the output is resolved from it, and
    // `admit_output` canonicalizes both sides itself.
    let entry = EntryPaths::as_typed(&input);
    let output_path =
        resolve_output_path_for_kind(Some(entry), &output, &out_dir, &config, compiled.kind)?;
    // #425: nothing — output or sidecar map — is written once the output is the entry,
    // and no warning announces that write.
    let written_path = output_path.as_ref().map(|t| t.path.as_path());
    admit_output(written_path, entry, &output, compiled.kind, quiet)
        .map_err(miette::Error::from)?;

    let mut source_map = compiled.source_map;
    if let Some(ref mut sm) = source_map {
        apply_source_map_file_label(sm, written_path, false);
    }

    if use_source_map {
        if inline {
            // Inline: embed carrier into the output, then write (ADR-002 not applicable:
            // inline mode intentionally modifies the output file).
            let map_json = source_map.as_ref().map(|sm| sm.to_json());
            let final_content = if let Some(ref json) = map_json {
                embed_carrier(compiled.content, json)
            } else {
                compiled.content
            };
            write_output(output_path.as_ref(), &final_content, quiet, true)?;

            // Stale sidecar reconciliation: if there is an existing .map file from
            // a prior sidecar build, remove it (AC-FUNC-10).
            if let Some(ref out) = output_path {
                verify_then_delete_map(&out.sibling(map_path_for), &file_name_of(out), quiet)?;
            }
        } else {
            // Sidecar: write output byte-identical to no-flag build (ADR-002).
            match &output_path {
                None => {
                    // Sidecar + stdout: three-case ladder.
                    if source_map.is_none() {
                        // Case 1 — messages mode: compiler produced no map; write to stdout.
                        return write_output(None, &compiled.content, quiet, true);
                    }
                    if flag_source_map {
                        // Case 2 — explicit --source-map flag: hard error.
                        return Err(miette::miette!(
                            "--source-map (sidecar) requires an output file; use -o <file> or \
                             --out-dir, --inline to embed the map for stdout, or --no-source-map \
                             to skip it"
                        ));
                    }
                    // Case 3 — config-sourced source_map: degrade gracefully. The warning
                    // names `mds.json` as the input reached it, as a config error does
                    // (#390).
                    if !quiet {
                        let cfg_path = config
                            .as_ref()
                            .map(ProjectConfig::shown_file)
                            .unwrap_or_else(|| PathBuf::from("mds.json"));
                        crate::output::ewriteln!(
                            "warning: source_map in {} has no effect when writing to \
                             stdout (sidecar requires -o <file> or --out-dir); use --inline to \
                             embed the map, or --no-source-map to silence this warning",
                            crate::output::safe_path(&cfg_path)
                        );
                    }
                    return write_output(None, &compiled.content, quiet, true);
                }
                Some(out) => {
                    write_output(Some(out), &compiled.content, quiet, true)?;
                    if let Some(ref sm) = source_map {
                        let map = out.sibling(map_path_for);
                        let map_json = sm.to_json();
                        atomic_write_file(
                            &map,
                            &map_json,
                            Durability::RenameOnly,
                            Parents::Create,
                        )?;
                        if !quiet {
                            crate::output::ewriteln!(
                                "Source map written to {}",
                                crate::output::safe_path(&map.shown)
                            );
                        }
                    }
                }
            }
        }
    } else {
        write_output(output_path.as_ref(), &compiled.content, quiet, true)?;

        // No-source-map build: if a stale sidecar exists from a prior source-map build,
        // remove it (AC-FUNC-10).
        if let Some(ref out) = output_path {
            verify_then_delete_map(&out.sibling(map_path_for), &file_name_of(out), quiet)?;
        }
    }

    Ok(())
}

/// The file name of `output`, the name its sidecar map records as `file` — lossy, as
/// [`has_sidecar_head`] compares it.
fn file_name_of(output: &WriteTarget) -> String {
    output
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Compile every non-partial `.mds` file under `dir`, streaming one at a time
/// (AC-PERF-02: peak RSS ≈ O(largest single file), not O(total)).
///
/// Continue-on-error: a per-file compile error does NOT abort the run.
/// All valid files are written; a summary is printed (unless `--quiet` and the run
/// succeeded); non-zero exit when any failed (AC-FUNC-18).
///
/// **Summary / quiet contract (AD-216-1):** the summary line is suppressed under
/// `--quiet` on a fully-successful run (`fail_count == 0`).  When any file fails,
/// the summary is always emitted so the non-zero exit is never unexplained.
/// This mirrors the gate used by `mds check` (`main.rs`) and `mds fmt` (`fmt.rs`).
///
/// **Nothing to build is an error (#204, #387):** when the walk yields no files to
/// compile the run exits 1 with a one-line stderr diagnostic that bypasses `--quiet`.
/// Three shapes, in the order they are checked: the all-excluded count diagnostic
/// (every candidate sits under a default-excluded directory), `no .mds files found
/// in <dir>; nothing was built` (a genuinely empty tree), and the partials-only
/// count diagnostic (#387 — the tree is non-empty but every candidate is a
/// `_`-prefixed partial). All three exit through [`crate::output::exit`]: no `MdsError`
/// variant exists for "nothing to do" and `exit_code` must not grow one for a
/// non-error class.
/// `mds check` mirrors all three with exit 1; `mds fmt` (exit 1) and `mds lint`
/// (exit 2) mirror only the first two — they iterate every file including partials,
/// so a partials-only tree is real work for them, not "nothing to do".
/// `mds watch <dir>` deliberately does NOT error on any of the three.
///
/// **I/O failures (#157):** an output directory that cannot be created, an output or
/// `.map` sidecar that cannot be written, and a stale sibling that cannot be read or
/// whose proven removal fails are each reported as one `mds::io` error through
/// [`crate::output::eprint_io_failure`], which records it for the exit code, so the run
/// exits 2 while the other files are still built. A compile that fails is reported
/// through [`crate::output::eprint_file_failure`], which records it the same way when it
/// is an I/O or file-system failure — a source that cannot be read, say. All but the
/// stale sibling count their file as failed; its file was built. A run whose failures are
/// only template errors or resource limits exits 1.
///
/// Subtree mirroring: with `--out-dir`, mirrors the source subtree into the out-dir
/// with the intrinsic extension per file (AC-FUNC-16). Without `--out-dir`, each
/// output is placed next to its source (AC-FUNC-19).
///
/// Stale-output cleanup: after writing an output, looks at the other kind's output of
/// the same name, left when the source compiled to that kind, and removes it only when
/// mds provably wrote it — a stale `.md` never — keeping anything else with a warning
/// ([`crate::output::probe_and_remove_stale`], #160).
fn run_build_directory(
    dir: &Path,
    out_dir: Option<PathBuf>,
    runtime_vars: Option<HashMap<String, mds::Value>>,
    quiet: bool,
    source_map: bool,
    embed_sources: bool,
    inline: bool,
) -> Result<()> {
    use crate::output::{
        collect_mds_files_detailed, is_partial, output_base_no_ext, output_path_for,
        probe_and_remove_stale, resolve_output_base, OutputBase, RootPaths,
    };

    const MAX_DEPTH: usize = 64;

    // Load project config from the directory root.
    let config = load_config(dir)?;
    // The out-dir's canonical form keeps the starts_with checks reliable; its shown form
    // is the out-dir as typed (#390).
    let output_base = resolve_output_base(out_dir.as_ref(), &config)?;

    // Canonicalize dir for the starts_with comparison (issue 4): the out-dir's canonical
    // form (d) is canonical, but `dir` may be a raw relative or pre-resolved-but-not-canonical path.
    // Mismatch (e.g. /private/tmp vs /tmp on macOS) causes the exclusion to silently fail
    // and includes the out-dir in the collection — not a security issue (only .mds are
    // gathered) but causes redundant scanning. Fall back to raw dir when canonicalize fails
    // (dir may not exist yet in unusual configurations).
    let canonical_dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());

    // Exclude the out-dir from collection when it is nested inside the source root.
    let exclude_prefix: Option<PathBuf> = match &output_base {
        OutputBase::Dir { canonical: d, .. } if d.starts_with(&canonical_dir) => Some(d.clone()),
        _ => None,
    };

    let walk = collect_mds_files_detailed(dir, MAX_DEPTH, exclude_prefix.as_deref());
    let files = walk.files;

    if files.is_empty() {
        if walk.excluded_by_default > 0 {
            // All candidates were inside default-excluded directories.  Emit the
            // diagnostic even under --quiet (avoids a silent CI green pass — avoids
            // PF-004 enforcement gap where the limit is real on one path and absent
            // on another).
            crate::output::ewriteln!(
                "{} .mds file(s) found but all are under default-excluded directories \
                 (hidden dirs, node_modules); nothing was built",
                walk.excluded_by_default
            );
            crate::output::exit(1);
        }
        // #204: an empty tree is "nothing to build", not success.  Same shape as the
        // all-excluded arm above — emitted even under --quiet (a silent green pass on
        // a mistyped or not-yet-populated directory is the CI failure mode this
        // closes) and exit 1, the build/check/fmt "nothing was done" code (spec §7.9).
        // `mds watch <dir>` deliberately still starts on an empty tree: a file created
        // later is a valid flow there.
        crate::output::ewriteln!(
            "no .mds files found in {}; nothing was built",
            crate::output::safe_path(dir)
        );
        crate::output::exit(1);
    }

    // #387: a tree whose only .mds files are partials is "nothing to build" too. The
    // walker collects partials (watch/fmt/lint need them) but this loop skips them, so
    // without this arm the run ends `0 built, 0 failed`, exit 0 — the silent green pass
    // #204 closed for the empty tree. Same shape as the all-excluded arm: count-carrying,
    // emitted even under --quiet, exit 1. fmt and lint operate on partials and keep their
    // behaviour; `mds watch <dir>` still starts.
    if let Some(partials_only_count) = crate::output::partials_only(&files) {
        crate::output::ewriteln!(
            "{partials_only_count} .mds file(s) found in {} but all are _-prefixed partials; \
             nothing was built",
            crate::output::safe_path(dir)
        );
        crate::output::exit(1);
    }

    let mut ok_count: usize = 0;
    let mut fail_count: usize = 0;
    // R5: successful compilations whose written artifact is ZERO bytes (strict
    // `is_empty()`, not trim — messages-mode JSON is never empty, and a
    // whitespace-only markdown body is still content the author wrote).  A
    // definitions-only module compiles to zero bytes silently; "N built" alone
    // implies N useful artifacts (PF-034-adjacent).
    let mut empty_count: usize = 0;
    // Track paths successfully written in this build run so the stale-cleanup
    // step can verify it was tool-produced before deleting (issue 1 guard):
    // prevents clobbering a hand-authored file that shares a stem with a .mds
    // (e.g. notes.md kept next to notes.mds that now compiles to notes.json).
    // Same-stem .md/.json files adjacent to a .mds in NextToSource mode are
    // considered tool-owned; if a collision is a concern use --out-dir to
    // separate source and output trees.
    let mut written_this_run: HashSet<PathBuf> = HashSet::new();

    for file in &files {
        // Skip partials: they contribute to imports but produce no standalone output.
        if is_partial(file) {
            continue;
        }

        // Per-file source_map_base: the output directory for this file, computed
        // from the kind-independent directory oracle, without creating it — an early
        // create_dir_all would leave an empty directory on compile failure (Step 6
        // Caveat 1). A walked file's output stem always has a parent, the directory it
        // lands in, so the base is always that directory.
        let base_no_ext = output_base_no_ext(file, dir, &output_base);
        let opts = mds::CompileOptions::default()
            .with_source_map(source_map)
            .with_include_sources_content(embed_sources)
            .with_source_map_base(base_no_ext.parent().map(Path::to_path_buf));

        // Compile (all reads go through mds-core which enforces MAX_FILE_SIZE — PF-004).
        // A panic in it fails this file alone, and the batch goes on (#389).
        let compiled = crate::output::catch_compile(
            file,
            AssertUnwindSafe(|| compile_to_content(file, runtime_vars.clone(), quiet, opts)),
        );
        match compiled {
            Ok(Ok(mut compiled)) => {
                let ext = compiled.kind.extension();
                let target = output_path_for(file, RootPaths::as_typed(dir), &output_base, ext);

                // Set `file` field for this output path (sources already relativized by core).
                if let Some(ref mut sm) = compiled.source_map {
                    apply_source_map_file_label(sm, Some(&target.path), false);
                }

                // Determine final content (inline embeds the carrier).
                let final_content = if source_map && inline {
                    if let Some(ref sm) = compiled.source_map {
                        embed_carrier(compiled.content, &sm.to_json())
                    } else {
                        compiled.content
                    }
                } else {
                    compiled.content
                };

                // R5: capture emptiness BEFORE the write moves/borrows the content;
                // counted only in the Ok arm below — a failed write is a failure,
                // not an empty success.
                let wrote_empty = final_content.is_empty();

                // #227: the dir-mode twin of `write_output` — same atomic contract, but
                // it accumulates per-file counters instead of returning early, so it is
                // its own call site. Both are enforced by `tests/write_funnel.rs`. The
                // write creates the out-dir and the mirrored directories below it, and
                // refuses a symlink among them (#160).
                match atomic_write_file(
                    &target,
                    &final_content,
                    Durability::RenameOnly,
                    Parents::Create,
                ) {
                    Ok(()) => {
                        if wrote_empty {
                            empty_count += 1;
                        }
                        if !quiet {
                            crate::output::ewriteln!(
                                "Compiled to {}",
                                crate::output::safe_path(&target.shown)
                            );
                        }
                        written_this_run.insert(target.path.clone());

                        // Write sidecar map (non-inline mode).
                        if source_map && !inline {
                            if let Some(ref sm) = compiled.source_map {
                                let map = target.sibling(map_path_for);
                                let map_json = sm.to_json();
                                if let Err(e) = atomic_write_file(
                                    &map,
                                    &map_json,
                                    Durability::RenameOnly,
                                    Parents::Create,
                                ) {
                                    // The primitive's message already names the path —
                                    // re-prefixing it would print the path twice (#227).
                                    crate::output::eprint_io_failure(e);
                                    fail_count += 1;
                                    continue;
                                }
                                if !quiet {
                                    crate::output::ewriteln!(
                                        "Source map written to {}",
                                        crate::output::safe_path(&map.shown)
                                    );
                                }
                            }
                        }

                        // Stale-output cleanup (#160): the other kind's output of the
                        // name just written — left by a build when the source compiled
                        // to that kind — is removed only when mds provably wrote it, and
                        // a stale `.md` never (see `probe_and_remove_stale`). Below an
                        // out-dir every output is looked at; next to the source only one
                        // this run wrote, so a hand-authored file of that name is never
                        // touched.
                        let stale_path =
                            target.path.with_extension(compiled.kind.stale_extension());
                        let looked_at = matches!(output_base, OutputBase::Dir { .. })
                            || written_this_run.contains(&stale_path);
                        if looked_at {
                            // The output itself was built, so the file is not counted as
                            // failed; a failed removal still lifts the exit code (#157).
                            if let Err(e) = probe_and_remove_stale(&target, compiled.kind, quiet) {
                                crate::output::eprint_io_failure(e);
                            }
                        }
                        ok_count += 1;
                    }
                    Err(e) => {
                        // The primitive's message already names the path (#227).
                        crate::output::eprint_io_failure(e);
                        fail_count += 1;
                    }
                }
            }
            Ok(Err(e)) => {
                // Route through the single render choke point (avoids PF-004 /
                // architecture-6: hand-rolled sanitize_control_chars bypass); a source
                // that cannot be read lifts the exit code to 2 (#157).
                crate::output::eprint_file_failure(e);
                fail_count += 1;
            }
            // The panic hook reported it, and the run will exit 101.
            Err(crate::output::Panicked) => fail_count += 1,
        }
    }

    // AD-216-1: mirror the gate used by `mds check` (main.rs) and `mds fmt` (fmt.rs):
    // suppress the summary under --quiet when every file succeeded, so a quiet CI job
    // produces no output on a clean run.  AD-216-2: exit codes are untouched — a
    // --quiet build with failures still exits non-zero with the summary explaining why
    // (1, or 2 once an I/O failure was recorded — the exit funnel applies it, #157).
    // R5: the empty clause appears ONLY when empty_count > 0 — the historical
    // `{ok} built, {fail} failed` form is byte-identical otherwise (no golden
    // churn).  The suppression gate itself is unchanged (AD-216-1): empty
    // outputs are informational, not failures, so they do not force the summary
    // through --quiet.
    if !quiet || fail_count > 0 {
        if empty_count > 0 {
            crate::output::ewriteln!("{ok_count} built ({empty_count} empty), {fail_count} failed");
        } else {
            crate::output::ewriteln!("{ok_count} built, {fail_count} failed");
        }
    }

    if fail_count > 0 {
        crate::output::exit(1);
    }
    Ok(())
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// #390: when the working directory holds several `.mds` files, the error lists their
    /// names escaped. The user never typed them, and a name may hold a newline, which
    /// would otherwise start a line of its own inside the error. The control characters
    /// and their escaped forms are built at run time from numbers.
    ///
    /// Positive controls: the name carries the raw newline, so the vector reaches the
    /// message; ordinary names keep their text and their order.
    #[test]
    fn several_mds_files_names_each_file_escaped() {
        let (lf, esc) = (char::from(0x0a_u8), char::from(0x1b_u8));
        let hostile = format!("x{lf}Built forged{esc}.mds");
        assert!(
            hostile.contains(lf),
            "control: the name carries a raw newline"
        );
        let entries = [PathBuf::from("b.mds"), PathBuf::from(&hostile)];
        assert_eq!(
            several_mds_files("build", &entries).to_string(),
            format!(
                "multiple .mds files found: b.mds, x{}u000ABuilt forged{}u001B.mds\n  \
                 hint: specify which file, e.g. 'mds build b.mds'",
                '\\', '\\'
            ),
            "each name must be shown escaped"
        );

        let entries = [PathBuf::from("b.mds"), PathBuf::from("a.mds")];
        assert_eq!(
            several_mds_files("lint", &entries).to_string(),
            "multiple .mds files found: a.mds, b.mds\n  \
             hint: specify which file, e.g. 'mds lint a.mds'",
            "control: ordinary names keep their text and their order"
        );
    }

    /// #425: `admit_output` refuses an output that is the entry file and names the entry
    /// by `typed`, never by `canonical`; any other output is admitted, stdout included.
    /// Every write site — `mds build`, `mds watch`'s startup compile, its startup
    /// fallback and every rebuild — passes the entry as one `EntryPaths`, so none of them
    /// can hand the two forms over swapped.
    #[test]
    fn admit_output_refuses_naming_the_entry_as_typed() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("page.md"), "x").unwrap();
        let typed = dir.path().join("sub").join("..").join("page.md");
        let canonical = dir.path().join("page.md").canonicalize().unwrap();
        let entry = EntryPaths {
            typed: &typed,
            canonical: &canonical,
        };
        let admit = |output: Option<&Path>| {
            admit_output(output, entry, &None, OutputKind::Markdown, true)
                .map_err(|e| e.to_string())
        };

        let refused = admit(Some(&canonical)).unwrap_err();
        assert_eq!(
            refused,
            format!(
                "output would overwrite the entry file: \"{}\"; \
                 write it elsewhere with -o <file> or --out-dir <dir>",
                typed.display()
            )
        );
        assert!(
            !refused.contains(&*canonical.to_string_lossy()),
            "{refused}"
        );
        assert_eq!(
            admit(Some(&dir.path().join("sub").join("..").join("page.md"))),
            Err(refused),
            "another spelling of the entry is the entry"
        );

        assert_eq!(admit(Some(&dir.path().join("out.md"))), Ok(()), "control");
        assert_eq!(admit(None), Ok(()), "stdout is never the entry");
    }

    /// #425: an output whose directory does not exist yet is the file the write will
    /// reach once `write_output` has created that directory. A created directory is a
    /// plain one, so a `..` after it leads back to its parent — the entry's directory
    /// here — while an existing component is followed as the write follows it, a
    /// symlink included. The name is then looked up in the directory reached, so on a
    /// case-insensitive volume a case variant of the entry's name is the entry there
    /// too, however that directory was reached. Nothing is created by asking.
    #[test]
    fn file_identity_resolves_a_directory_the_write_creates() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("page.md"), "x").unwrap();
        let page = root.join("page.md");
        let case_insensitive = root.join("PAGE.md").exists();
        // A case variant is the entry on a case-insensitive volume, another file on a
        // case-sensitive one: each volume holds the variant to its own answer.
        let variant = if case_insensitive {
            page.clone()
        } else {
            root.join("PAGE.md")
        };

        let rows = [
            ("page.md", page.clone()),
            ("sub/../page.md", page.clone()),
            ("newdir/../page.md", page.clone()),
            ("a/b/../../page.md", page.clone()),
            ("sub/new/../../page.md", page.clone()),
            ("new/./x/../../page.md", page.clone()),
            ("newdir/page.md", root.join("newdir").join("page.md")),
            (
                "sub/new/page.md",
                root.join("sub").join("new").join("page.md"),
            ),
            ("PAGE.md", variant.clone()),
            ("newdir/../PAGE.md", variant.clone()),
            ("a/b/../../PAGE.md", variant),
        ];
        // `dir.path()` is not canonical on macOS (`/var` → `/private/var`), so the
        // existing part of each path is canonicalized, not merely joined.
        let mismatches: Vec<String> = rows
            .iter()
            .filter_map(|(rel, expected)| {
                let got = file_identity(&dir.path().join(rel));
                (got.as_ref() != Some(expected)).then(|| format!("{rel}: {got:?}"))
            })
            .collect();
        assert!(mismatches.is_empty(), "{mismatches:#?}");

        #[cfg(unix)]
        {
            // Out of a directory the write creates, then through an existing link that
            // leads back to the entry's directory: the link is followed.
            std::os::unix::fs::symlink(&root, root.join("lnk")).unwrap();
            assert_eq!(
                file_identity(&dir.path().join("newdir/../lnk/page.md")),
                Some(page.clone())
            );
        }
        assert!(!root.join("newdir").exists() && !root.join("a").exists());
    }

    // ── compute_source_map_base ───────────────────────────────────────────────
    //
    // `source_map_base` is the anchor core's `relativize_source` uses to emit
    // map-relative `sources[]` (ADR-005).  Core resolves it against an ABSOLUTE
    // project root, so a relative base fails the containment check and silently
    // demotes the result to root-relative — a `sources[]` entry that no longer
    // resolves from the map file's directory.  The invariant is therefore
    // "every branch returns an absolute path", asserted here per branch because
    // the failure mode is silent (wrong paths, not an error).

    #[test]
    fn source_map_base_is_absolute_for_every_output_mode() {
        let input = PathBuf::from("src/a.mds");
        let cases: Vec<(&str, Option<String>, Option<PathBuf>)> = vec![
            ("-o - (stdout)", Some("-".to_string()), None),
            (
                "-o relative/out.md",
                Some("relative/out.md".to_string()),
                None,
            ),
            ("-o bare.md", Some("bare.md".to_string()), None),
            ("-o /abs/out.md", Some("/abs/out.md".to_string()), None),
            ("--out-dir relative", None, Some(PathBuf::from("dist"))),
            ("--out-dir /abs", None, Some(PathBuf::from("/abs/dist"))),
            ("default (beside source)", None, None),
        ];
        for (label, output, out_dir) in cases {
            let got = compute_source_map_base(&input, &output, &out_dir, &None)
                .unwrap_or_else(|e| panic!("{label}: the working directory exists: {e}"));
            assert!(
                got.is_absolute(),
                "{label}: source_map_base must be absolute so core's root-containment \
                 check succeeds; got {got:?}"
            );
        }
    }

    #[test]
    fn source_map_base_for_bare_filename_output_is_cwd() {
        // PF-006: Path::parent() of a bare filename is Some("") not None, so a
        // naive parent() would yield an empty (relative) base here.
        let got = compute_source_map_base(
            Path::new("src/a.mds"),
            &Some("out.md".to_string()),
            &None,
            &None,
        )
        .expect("bare -o must still produce a base");
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            got, cwd,
            "`-o out.md` writes into the CWD, so the map base is the CWD"
        );
    }

    #[test]
    fn source_map_base_tracks_the_output_file_directory() {
        // The map is written beside the output file, so the base must be the
        // output file's directory — this is what makes sources[] map-relative.
        let got = compute_source_map_base(
            Path::new("src/a.mds"),
            &Some("dist/nested/a.md".to_string()),
            &None,
            &None,
        )
        .expect("base must be Some");
        assert_eq!(got, std::env::current_dir().unwrap().join("dist/nested"));
    }

    #[test]
    fn parse_cli_value_nan_is_string() {
        // "NaN".parse::<f64>() succeeds but is not finite — must fall through to string.
        assert_eq!(
            parse_cli_value("NaN".to_string()),
            mds::Value::String("NaN".to_string()),
            "--set val=NaN must produce Value::String, not Value::Number(NaN)"
        );
    }

    #[test]
    fn parse_cli_value_infinity_is_string() {
        assert_eq!(
            parse_cli_value("Infinity".to_string()),
            mds::Value::String("Infinity".to_string()),
            "--set val=Infinity must produce Value::String, not Value::Number(inf)"
        );
    }

    #[test]
    fn parse_cli_value_neg_infinity_is_string() {
        assert_eq!(
            parse_cli_value("-Infinity".to_string()),
            mds::Value::String("-Infinity".to_string()),
            "--set val=-Infinity must produce Value::String, not Value::Number(-inf)"
        );
    }

    #[test]
    fn parse_cli_value_finite_float_is_number() {
        // Sanity check: legitimate floats still parse as numbers.
        // Use 2.5 (exact in binary) to avoid clippy::approx_constant warning.
        assert_eq!(
            parse_cli_value("2.5".to_string()),
            mds::Value::Number(2.5),
            "finite float must still become Value::Number"
        );
    }

    #[test]
    fn derive_output_filename_swaps_mds_extension() {
        assert_eq!(
            derive_output_filename_for_kind(Path::new("foo.mds"), OutputKind::Markdown),
            OsString::from("foo.md")
        );
    }

    #[test]
    fn derive_output_filename_preserves_compound_extension() {
        assert_eq!(
            derive_output_filename_for_kind(Path::new("foo.bar.mds"), OutputKind::Markdown),
            OsString::from("foo.bar.md")
        );
    }

    #[test]
    fn derive_output_filename_no_extension() {
        assert_eq!(
            derive_output_filename_for_kind(Path::new("README"), OutputKind::Markdown),
            OsString::from("README.md")
        );
    }

    #[test]
    fn derive_output_filename_other_extension() {
        assert_eq!(
            derive_output_filename_for_kind(Path::new("foo.txt"), OutputKind::Markdown),
            OsString::from("foo.md")
        );
    }

    #[test]
    fn resolve_output_path_dash_o_dash_is_stdout() {
        let result = resolve_output_path_for_kind(
            Some(EntryPaths::as_typed(Path::new("foo.mds"))),
            &Some("-".to_string()),
            &None,
            &None,
            OutputKind::Markdown,
        )
        .unwrap();
        assert_eq!(result, None, "-o - should resolve to stdout (None)");
    }

    #[test]
    fn resolve_output_path_stdin_no_o_is_stdout() {
        let result = resolve_output_path_for_kind(
            Some(EntryPaths::as_typed(Path::new("-"))),
            &None,
            &None,
            &None,
            OutputKind::Markdown,
        )
        .unwrap();
        assert_eq!(
            result, None,
            "stdin input with no -o should resolve to stdout"
        );
    }

    #[test]
    fn resolve_output_path_default_file_next_to_source() {
        let result = resolve_output_path_for_kind(
            Some(EntryPaths::as_typed(Path::new("/some/dir/hello.mds"))),
            &None,
            &None,
            &None,
            OutputKind::Markdown,
        )
        .unwrap();
        assert_eq!(
            result,
            Some(WriteTarget::as_typed(PathBuf::from("/some/dir/hello.md"))),
            "default should produce .md next to source"
        );
    }

    /// #390: an entry held in two forms, as `mds watch` holds it, has its default output
    /// written beside the canonical form and named beside the typed one — `./hello.md`
    /// for a bare name, `..` kept as typed — while `--out-dir` takes the name from the
    /// canonical form and shows the out-dir as typed. #160: each is anchored at the
    /// directory it is written in.
    #[test]
    fn resolve_output_path_default_is_named_beside_the_entry_as_typed() {
        let resolve = |typed: &str, out_dir: Option<&str>| {
            resolve_output_path_for_kind(
                Some(EntryPaths {
                    typed: Path::new(typed),
                    canonical: Path::new("/some/dir/hello.mds"),
                }),
                &None,
                &out_dir.map(PathBuf::from),
                &None,
                OutputKind::Markdown,
            )
            .unwrap()
        };
        assert_eq!(
            resolve("hello.mds", None),
            Some(WriteTarget::below(
                Path::new("/some/dir"),
                Path::new("."),
                Path::new("hello.md")
            ))
        );
        assert_eq!(
            resolve("sub/../sub/hello.mds", None),
            Some(WriteTarget::below(
                Path::new("/some/dir"),
                Path::new("sub/../sub"),
                Path::new("hello.md")
            ))
        );
        assert_eq!(
            resolve("hello.mds", Some("out")),
            Some(WriteTarget::as_typed(PathBuf::from("out/hello.md")))
        );
    }

    #[test]
    fn resolve_output_path_stdin_with_out_dir_uses_out_dir() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = dir.path().join("out");
        let result = resolve_output_path_for_kind(
            Some(EntryPaths::as_typed(Path::new("-"))),
            &None,
            &Some(out_dir.clone()),
            &None,
            OutputKind::Markdown,
        )
        .unwrap();
        assert_eq!(
            result,
            Some(WriteTarget::as_typed(out_dir.join("output.md"))),
            "stdin with --out-dir should produce output.md inside the out dir"
        );
        // #425: resolving creates nothing — `write_output` creates the directory, once
        // the output has been admitted — so a refused output leaves no directory behind.
        assert!(!out_dir.exists(), "resolving creates no directory");
    }

    /// A loaded `mds.json` with `build.output_dir = "build"` in `/project`, reached as
    /// `shown_dir`.
    fn config_with_output_dir(shown_dir: &str) -> Option<ProjectConfig> {
        Some(ProjectConfig {
            config: MdsConfig {
                build: BuildConfig {
                    output_dir: Some("build".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
            dir: PathBuf::from("/project"),
            shown_dir: PathBuf::from(shown_dir),
        })
    }

    /// #390: an output under `mds.json` `build.output_dir` is written below the canonical
    /// config directory and named below the directory `mds.json` was reached by. #160:
    /// the config directory is the write's anchor, `build.output_dir` below it.
    #[test]
    fn resolve_output_path_config_output_dir_is_shown_below_the_directory_reached() {
        let result = resolve_output_path_for_kind(
            Some(EntryPaths::as_typed(Path::new("src/hello.mds"))),
            &None,
            &None,
            &config_with_output_dir("src/.."),
            OutputKind::Markdown,
        )
        .unwrap();
        assert_eq!(
            result,
            Some(WriteTarget::below(
                Path::new("/project"),
                Path::new("src/.."),
                &Path::new("build").join("hello.md")
            ))
        );
    }

    #[test]
    fn resolve_output_path_explicit_o_wins_over_config() {
        let config = config_with_output_dir(".");
        let result = resolve_output_path_for_kind(
            Some(EntryPaths::as_typed(Path::new("/project/hello.mds"))),
            &Some("out.md".to_string()),
            &None,
            &config,
            OutputKind::Markdown,
        )
        .unwrap();
        assert_eq!(
            result,
            Some(WriteTarget::as_typed(PathBuf::from("out.md"))),
            "-o should win over mds.json config"
        );
    }

    // ── T5: malformed fmt config fails config loading ─────────────────────────

    #[test]
    fn fmt_config_malformed_bool_field_fails_loading() {
        // `FmtConfig.sort_frontmatter_keys` is a `bool`. Supplying a string
        // value must cause `serde_json` to reject the config, so `load_config`
        // returns `Err` rather than silently using the default. This ensures a
        // bad `mds.json` is reported loudly rather than quietly ignored.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("mds.json"),
            r#"{"fmt": {"sort_frontmatter_keys": "not-a-bool"}}"#,
        )
        .unwrap();

        let result = load_config(dir.path());
        assert!(
            result.is_err(),
            "a malformed fmt config (wrong type for sort_frontmatter_keys) must fail config loading"
        );
    }

    #[test]
    fn fmt_config_valid_section_loads_cleanly() {
        // Complement to the malformed test: a well-typed fmt section must parse
        // without error and produce the expected field value.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("mds.json"),
            r#"{"fmt": {"sort_frontmatter_keys": false}}"#,
        )
        .unwrap();

        let result = load_config(dir.path()).expect("valid fmt config must load");
        let config = result.expect("mds.json must be found").config;
        assert!(
            !config.fmt.sort_frontmatter_keys,
            "sort_frontmatter_keys: false must deserialize correctly"
        );
    }

    // ── build_runtime_vars: cross-flag duplicate rejection (#152) ─────────────

    #[test]
    fn build_runtime_vars_cross_flag_duplicate_is_error() {
        // A key present in both --set and --set-string must be a hard error.
        let result = build_runtime_vars(RuntimeVarArgs {
            vars: None,
            set_vars: vec![("x".to_string(), "1".to_string())],
            set_string_vars: vec![("x".to_string(), "2".to_string())],
        });
        assert!(result.is_err(), "cross-flag duplicate key must be rejected");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("variable 'x' is set by both --set and --set-string"),
            "error message must identify the key; got: {msg}"
        );
        assert!(
            msg.contains("use only one"),
            "error message must say 'use only one'; got: {msg}"
        );
    }

    #[test]
    fn build_runtime_vars_check_command_cross_flag_duplicate_is_error() {
        // The same build_runtime_vars function is used by both mds build and mds check.
        // Verify the error fires regardless of which args struct wraps it.
        let result = build_runtime_vars(RuntimeVarArgs {
            vars: None,
            set_vars: vec![("count".to_string(), "3".to_string())],
            set_string_vars: vec![("count".to_string(), "three".to_string())],
        });
        assert!(
            result.is_err(),
            "cross-flag duplicate must be rejected in check parity"
        );
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("variable 'count'"),
            "error must name the duplicate key; got: {msg}"
        );
    }

    #[test]
    fn build_runtime_vars_same_flag_last_wins() {
        // Within a single flag group, the last occurrence wins (no error).
        let resolved = build_runtime_vars(RuntimeVarArgs {
            vars: None,
            set_vars: vec![
                ("x".to_string(), "1".to_string()),
                ("x".to_string(), "2".to_string()),
            ],
            set_string_vars: vec![],
        })
        .expect("same-flag last-wins must not error");
        let map = resolved.vars.expect("non-empty vars");
        assert_eq!(
            map.get("x"),
            Some(&mds::Value::Number(2.0)),
            "last --set wins within the same flag"
        );
        // U1: the duplicate key is reported in the struct.
        assert_eq!(
            resolved.duplicate_set_keys,
            vec!["x".to_string()],
            "duplicate_set_keys must list 'x'"
        );
        assert!(
            resolved.duplicate_set_string_keys.is_empty(),
            "duplicate_set_string_keys must be empty"
        );
    }

    #[test]
    fn build_runtime_vars_same_flag_string_last_wins() {
        // Within --set-string, the last occurrence also wins (no error).
        let resolved = build_runtime_vars(RuntimeVarArgs {
            vars: None,
            set_vars: vec![],
            set_string_vars: vec![
                ("id".to_string(), "007".to_string()),
                ("id".to_string(), "009".to_string()),
            ],
        })
        .expect("same-flag last-wins in --set-string must not error");
        let map = resolved.vars.expect("non-empty vars");
        assert_eq!(
            map.get("id"),
            Some(&mds::Value::String("009".to_string())),
            "last --set-string wins within the same flag"
        );
        // U2: the duplicate key is reported in the struct.
        assert_eq!(
            resolved.duplicate_set_string_keys,
            vec!["id".to_string()],
            "duplicate_set_string_keys must list 'id'"
        );
        assert!(
            resolved.duplicate_set_keys.is_empty(),
            "duplicate_set_keys must be empty"
        );
    }

    // ── U3: a key repeated 3+ times is reported exactly once ─────────────────

    #[test]
    fn build_runtime_vars_triple_repeat_reported_once() {
        // RED: duplicate_set_keys should list "x" exactly once, not twice or three times.
        let resolved = build_runtime_vars(RuntimeVarArgs {
            vars: None,
            set_vars: vec![
                ("x".to_string(), "1".to_string()),
                ("x".to_string(), "2".to_string()),
                ("x".to_string(), "3".to_string()),
            ],
            set_string_vars: vec![],
        })
        .expect("triple repeat must not error");
        assert_eq!(
            resolved.duplicate_set_keys,
            vec!["x".to_string()],
            "a key repeated 3 times must appear exactly once in duplicate_set_keys; \
             got: {:?}",
            resolved.duplicate_set_keys
        );
    }

    // ── U4: distinct keys produce no duplicates (negative control) ────────────

    #[test]
    fn build_runtime_vars_distinct_keys_no_duplicates() {
        // RED: duplicate lists must be empty when every key appears exactly once.
        let resolved = build_runtime_vars(RuntimeVarArgs {
            vars: None,
            set_vars: vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string()),
            ],
            set_string_vars: vec![("c".to_string(), "three".to_string())],
        })
        .expect("distinct keys must not error");
        assert!(
            resolved.duplicate_set_keys.is_empty(),
            "distinct --set keys must produce no duplicates; got: {:?}",
            resolved.duplicate_set_keys
        );
        assert!(
            resolved.duplicate_set_string_keys.is_empty(),
            "distinct --set-string keys must produce no duplicates; got: {:?}",
            resolved.duplicate_set_string_keys
        );
    }

    // ── U5: cross-flag hard error precedes duplicate detection ───────────────

    #[test]
    fn build_runtime_vars_cross_flag_wins_over_intra_flag_duplicate() {
        // U5: a case that is BOTH a cross-flag collision AND an intra-flag duplicate
        // (x in --set twice, x in --set-string once) must return the cross-flag ERROR,
        // not a warning about the intra-flag duplicate.
        let result = build_runtime_vars(RuntimeVarArgs {
            vars: None,
            set_vars: vec![
                ("x".to_string(), "1".to_string()),
                ("x".to_string(), "2".to_string()),
            ],
            set_string_vars: vec![("x".to_string(), "three".to_string())],
        });
        assert!(
            result.is_err(),
            "cross-flag collision must be a hard error even when --set also has a duplicate"
        );
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("variable 'x' is set by both --set and --set-string"),
            "error must identify the cross-flag collision; got: {msg}"
        );
    }

    // ── AC-SEC-03: inline carrier is a self-contained, idempotent HTML comment ──

    #[test]
    fn embed_carrier_is_idempotent() {
        // Re-embedding must strip the existing carrier first, so applying it
        // twice yields the same bytes as applying it once (a distributable
        // artifact must never accumulate stacked carriers).
        let json = r#"{"version":3,"sources":["a.mds"],"mappings":"AAAA"}"#;
        let once = embed_carrier("Hello world\n".to_string(), json);
        let twice = embed_carrier(once.clone(), json);
        assert_eq!(
            once, twice,
            "embedding the carrier twice must be idempotent"
        );
        assert_eq!(
            twice.matches("sourceMappingURL=data:").count(),
            1,
            "the carrier must appear exactly once, got:\n{twice}"
        );
    }

    #[test]
    fn carrier_line_cannot_break_out_of_html_comment() {
        // AC-SEC-03: the base64 payload must exclude '<', '>', and '-' so it
        // cannot terminate the enclosing HTML comment early, even when the map
        // JSON itself contains '-->' (e.g. embedded source text).
        let hostile_json = r#"{"sourcesContent":["--> break out <script>"]}"#;
        let line = carrier_line(hostile_json);
        let payload = line
            .strip_prefix("<!--# sourceMappingURL=data:application/json;base64,")
            .and_then(|s| s.strip_suffix(" -->"))
            .expect("carrier must have the expected envelope");
        assert!(!payload.contains('<'), "payload must not contain '<'");
        assert!(!payload.contains('>'), "payload must not contain '>'");
        assert!(!payload.contains('-'), "payload must not contain '-'");
    }

    #[test]
    fn build_runtime_vars_distinct_keys_no_error() {
        // Using --set and --set-string for DIFFERENT keys must succeed.
        let resolved = build_runtime_vars(RuntimeVarArgs {
            vars: None,
            set_vars: vec![("num".to_string(), "42".to_string())],
            set_string_vars: vec![("id".to_string(), "007".to_string())],
        })
        .expect("distinct keys across flags must not error");
        let map = resolved.vars.expect("non-empty vars");
        assert_eq!(map.get("num"), Some(&mds::Value::Number(42.0)));
        assert_eq!(map.get("id"), Some(&mds::Value::String("007".to_string())));
    }

    // ── #326: duplicate --vars file keys surface on RuntimeVars ───────────────

    #[test]
    fn build_runtime_vars_vars_file_duplicate_key_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vars.json");
        std::fs::write(&path, r#"{"x": 1, "x": 2}"#).unwrap();

        let resolved = build_runtime_vars(RuntimeVarArgs {
            vars: Some(path.clone()),
            set_vars: vec![],
            set_string_vars: vec![],
        })
        .expect("duplicate vars-file key must not error");
        assert_eq!(resolved.duplicate_vars_file_keys, vec!["x".to_string()]);
        assert_eq!(resolved.vars_file, Some(path));
        let map = resolved.vars.expect("non-empty vars");
        assert_eq!(map.get("x"), Some(&mds::Value::Number(2.0)));
    }

    #[test]
    fn build_runtime_vars_vars_file_nested_duplicate_reports_dotted_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vars.json");
        std::fs::write(&path, r#"{"cfg":{"a":1,"a":2}}"#).unwrap();

        let resolved = build_runtime_vars(RuntimeVarArgs {
            vars: Some(path),
            set_vars: vec![],
            set_string_vars: vec![],
        })
        .expect("nested duplicate vars-file key must not error");
        assert_eq!(resolved.duplicate_vars_file_keys, vec!["cfg.a".to_string()]);
    }

    /// Positive control (PF-013): a clean vars file reports no duplicates, while
    /// still populating `vars_file`.
    #[test]
    fn build_runtime_vars_vars_file_without_duplicates_reports_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vars.json");
        std::fs::write(&path, r#"{"name": "World"}"#).unwrap();

        let resolved = build_runtime_vars(RuntimeVarArgs {
            vars: Some(path.clone()),
            set_vars: vec![],
            set_string_vars: vec![],
        })
        .expect("clean vars file must not error");
        assert!(
            resolved.duplicate_vars_file_keys.is_empty(),
            "expected no duplicates, got: {:?}",
            resolved.duplicate_vars_file_keys
        );
        assert_eq!(resolved.duplicate_vars_file_keys_omitted, 0);
        assert!(resolved.vars_file.is_some(), "vars_file must be populated");
    }

    #[test]
    fn build_runtime_vars_no_vars_file_reports_no_duplicates_and_no_path() {
        let resolved = build_runtime_vars(RuntimeVarArgs {
            vars: None,
            set_vars: vec![("a".to_string(), "1".to_string())],
            set_string_vars: vec![],
        })
        .expect("no vars file must not error");
        assert!(
            resolved.duplicate_vars_file_keys.is_empty(),
            "expected no duplicates when no vars file was given, got: {:?}",
            resolved.duplicate_vars_file_keys
        );
        assert_eq!(resolved.duplicate_vars_file_keys_omitted, 0);
        assert_eq!(
            resolved.vars_file, None,
            "vars_file must be None when --vars was not given"
        );
    }

    /// U5 (extended): the cross-flag hard error must precede the vars-file read
    /// entirely — a nonexistent vars path must not surface a file-not-found error
    /// when --set and --set-string also collide.
    #[test]
    fn build_runtime_vars_cross_flag_error_precedes_the_vars_file_read() {
        let result = build_runtime_vars(RuntimeVarArgs {
            vars: Some(PathBuf::from("/does/not/exist/vars.json")),
            set_vars: vec![("x".to_string(), "1".to_string())],
            set_string_vars: vec![("x".to_string(), "2".to_string())],
        });
        assert!(
            result.is_err(),
            "cross-flag collision must be a hard error even with a nonexistent vars path"
        );
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("variable 'x' is set by both --set and --set-string"),
            "error must be the cross-flag collision, not a file-read error; got: {msg}"
        );
    }

    // ── Bounded reads (#428) ────────────────────────────────────────────────────

    /// A stream of `x` bytes — `len` of them, or without end — handed out at most 64 KiB
    /// per read, as a pipe does, counting how many it served.
    struct Stream {
        left: Option<u64>,
        served: u64,
    }

    impl Stream {
        fn sized(len: u64) -> Self {
            Self {
                left: Some(len),
                served: 0,
            }
        }

        fn endless() -> Self {
            Self {
                left: None,
                served: 0,
            }
        }
    }

    impl Read for Stream {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(64 * 1024);
            let n = self
                .left
                .map_or(n, |left| n.min(usize::try_from(left).unwrap_or(n)));
            buf[..n].fill(b'x');
            if let Some(left) = self.left.as_mut() {
                *left -= n as u64;
            }
            self.served += n as u64;
            Ok(n)
        }
    }

    /// #428: stdin is read into a buffer that never holds more than the per-file cap
    /// plus one byte — a buffer only grows, so its final capacity is the most it ever
    /// held — and never past that byte: exactly the cap is accepted, one byte more is
    /// refused, and an endless stream is read to one byte past the cap. #157: more than
    /// the cap is `mds::resource_limit` (exit 3); bytes that are not UTF-8 and a read
    /// that fails are `mds::io` (exit 2).
    #[test]
    fn read_stdin_holds_at_most_the_cap_plus_one_byte() {
        let at_cap = read_stdin_from(&mut Stream::sized(MAX_FILE_SIZE)).unwrap();
        assert_eq!(
            at_cap.len() as u64,
            MAX_FILE_SIZE,
            "exactly the cap is read"
        );
        assert!(
            at_cap.capacity() as u64 <= MAX_FILE_SIZE + 1,
            "capacity {} for {MAX_FILE_SIZE} bytes, over the cap plus one byte",
            at_cap.capacity()
        );

        let over_cap = "resource limit exceeded: stdin input exceeds maximum size of 10 MiB";
        let over: miette::Report = read_stdin_from(&mut Stream::sized(MAX_FILE_SIZE + 1))
            .unwrap_err()
            .into();
        assert_eq!(
            (exit_code(&over), over.to_string()),
            (3, over_cap.to_owned())
        );

        let mut endless = Stream::endless();
        let err: miette::Report = read_stdin_from(&mut endless).unwrap_err().into();
        assert_eq!((exit_code(&err), err.to_string()), (3, over_cap.to_owned()));
        assert_eq!(endless.served, MAX_FILE_SIZE + 1, "read no further");

        let err: miette::Report = read_stdin_from(&mut &b"ok \xff"[..]).unwrap_err().into();
        assert_eq!(
            (exit_code(&err), err.to_string()),
            (
                2,
                "cannot read stdin: stream did not contain valid UTF-8".to_owned()
            )
        );

        let err: miette::Report = read_stdin_from(&mut Unreadable).unwrap_err().into();
        assert_eq!(
            (exit_code(&err), err.to_string()),
            (
                2,
                format!("cannot read stdin: {}", std::io::ErrorKind::Other)
            ),
            "the error's kind, not the text it carries (#390)"
        );
        assert_eq!(read_stdin_from(&mut &b"Hello!\n"[..]).unwrap(), "Hello!\n");
    }

    /// A reader whose every read fails.
    struct Unreadable;

    impl Read for Unreadable {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("stream broke"))
        }
    }

    /// #428: the stale-map check reads only the bytes a sidecar mds wrote starts with —
    /// `SourceMap::to_json` writes `version`, then `file`, then `sources` — and deletes
    /// nothing else: a map written for the output (its name escaped as JSON, or not) is
    /// recognised, while one for another output, a hand-formatted one with the same
    /// fields, one that ends after `file`, and a large file of anything else are not,
    /// and the large one is never read whole.
    #[test]
    fn stale_map_check_reads_only_the_sidecar_head() {
        let sidecar = |name: &str| {
            let mut sm = mds::compile_str_with_deps_opts(
                "Hi\n",
                None,
                None,
                mds::CompileOptions::default().with_source_map(true),
            )
            .unwrap()
            .source_map
            .expect("a map was built");
            apply_source_map_file_label(&mut sm, Some(Path::new(name)), false);
            sm.to_json()
        };
        let verdict =
            |bytes: &str, name: &str| has_sidecar_head(&mut bytes.as_bytes(), name).unwrap();

        let mut mismatches = Vec::new();
        for name in ["out.md", "a \"quoted\" name.md", "caf\u{e9}.md"] {
            if !verdict(&sidecar(name), name) {
                mismatches.push(format!("{name}: its own sidecar is not recognised"));
            }
        }
        let rows = [
            ("another output's sidecar", sidecar("other.md")),
            (
                "hand-formatted, same fields",
                "{ \"version\": 3, \"file\": \"out.md\", \"sources\": [], \"names\": [], \"mappings\": \"\" }".to_owned(),
            ),
            ("ends after file", "{\"version\":3,\"file\":\"out.md\"}".to_owned()),
            ("version 2", "{\"version\":2,\"file\":\"out.md\",\"sources\":[]}".to_owned()),
        ];
        for (label, bytes) in rows {
            if verdict(&bytes, "out.md") {
                mismatches.push(format!("{label}: recognised as out.md's sidecar"));
            }
        }
        let head = "{\"version\":3,\"file\":\"out.md\",".len() as u64;
        let mut large = Stream::sized(64 * 1024 * 1024);
        if has_sidecar_head(&mut large, "out.md").unwrap() || large.served > head {
            mismatches.push(format!(
                "64 MiB of x: read {} bytes; the head is {head}",
                large.served
            ));
        }
        assert!(mismatches.is_empty(), "{mismatches:#?}");
    }

    /// #157: a stale sidecar that cannot be removed is an `mds::io` error, not a
    /// warning; the control removes the same sidecar from a writable directory.
    ///
    /// `#[cfg(unix)]`: the unlink failure comes from a `0o555`-mode directory, which
    /// Windows' read-only attribute does not reproduce.
    #[cfg(unix)]
    #[test]
    fn a_stale_map_that_cannot_be_removed_is_an_io_error() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let map = dir.path().join("out.md.map");
        let sidecar = "{\"version\":3,\"file\":\"out.md\",\"sources\":[],\"mappings\":\"\"}";

        // Control: a writable directory, and the sidecar is removed.
        std::fs::write(&map, sidecar).unwrap();
        assert!(
            verify_then_delete_map(&WriteTarget::as_typed(map.clone()), "out.md", true).is_ok()
        );
        assert!(!map.exists(), "control: the stale sidecar must be removed");

        std::fs::write(&map, sidecar).unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        let probe = std::fs::write(dir.path().join("probe"), b"");
        let result = verify_then_delete_map(&WriteTarget::as_typed(map.clone()), "out.md", true);
        let _ = std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755));
        if probe.is_ok() {
            crate::output::ewriteln!(
                "skipped: a file can be created at mode 0o555 (running as root?)"
            );
            return;
        }
        match result {
            Err(MdsError::Io { message }) => assert!(
                message.starts_with("could not remove stale map "),
                "unexpected message: {message}"
            ),
            other => panic!("want Err(MdsError::Io {{ .. }}); got {other:?}"),
        }
        assert!(
            map.exists(),
            "the sidecar that could not be removed is still there"
        );
    }

    /// #157: a stale `.map` that cannot be read is an `mds::io` error, not a "not a
    /// tool-generated map" warning — nothing is known about its content. The control: a
    /// readable map that is not a sidecar is left in place without an error.
    ///
    /// `#[cfg(unix)]`: the read failure comes from a `0o000`-mode file.
    #[cfg(unix)]
    #[test]
    fn a_stale_map_that_cannot_be_read_is_an_io_error() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let map = dir.path().join("out.md.map");

        // Control: readable, not a sidecar — left in place, no error.
        std::fs::write(&map, "{\"hand\":\"written\"}").unwrap();
        assert!(
            verify_then_delete_map(&WriteTarget::as_typed(map.clone()), "out.md", true).is_ok()
        );
        assert!(map.exists(), "control: a hand-written map is left in place");

        let sidecar = "{\"version\":3,\"file\":\"out.md\",\"sources\":[],\"mappings\":\"\"}";
        std::fs::write(&map, sidecar).unwrap();
        std::fs::set_permissions(&map, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&map).is_ok() {
            crate::output::ewriteln!(
                "skipped: a file can be read at mode 0o000 (running as root?)"
            );
            return;
        }
        let result = verify_then_delete_map(&WriteTarget::as_typed(map.clone()), "out.md", true);
        match result {
            Err(MdsError::Io { message }) => assert!(
                message.starts_with("cannot read stale map ") && message.contains("out.md.map"),
                "unexpected message: {message}"
            ),
            other => panic!("want Err(MdsError::Io {{ .. }}); got {other:?}"),
        }
        assert!(map.exists(), "a map that cannot be read is left in place");
    }

    /// #160: a stale map whose directory below the anchor is a symlink — `mds.json`'s
    /// `build.output_dir` puts directories there — is not removed through it: the removal
    /// is refused (`mds::io`), naming the map and the link as shown, and the sidecar of
    /// that name where the link leads is left. Control: the same sidecar below a real
    /// directory is removed.
    #[cfg(unix)]
    #[test]
    fn a_stale_map_is_never_removed_through_a_symlink_below_its_anchor() {
        let sidecar = "{\"version\":3,\"file\":\"x.md\",\"sources\":[],\"mappings\":\"\"}";
        let dir = tempfile::tempdir().unwrap();
        let anchor = dir.path().join("out");
        std::fs::create_dir_all(anchor.join("real")).unwrap();
        std::fs::create_dir(dir.path().join("victim")).unwrap();
        std::os::unix::fs::symlink("../victim", anchor.join("sub")).unwrap();
        let victim = dir.path().join("victim/x.md.map");
        std::fs::write(&victim, sidecar).unwrap();

        let map = WriteTarget::below(&anchor, Path::new("out"), Path::new("sub/x.md.map"));
        let result = verify_then_delete_map(&map, "x.md", true);
        assert_eq!(
            std::fs::read_to_string(&victim).ok().as_deref(),
            Some(sidecar),
            "nothing is removed through the symlink"
        );
        match result {
            Err(MdsError::Io { message }) => assert_eq!(
                message,
                "could not remove stale map out/sub/x.md.map: \
                 refusing to follow a symlink at out/sub"
            ),
            other => panic!("want Err(MdsError::Io {{ .. }}); got {other:?}"),
        }

        // Control: below a real directory, the sidecar is removed.
        let real = anchor.join("real/x.md.map");
        std::fs::write(&real, sidecar).unwrap();
        let map = WriteTarget::below(&anchor, Path::new("out"), Path::new("real/x.md.map"));
        assert!(verify_then_delete_map(&map, "x.md", true).is_ok());
        assert!(!real.exists(), "control: the stale sidecar is removed");
    }
}
