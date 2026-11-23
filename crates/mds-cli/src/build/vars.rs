//! Runtime variables from CLI flags and vars files.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use mds::MdsError;
use miette::Result;

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
///
/// A file that is a symlink is refused through [`vars_file_error`], as `mds watch`
/// refuses it at startup (#157). With a `log`, the file's byte count is recorded in it
/// ([`mds::load_vars_file_logged`]).
fn load_optional_vars_file(
    path: Option<PathBuf>,
    log: Option<&mds::ReadLog>,
) -> Result<Option<mds::VarsLoad>> {
    path.map(|p| {
        match log {
            Some(log) => mds::load_vars_file_logged(&p, log),
            None => mds::load_vars_file_reporting_duplicates(&p),
        }
        .map_err(|e| miette::Error::from(vars_file_error(&p, e)))
    })
    .transpose()
}

/// The error a `--vars` file at `typed`, the path as typed, is refused with (#157).
///
/// The symlink check's refusal (`ImportError`, the only one a vars file's load or its
/// symlink check gives) is an I/O failure on the command line, the same on every
/// command: `mds::io`, exit 2, `--vars file must not be a symlink: "<path>"`, the path
/// escaped and quoted, as the directory argument's refusal quotes it. Every other error — a
/// forbidden character in the resolved path, a file missing, unreadable, too large or not
/// a JSON object — keeps its own message and code.
pub(crate) fn vars_file_error(typed: &Path, e: MdsError) -> MdsError {
    match e {
        MdsError::ImportError { .. } => MdsError::Io {
            message: format!(
                "--vars file must not be a symlink: \"{}\"",
                mds::escape_path_for_message(&typed.to_string_lossy())
            ),
        },
        other => other,
    }
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
    build_runtime_vars_into(args, None)
}

/// [`build_runtime_vars`], and record the `--vars` file's byte count in `log`
/// ([`mds::load_vars_file_logged`]): `mds watch` learns from it whether the load read the
/// file empty (#380).
pub(crate) fn build_runtime_vars_logged(
    args: RuntimeVarArgs,
    log: &mds::ReadLog,
) -> Result<RuntimeVars> {
    build_runtime_vars_into(args, Some(log))
}

fn build_runtime_vars_into(
    args: RuntimeVarArgs,
    log: Option<&mds::ReadLog>,
) -> Result<RuntimeVars> {
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

    // Clone the path BEFORE load_optional_vars_file(vars, log) moves it (#326).
    let vars_file = vars.clone();
    let loaded = load_optional_vars_file(vars, log)?;
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
/// Every untrusted value interpolated into `eprint_warning` must be wrapped in
/// `safe_inline(…)` / `safe_path(…)` **at the interpolation site** — not hoisted into a
/// `let` binding first — so the print-discipline guard can see it. `key` is raw,
/// untrusted text straight from the JSON; `Path` is not `Display`, so it goes through
/// `safe_path`, not `safe_inline`.
///
/// No-op when `quiet` is true.
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
/// for any duplicate keys found by [`build_runtime_vars`], plus the `--vars` file
/// duplicate-key warnings (#326), in that order: the file's, then `--set`'s, then
/// `--set-string`'s.
///
/// Every untrusted value interpolated into `eprint_warning` must be wrapped in
/// `safe_inline(…)` **at the interpolation site** — not hoisted into a `let` binding
/// first.  The call is repeated rather than bound so the print-discipline guard can see
/// it, as in [`emit_duplicate_vars_file_warnings`].
///
/// No-op when `quiet` is true.
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
