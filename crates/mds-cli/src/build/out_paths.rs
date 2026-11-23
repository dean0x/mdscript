//! Resolve output paths and check output flags.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

#[cfg(doc)]
use super::admit_output;
use super::config::ProjectConfig;
#[cfg(doc)]
use super::stdio::write_output;
use super::EntryPaths;
use crate::output::WriteTarget;
use mds::{effective_parent, CompiledOutput};
use miette::Result;

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
pub(super) fn warn_output_extension_mismatch(
    output: &Option<String>,
    kind: OutputKind,
    quiet: bool,
) {
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

#[cfg(test)]
mod tests {
    use super::super::config::{BuildConfig, MdsConfig};
    use super::*;

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
}
