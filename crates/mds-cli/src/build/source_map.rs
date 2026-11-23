//! Set source map paths and labels, embed maps, and write or check sidecars.

use std::io::Read;
use std::path::{Path, PathBuf};

use super::config::ProjectConfig;
#[cfg(doc)]
use super::out_paths::resolve_output_path_for_kind;
use super::stdio::write_output;
use crate::output::WriteTarget;
use crate::write::{write_compiled, Inputs, NotRemoved, Removal};
use mds::{effective_parent, MdsError, SourceMap, STRING_SOURCE_MAP_LABEL};
use miette::Result;

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

/// Embed a map if one exists. Otherwise, return the content unchanged.
#[must_use]
pub(super) fn embed_map_in_content(content: String, map: Option<&SourceMap>) -> String {
    match map {
        Some(sm) => embed_carrier(content, &sm.to_json()),
        None => content,
    }
}

/// Write a source map beside the compiled output.
pub(super) fn write_sidecar_map(
    out: &WriteTarget,
    sm: &SourceMap,
    inputs: &Inputs,
    quiet: bool,
) -> Result<(), MdsError> {
    let map = out.sibling(map_path_for);
    // Write the sidecar's `file`, `sources` and `sourcesContent` verbatim (spec §7.5).
    // Consumers need the exact paths for file references. Escape the status line only.
    write_compiled(&map, &sm.to_json(), inputs)?;
    if !quiet {
        crate::output::ewriteln!(
            "Source map written to {}",
            crate::output::safe_path(&map.shown)
        );
    }
    Ok(())
}

/// Resolve a sidecar request when the destination is stdout.
pub(super) fn write_stdout_ladder(
    map_present: bool,
    flag_source_map: bool,
    cfg: Option<&ProjectConfig>,
    content: String,
    inputs: &Inputs,
    quiet: bool,
) -> Result<()> {
    if map_present {
        if flag_source_map {
            return Err(miette::miette!(
                "--source-map (sidecar) requires an output file; use -o <file> or \
                 --out-dir, --inline to embed the map for stdout, or --no-source-map \
                 to skip it"
            ));
        }
        // Name the config as the input reached it, as a config error does (#390).
        if !quiet {
            let cfg_path = cfg
                .map(ProjectConfig::shown_file)
                .unwrap_or_else(|| PathBuf::from("mds.json"));
            crate::output::ewriteln!(
                "warning: source_map in {} has no effect when writing to \
                 stdout (sidecar requires -o <file> or --out-dir); use --inline to \
                 embed the map, or --no-source-map to silence this warning",
                crate::output::safe_path(&cfg_path)
            );
        }
    }
    write_output(None, &content, inputs, quiet, true)
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
pub(super) fn compute_source_map_base(
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
        Ok(Removal::Kept) => keep_stale_map(map, "not proven to be written by mds", quiet),
        Err(not_a_file @ NotRemoved::NotAFile) => keep_stale_map(map, not_a_file.cause(), quiet),
        Err(not_removed) => Err(crate::output::stale_removal_error(
            "stale map",
            &map.shown,
            &not_removed,
        )),
    }
}

/// The warning for a stale map left in place, `warning: kept stale map <path>: <why>`, in
/// the words of the stale outputs `mds build` keeps ([`crate::output::probe_and_remove_stale`]);
/// none under `quiet`. The path is `map.shown` (#390).
fn keep_stale_map(map: &WriteTarget, why: &str, quiet: bool) -> Result<(), MdsError> {
    if !quiet {
        crate::output::ewriteln!(
            "warning: kept stale map {}: {}",
            crate::output::safe_path(&map.shown),
            crate::output::safe_inline(why)
        );
    }
    Ok(())
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

/// The file name of `output`, the name its sidecar map records as `file` — lossy, as
/// [`has_sidecar_head`] compares it.
pub(super) fn file_name_of(output: &WriteTarget) -> String {
    output
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::super::stdio::tests::Stream;
    use super::*;

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
