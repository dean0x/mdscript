//! Windows verbatim-path simplification for user-visible absolute paths.
//!
//! On Windows `std::fs::canonicalize` returns verbatim paths (`\\?\C:\…`,
//! `\\?\UNC\server\share\…`). They stay the internal form of a native module key
//! — containment compares canonical paths with each other — but a path handed
//! back to the caller should be the conventional one (`C:\…`), the form every
//! other tool reports and compares against (#409).
//!
//! The rewrite is made only when it is lossless: the verbatim form is taken
//! literally by Windows, while the conventional form is parsed (length-limited,
//! `.`/`..` collapsed, trailing dots and spaces trimmed, device names such as
//! `CON` redirected). A path any of that would change keeps its prefix.
//!
//! This is string logic over `\`-separated Windows paths, so it is tested the same
//! way on every host. It is compiled into Windows builds only: a canonical path on
//! any other platform never starts with `\\?\`, so there is nothing to rewrite.

/// The verbatim prefix.
const VERBATIM: &str = r"\\?\";

/// The longest path, in UTF-16 code units, that the conventional form can name:
/// `MAX_PATH` (260) counts the terminating NUL.
const MAX_CONVENTIONAL_PATH: usize = 259;

/// The longest file-name component, in UTF-16 code units.
const MAX_COMPONENT: usize = 255;

/// Rewrite a verbatim disk or UNC path to its conventional form when the rewrite
/// names exactly the same file.
///
/// `\\?\C:\x` becomes `C:\x`, and `\\?\UNC\server\share\x` becomes
/// `\\server\share\x`. Returns `None` — keep the path as it is — when `path` is
/// not a verbatim disk or UNC path (already conventional, another verbatim form
/// such as `\\?\Volume{…}`, or not a Windows path at all), or when the
/// conventional form would be longer than `MAX_PATH` or would contain a
/// component that it cannot express literally (empty, `.` or `..`, a character
/// Windows forbids in a name, a trailing dot or space, a reserved device name).
pub(crate) fn simplify_verbatim(path: &str) -> Option<String> {
    let rest = path.strip_prefix(VERBATIM)?;
    let simplified = match rest.strip_prefix(r"UNC\") {
        Some(unc) => {
            let mut parts = unc.splitn(3, '\\');
            let server = parts.next()?;
            let share = parts.next()?;
            let lossless = is_plain_component(server)
                && is_plain_component(share)
                && parts.next().is_none_or(is_plain_tail);
            lossless.then(|| format!(r"\\{unc}"))?
        }
        None => {
            let bytes = rest.as_bytes();
            let is_drive = bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && bytes[2] == b'\\';
            (is_drive && is_plain_tail(&rest[3..])).then(|| rest.to_owned())?
        }
    };
    (simplified.encode_utf16().count() <= MAX_CONVENTIONAL_PATH).then_some(simplified)
}

/// Whether the part of a path after its root — `a\b\c`, or empty for the root
/// itself — consists of components the conventional form keeps literally.
fn is_plain_tail(tail: &str) -> bool {
    tail.is_empty() || tail.split('\\').all(is_plain_component)
}

/// Whether the conventional form keeps `component` exactly as written.
fn is_plain_component(component: &str) -> bool {
    !component.is_empty()
        && component != "."
        && component != ".."
        && component.encode_utf16().count() <= MAX_COMPONENT
        && !component.ends_with(['.', ' '])
        && !component
            .chars()
            .any(|c| c < ' ' || matches!(c, '<' | '>' | ':' | '"' | '/' | '|' | '?' | '*'))
        && !is_device_name(component)
}

