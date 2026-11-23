//! Project configuration and compile input paths.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::write::Inputs;
use mds::{effective_parent, MAX_TRAVERSAL_DEPTH};
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
    /// config and optionally invoking a separate check. This closes a gap: a
    /// consumer could previously skip detection silently by not invoking the
    /// separate check.
    /// Note: `#[must_use]` on `from_rules_checked` fires only when the entire
    /// return value is dropped; `let (config, _) = …` silently discards the
    /// `Option<mds::UnknownRuleNames>` and is the caller's own choice (as the
    /// doc of `from_rules_checked` states: the `#[must_use]` only warns "if the
    /// return value is discarded entirely").
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

    /// `mds.json` itself, below its canonical directory: the file the run read, which no
    /// output is written over (#425).
    pub(crate) fn file(&self) -> PathBuf {
        self.dir.join("mds.json")
    }
}

/// The files the compile of `entry` read, which its output is never written over (#425):
/// the entry — none for stdin — the modules it imported, as the compiler reports them
/// (`dependencies`), and `reads`, the files the whole run reads: the `--vars` file and the
/// `mds.json` in force.
pub(crate) fn compile_inputs(
    entry: Option<&Path>,
    dependencies: &[String],
    reads: &[PathBuf],
) -> Inputs {
    Inputs::new(
        entry
            .map(Path::to_path_buf)
            .into_iter()
            .chain(dependencies.iter().map(PathBuf::from))
            .chain(reads.iter().cloned()),
    )
}

/// The files a whole run reads besides each compile's own, for [`compile_inputs`]: the
/// `--vars` file and the `mds.json` in force (#425).
pub(crate) fn run_reads(vars: Option<&Path>, config: Option<&ProjectConfig>) -> Vec<PathBuf> {
    vars.map(Path::to_path_buf)
        .into_iter()
        .chain(config.map(ProjectConfig::file))
        .collect()
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
    let Some((current, shown_dir)) = config_dir(start) else {
        return Ok(None);
    };
    let candidate = current.join("mds.json");
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
    let bytes = mds::read_at_most(&mut file, MAX_CONFIG_SIZE + 1, size).map_err(cannot_read)?;
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
    Ok(Some(ProjectConfig {
        config,
        dir: current,
        shown_dir,
    }))
}

/// The directory [`load_config`] finds `mds.json` in for `start`, without reading it:
/// walking up from `start` — its parent when it is no directory — made canonical first, at
/// most [`MAX_TRAVERSAL_DEPTH`] directories, the first that holds a file named `mds.json`.
/// Returned canonical, and as `start` leads to it (`.`, one `..` per step up); `None` when
/// no directory on the way holds one.
fn config_dir(start: &Path) -> Option<(PathBuf, PathBuf)> {
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
        if current.join("mds.json").is_file() {
            return Some((current, shown_dir));
        }
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => break,
        }
        shown_dir.push("..");
    }
    None
}

/// The inputs of `source`, a directory run's, which its output is never written over
/// (#425): [`compile_inputs`]'s — `source`, the modules its compile imported
/// (`dependencies`) and `reads`, the files the whole run reads — and the `mds.json` nearest
/// `source`, the one `mds build <source>` or `mds watch <source>` would hold in force,
/// found as [`load_config`] finds it. A directory run reads one `mds.json`, from its
/// directory argument upward, and never reads one nearer a source; but an output never
/// replaces it either, as it would not in file mode. That one is searched for only by a
/// write that meets a file at its output, and then at that moment
/// ([`Inputs::and_found_from`]): where nothing is, nothing is replaced.
pub(crate) fn source_inputs(source: &Path, dependencies: &[String], reads: &[PathBuf]) -> Inputs {
    compile_inputs(Some(source), dependencies, reads).and_found_from(source, nearest_config)
}

/// The `mds.json` nearest `source`, found as [`load_config`] finds it, if any.
fn nearest_config(source: &Path) -> Option<PathBuf> {
    config_dir(source).map(|(dir, _)| dir.join("mds.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
