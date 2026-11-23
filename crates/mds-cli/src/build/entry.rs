//! Select an explicit entry or find one in the working directory.

use std::path::PathBuf;

use mds::MdsError;
use miette::Result;

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

    single_mds_file(subcommand, &entries)
}

/// The one `.mds` file among `entries`, those [`auto_detect_mds_file`] found in the working
/// directory, or why there is not one.
fn single_mds_file(subcommand: &str, entries: &[PathBuf]) -> Result<PathBuf> {
    match entries {
        [] => Err(miette::Report::new(AutoDetectError::none())),
        [single] => Ok(single.clone()),
        several => Err(several_mds_files(subcommand, several)),
    }
}

/// Why [`auto_detect_mds_file`] found no single `.mds` file to use: what went wrong, and,
/// apart from it, the way out (#309). A human report shows the way out as the
/// diagnostic's `help:` line, and `mds lint --format json` as the error document's `help`,
/// so neither carries it as a second line of the message.
#[derive(Debug)]
pub(crate) struct AutoDetectError {
    message: String,
    help: String,
}

impl AutoDetectError {
    /// The working directory holds no `.mds` file.
    fn none() -> Self {
        Self {
            message: "no .mds files found in current directory".to_owned(),
            help: "run 'mds init' to create a starter template".to_owned(),
        }
    }
}

impl std::fmt::Display for AutoDetectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AutoDetectError {}

impl miette::Diagnostic for AutoDetectError {
    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        Some(Box::new(&self.help))
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
    miette::Report::new(AutoDetectError {
        message: format!("multiple .mds files found: {}", names.join(", ")),
        help: format!(
            "specify which file, e.g. 'mds {subcommand} {}'",
            names.first().map(|s| s.as_str()).unwrap_or("<file>.mds"),
        ),
    })
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
        let error = several_mds_files("build", &entries);
        assert_eq!(
            error.to_string(),
            format!(
                "multiple .mds files found: b.mds, x{}u000ABuilt forged{}u001B.mds",
                '\\', '\\'
            ),
            "each name must be shown escaped"
        );

        let entries = [PathBuf::from("b.mds"), PathBuf::from("a.mds")];
        let error = several_mds_files("lint", &entries);
        assert_eq!(
            error.to_string(),
            "multiple .mds files found: a.mds, b.mds",
            "control: ordinary names keep their text and their order"
        );
        assert_eq!(
            error.help().map(|help| help.to_string()).as_deref(),
            Some("specify which file, e.g. 'mds lint a.mds'"),
            "the way out is the help, apart from the message"
        );
    }

    /// A working directory with no `.mds` file says so in the message and holds the way out
    /// in the help, never as a second line of the message (#309). Control: one file is the
    /// file.
    #[test]
    fn no_mds_file_keeps_its_hint_apart_from_its_message() {
        let error = single_mds_file("build", &[]).expect_err("no file to use");
        assert_eq!(
            (error.to_string(), error.help().map(|help| help.to_string())),
            (
                "no .mds files found in current directory".to_owned(),
                Some("run 'mds init' to create a starter template".to_owned())
            )
        );
        assert_eq!(
            single_mds_file("build", &[PathBuf::from("a.mds")]).unwrap(),
            PathBuf::from("a.mds"),
            "control: a single file is used"
        );
    }
}