/// Whether `component` names a DOS device, which the conventional form redirects
/// to the device instead of a file: the part before the first `.`, with trailing
/// spaces ignored, is a device name in any case (`con`, `CON.txt`, `Com1 .log`).
fn is_device_name(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .trim_end_matches(' ');
    let fixed = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];
    if fixed.iter().any(|d| stem.eq_ignore_ascii_case(d)) {
        return true;
    }
    // COM and LPT with one digit, including the superscript digits Windows
    // also reserves.
    let mut chars = stem.chars();
    let head: String = chars.by_ref().take(3).collect();
    let digit = chars.next();
    (head.eq_ignore_ascii_case("COM") || head.eq_ignore_ascii_case("LPT"))
        && matches!(digit, Some('0'..='9' | '\u{B9}' | '\u{B2}' | '\u{B3}'))
        && chars.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn simplify(path: &str) -> Option<String> {
        simplify_verbatim(path)
    }

    #[test]
    fn verbatim_drive_path_becomes_conventional() {
        assert_eq!(
            simplify(r"\\?\C:\Users\me\proj\lib.mds").as_deref(),
            Some(r"C:\Users\me\proj\lib.mds")
        );
        assert_eq!(simplify(r"\\?\d:\x.mds").as_deref(), Some(r"d:\x.mds"));
    }

    #[test]
    fn verbatim_drive_root_becomes_conventional() {
        assert_eq!(simplify(r"\\?\C:\").as_deref(), Some(r"C:\"));
    }

    #[test]
    fn verbatim_unc_path_becomes_conventional() {
        assert_eq!(
            simplify(r"\\?\UNC\server\share\dir\lib.mds").as_deref(),
            Some(r"\\server\share\dir\lib.mds")
        );
        assert_eq!(
            simplify(r"\\?\UNC\server\share").as_deref(),
            Some(r"\\server\share")
        );
        assert_eq!(
            simplify(r"\\?\UNC\server\share\").as_deref(),
            Some(r"\\server\share\")
        );
    }

    #[test]
    fn non_ascii_names_and_spaces_inside_names_are_kept() {
        let conventional = format!(r"C:\Users\j{}rgen\a b.mds", '\u{fc}');
        assert_eq!(
            simplify(&format!(r"\\?\{conventional}")),
            Some(conventional)
        );
    }

    #[test]
    fn a_path_that_is_not_verbatim_is_left_alone() {
        // Control: the verbatim twin of the first case IS rewritten.
        assert_eq!(
            simplify(r"\\?\C:\x\lib.mds").as_deref(),
            Some(r"C:\x\lib.mds")
        );
        for path in [
            r"C:\x\lib.mds",
            r"\\server\share\x.mds",
            "/home/u/lib.mds",
            "lib.mds",
            r"\\.\C:\x.mds",
            "",
        ] {
            assert_eq!(simplify(path), None, "{path:?}");
        }
    }

    #[test]
    fn other_verbatim_forms_keep_their_prefix() {
        // Controls: the disk and UNC forms next to each case below are rewritten.
        assert!(simplify(r"\\?\C:\x.mds").is_some());
        assert!(simplify(r"\\?\UNC\server\share\x.mds").is_some());
        for path in [
            r"\\?\Volume{0b8a4ee1-0000-0000-0000-100000000000}\x.mds",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\x.mds",
            // Drive-relative once the prefix is gone: `C:x` means "x in C:'s cwd".
            r"\\?\C:",
            r"\\?\C:x.mds",
            r"\\?\UNC\server",
            r"\\?\UNC\\share\x.mds",
            r"\\?\unc\server\share\x.mds",
        ] {
            assert_eq!(simplify(path), None, "{path:?}");
        }
    }

    /// `C:\`, 200 × `first`, `\`, then `a`s: exactly `len` UTF-16 units long
    /// (`first` must be a single UTF-16 unit).
    fn drive_path(first: char, len: usize) -> String {
        let head: String = std::iter::repeat_n(first, 200).collect();
        format!(r"C:\{head}\{}", "a".repeat(len - 204))
    }

    #[test]
    fn a_path_longer_than_max_path_keeps_its_prefix() {
        let at_limit = drive_path('a', MAX_CONVENTIONAL_PATH);
        assert_eq!(at_limit.encode_utf16().count(), MAX_CONVENTIONAL_PATH);
        assert_eq!(
            simplify(&format!(r"\\?\{at_limit}")),
            Some(at_limit.clone())
        );
        let over = drive_path('a', MAX_CONVENTIONAL_PATH + 1);
        assert_eq!(simplify(&format!(r"\\?\{over}")), None);
    }

    #[test]
    fn max_path_counts_utf16_units_not_bytes() {
        // U+00FC is two UTF-8 bytes but one UTF-16 unit: at the limit in units
        // while far over it in bytes.
        let at_limit = drive_path('\u{fc}', MAX_CONVENTIONAL_PATH);
        assert!(at_limit.len() > MAX_CONVENTIONAL_PATH);
        assert_eq!(at_limit.encode_utf16().count(), MAX_CONVENTIONAL_PATH);
        assert_eq!(simplify(&format!(r"\\?\{at_limit}")), Some(at_limit));
    }

    #[test]
    fn a_component_longer_than_255_units_keeps_its_prefix() {
        let at_limit = "a".repeat(MAX_COMPONENT);
        assert!(simplify(&format!(r"\\?\C:\{at_limit}")).is_some());
        let over = "a".repeat(MAX_COMPONENT + 1);
        assert_eq!(simplify(&format!(r"\\?\C:\{over}")), None);
    }

    #[test]
    fn reserved_device_names_keep_their_prefix() {
        for name in [
            "CON",
            "con",
            "con.txt",
            "Aux.mds",
            "NUL.tar.gz",
            "prn ",
            "nul .mds",
            "COM1.mds",
            "lpt9",
            "COM0",
            "CONIN$",
            "conout$.log",
            "COM\u{B9}.mds",
            "LPT\u{B3}",
        ] {
            assert_eq!(
                simplify(&format!(r"\\?\C:\proj\{name}")),
                None,
                "{name:?} is a device name"
            );
        }
        // Controls: names that only start like a device are plain files.
        for name in [
            "CONSOLE.mds",
            "COM10.mds",
            "auxiliary.mds",
            "nullable",
            "lpt",
        ] {
            assert!(
                simplify(&format!(r"\\?\C:\proj\{name}")).is_some(),
                "{name:?} is a plain name"
            );
        }
    }

    #[test]
    fn names_the_conventional_form_rewrites_keep_their_prefix() {
        // Control: the same position with a plain name is rewritten.
        assert!(simplify(r"\\?\C:\a\b.mds").is_some());
        let control = format!("ctl{}.mds", char::from_u32(0x01).unwrap());
        for tail in [
            r"a\..\b.mds",
            r"a\.\b.mds",
            r"a\\b.mds",
            r"a\",
            "trailing-dot.",
            "trailing-space ",
            "stream.mds:alt",
            "q?.mds",
            "star*.mds",
            "quote\".mds",
            "pipe|.mds",
            "lt<.mds",
            "gt>.mds",
            "slash/.mds",
            control.as_str(),
        ] {
            assert_eq!(
                simplify(&format!(r"\\?\C:\{tail}")),
                None,
                "{tail:?} would not survive the conventional form"
            );
        }
        // The same rules apply to a UNC server and share name.
        assert_eq!(simplify(r"\\?\UNC\ser:ver\share\x.mds"), None);
        assert_eq!(simplify(r"\\?\UNC\server\CON\x.mds"), None);
    }
}
