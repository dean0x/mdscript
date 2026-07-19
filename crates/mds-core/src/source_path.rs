//! Source-path relativization for Source Map v3 `sources[]` entries.
//!
//! Provides a single choke-point function [`relativize_source`] that converts
//! an absolute or relative source path into a safe, map-relative or root-
//! relative form for embedding in a Source Map v3 document.
//!
//! # Security invariant (PF-005 / ADR-005)
//!
//! The guard is **unconditional and runtime-enforced** — no path that escapes
//! the project root, carries an absolute prefix, or embeds filesystem layout
//! information can survive into `sources[]`.  The invariant holds in release
//! builds (never `debug_assert!`).
//!
//! # Single choke-point (PF-004)
//!
//! **All** `sources[]` relativization MUST flow through this function.  There
//! is exactly one enforcement point so no alternate code path can silently
//! bypass the guard.

use std::path::Path;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Relativize a single `sources[]` entry for a Source Map v3 document.
///
/// # Parameters
///
/// - `source` — raw source key from [`crate::sourcemap::MapBuilder`]
///   (`sources[i]`), typically an absolute canonical path on native or a
///   virtual key string on WASM.
/// - `base` — directory that the source map file will be written to (the
///   "map directory").  When `None`, paths are relativized against `root`
///   instead (binding surfaces — napi / Python — that do not write a `.map`
///   file).
/// - `root` — established project root (from
///   [`crate::fs::FileSystem::source_root`]).  `None` for virtual / in-memory
///   filesystems ([`crate::fs::VirtualFs`] / WASM) where there is no
///   containment concept; separator unification plus the escape / absolute /
///   drive-qualified guards are applied in that case.  **This MUST preserve
///   today's WASM `sources[]` output byte-for-byte** (ADR-005) — it does,
///   because virtual keys are relative by construction, so no guard fires.
///
/// # Invariants (PF-005 / ADR-005)
///
/// - The returned string is **never** an absolute path.
/// - The returned string is **never** drive-qualified.
/// - For non-fallback, non-sentinel outputs with `root = Some(_)`:
///   `normalize(b.join(result))` equals `normalize(source)` and is contained
///   within `root` (round-trip re-check in step 9 of the guard algorithm).
///
/// # Sentinels
///
/// Strings delimited by `<` and `>` (e.g. `<stdin>`) pass through verbatim —
/// they are diagnostic labels, not filesystem paths.
///
/// # Guard algorithm (order matters — each step closes a specific hole)
///
/// 1. Sentinel: source starts `<` and ends `>` → return verbatim.
/// 2. Unify separators FIRST: `s = source.replace('\\', "/")` — closes the
///    backslash-on-Unix bypass (`..\..\` not caught as `../..` otherwise).
/// 3. Strip verbatim prefixes on unified string: `//?/UNC/` then `//?/`.
/// 4. Classify absolute: leading `/`, or drive-qualified (`C:\`, `C:/`, `C:`).
/// 5. Lexically normalize into components (resolve `.`, `..`) — closes
///    `./../../` and interior-`..` bypasses.
/// 6. If `root = None`: absolute / drive-qualified / lexical-escape checks all
///    degrade to basename; otherwise return the unified path.
/// 7. If not absolute: resolve against `base` (or `root`) before containment.
/// 8. Containment: component-wise descendant of `root`?  If not → basename.
/// 9. Emit relative to `b` (where `b = base` if `base` is inside `root`, else
///    `b = root`).  Round-trip re-check before returning; on failure → basename.
/// 10. Basename fallback: last non-`..` component of the NORMALIZED components
///     (never the raw string — see note in `basename_fallback`).  Safety
///     re-check; if that fails → `"source"`.
pub fn relativize_source(source: &str, base: Option<&Path>, root: Option<&Path>) -> String {
    // Step 1: sentinel — display labels are not filesystem paths.
    if source.starts_with('<') && source.ends_with('>') {
        return source.to_string();
    }

    // Step 2: unify separators FIRST — closes the backslash-on-Unix bypass.
    // `..\..\secret.mds` becomes `../../secret.mds` before any other check,
    // so the leading-`..` detector and apply_relative both see the real structure.
    let unified = source.replace('\\', "/");

    // Step 3: strip Windows verbatim-prefix variants (PF-003 / AC-SEC-01).
    let stripped = unified
        .strip_prefix("//?/UNC/")
        .or_else(|| unified.strip_prefix("//?/"))
        .unwrap_or(&unified);

    if stripped.is_empty() {
        return "source".to_string();
    }

    // Step 4: classify absolute.
    let is_abs = stripped.starts_with('/') || is_drive_qualified(stripped);

    // Step 5: lexically normalize into components, resolving `.` and `..`.
    // This closes the `./../../` (leading-dot-slash) and interior-dot-dot bypasses.
    let norm_comps: Vec<String> = if is_abs {
        normalize_abs(stripped)
    } else {
        normalize_rel(stripped)
    };

    // Step 6: root = None branch — VirtualFs / WASM.
    // No containment concept, so containment (step 8) and map-relative emission
    // (step 9) do not apply; only separator unification and the escape/absolute
    // guards run.  Preserves today's WASM `sources[]` output byte-for-byte for
    // every legitimate virtual key (ADR-005) — virtual keys are relative by
    // construction (`buildModulesMap` emits project-root-relative slash paths),
    // so neither guard below can fire on the shipped WASM path.
    let Some(root) = root else {
        // An absolute or drive-qualified key still encodes filesystem layout
        // even though there is no root to contain it against.  Degrade to the
        // basename so the "never absolute / never drive-qualified" invariant
        // documented above holds on THIS branch too (PF-005: a guarantee that
        // is real only where a root happens to be established is not a
        // guarantee — `FileSystem::source_root` is a defaulted method returning
        // `None`, so any impl that does not override it lands here).
        if is_abs {
            return basename_fallback(&norm_comps);
        }
        // A relative path whose first component is `..` escapes the virtual root.
        if norm_comps.first().map(String::as_str) == Some("..") {
            return basename_fallback(&norm_comps);
        }
        let joined = norm_comps.join("/");
        return if joined.is_empty() {
            "source".to_string()
        } else {
            joined
        };
    };

    // Normalize root components (absolute component list).
    let root_unified = path_to_unified(root);
    let root_comps = normalize_abs(&root_unified);

    // Step 7: if not absolute, resolve against base (or root) before containment.
    let abs_comps: Vec<String> = if is_abs {
        norm_comps.clone()
    } else {
        let anchor = match base {
            Some(b) => normalize_abs(&path_to_unified(b)),
            None => root_comps.clone(),
        };
        match apply_relative(anchor, &norm_comps) {
            Some(comps) => comps,
            // Path escapes above anchor root → basename fallback.
            None => return basename_fallback(&norm_comps),
        }
    };

    // Step 8: containment — source must be a component-wise descendant of root.
    // (Case-folded on Windows via `starts_with_comps`.)
    if !starts_with_comps(&abs_comps, &root_comps) {
        return basename_fallback(&abs_comps);
    }

    // Step 9: emit.
    //
    // `b = base` if `base` is also inside root, else `b = root`.
    // This handles the "source under root but output outside it" case: emit
    // root-relative rather than degrading to a basename.  It also makes CLI
    // and binding surfaces run the **same algorithm** — bindings are permanently
    // in the base-absent case and always receive root-relative paths.
    let b_comps = match base {
        Some(b) => {
            let bc = normalize_abs(&path_to_unified(b));
            if starts_with_comps(&bc, &root_comps) {
                bc
            } else {
                root_comps.clone()
            }
        }
        None => root_comps.clone(),
    };

    let result = component_diff(&b_comps, &abs_comps);

    // Round-trip re-check (step 9 guard, PF-005):
    //   normalize(b.join(result)) == normalize(source)
    //   AND result is not absolute
    //   AND result is not drive-qualified
    //
    // `apply_relative` receives `b_comps` (consumed here, no longer needed).
    let result_parts: Vec<String> = result.split('/').map(str::to_string).collect();
    let round_trip = apply_relative(b_comps, &result_parts);
    if round_trip.as_deref() != Some(abs_comps.as_slice())
        || result.starts_with('/')
        || is_drive_qualified(&result)
    {
        return basename_fallback(&abs_comps);
    }

    result
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// True if `s` looks like a Windows drive-qualified path in any of the three
/// forms that can survive separator unification.
///
/// Ported verbatim from `crates/mds-cli/src/build.rs` (SEC-2), including all
/// three disjuncts:
/// - `:\\` — classic backslash-qualified drive path.
/// - `:/` — forward-slash-normalised drive path (SEC-2 gap).
/// - leading `<letter>:` — bare drive designator without a separator.
fn is_drive_qualified(s: &str) -> bool {
    s.contains(":\\")
        || s.contains(":/")
        || (s.len() >= 2
            && (s.as_bytes()[0] as char).is_ascii_alphabetic()
            && s.as_bytes()[1] == b':')
}

/// Convert a `Path` to a `/`-unified string.
fn path_to_unified(p: &Path) -> String {
    p.to_str().unwrap_or("").replace('\\', "/")
}

/// Normalize an **absolute** path string into a component list.
///
/// Strips the leading `/` (Unix absolute) or keeps the drive prefix as the
/// first component (Windows absolute).  Resolves `.` (skip) and `..` (pop;
/// no-op when already at root).
fn normalize_abs(s: &str) -> Vec<String> {
    let rest = s.strip_prefix('/').unwrap_or(s);
    let mut comps: Vec<String> = Vec::new();
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                comps.pop(); // no-op when empty (already at root)
            }
            p => comps.push(p.to_string()),
        }
    }
    comps
}

/// Normalize a **relative** path string into a component list.
///
/// Preserves leading `..` segments — they are meaningful relative to an
/// anchor directory and are consumed by [`apply_relative`].  Resolves
/// interior `..` against preceding non-`..` segments where possible.
fn normalize_rel(s: &str) -> Vec<String> {
    let mut comps: Vec<String> = Vec::new();
    for part in s.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                // If the stack is empty or already ends in `..`, preserve escape.
                if comps.last().map(String::as_str) == Some("..") || comps.is_empty() {
                    comps.push("..".to_string());
                } else {
                    comps.pop();
                }
            }
            p => comps.push(p.to_string()),
        }
    }
    comps
}

/// Apply normalized relative components (`rel_parts`) on top of an anchor
/// (an already-normalized absolute component list).
///
/// Returns `None` if the relative path escapes above the anchor root
/// (i.e. has more `..` segments than the anchor has components).  The caller
/// treats `None` as a "path escapes" condition and falls back to the basename.
fn apply_relative(mut anchor: Vec<String>, rel_parts: &[String]) -> Option<Vec<String>> {
    for c in rel_parts {
        match c.as_str() {
            "" | "." => {} // Skip empty segments and current-directory markers.
            ".." => {
                if anchor.is_empty() {
                    return None; // Escapes above root.
                }
                anchor.pop();
            }
            p => anchor.push(p.to_string()),
        }
    }
    Some(anchor)
}

/// True when `path` component-wise starts with `prefix`.
///
/// On Windows, component comparison is ASCII-case-insensitive.
fn starts_with_comps(path: &[String], prefix: &[String]) -> bool {
    if path.len() < prefix.len() {
        return false;
    }
    #[cfg(windows)]
    return path[..prefix.len()]
        .iter()
        .zip(prefix.iter())
        .all(|(a, b)| a.eq_ignore_ascii_case(b));
    #[cfg(not(windows))]
    path[..prefix.len()]
        .iter()
        .zip(prefix.iter())
        .all(|(a, b)| a == b)
}

/// Compute the `/`-separated relative path from `from` to `to`.
///
/// Both inputs are already-normalized absolute component lists sharing the
/// same root (the caller has verified containment via [`starts_with_comps`]).
///
/// Ported from `crates/mds-cli/src/build.rs::relative_path` but the
/// absolute-path failure fallback is replaced with the basename fallback to
/// close that leak path (build.rs:978 bug class).
fn component_diff(from: &[String], to: &[String]) -> String {
    #[cfg(windows)]
    let common = from
        .iter()
        .zip(to.iter())
        .take_while(|(a, b)| a.eq_ignore_ascii_case(b))
        .count();
    #[cfg(not(windows))]
    let common = from
        .iter()
        .zip(to.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let ups = from.len() - common;
    let downs = &to[common..];
    let mut parts: Vec<&str> = Vec::with_capacity(ups + downs.len());
    parts.extend(std::iter::repeat_n("..", ups));
    for c in downs {
        parts.push(c.as_str());
    }
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

/// Basename fallback (algorithm step 10, PF-005).
///
/// Returns the last non-`..` component of `comps`, or `"source"` if the
/// component fails the safety re-check (empty, contains `/`, is `.` / `..`,
/// or is drive-qualified).
///
/// **NEVER derived from the raw source string** — using the raw string's
/// `file_name()` is the exact bug class that `build.rs:978` had: on Unix,
/// `..\..\Users\alice\secret.mds` has no `/`-based `file_name()` component,
/// so `Path::new(raw).file_name()` returns the whole string verbatim, leaking
/// the path instead of extracting just `"secret.mds"`.  Using the NORMALIZED
/// component list avoids this.
fn basename_fallback(comps: &[String]) -> String {
    let last = comps
        .iter()
        .rev()
        .find(|s| s.as_str() != "..")
        .map(String::as_str)
        .unwrap_or("source");
    // Safety re-check: non-empty, no embedded '/', not "." or "..", not drive-qualified.
    if last.is_empty()
        || last.contains('/')
        || last == "."
        || last == ".."
        || is_drive_qualified(last)
    {
        "source".to_string()
    } else {
        last.to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(s: &'static str) -> &'static Path {
        Path::new(s)
    }

    /// Assert that `out` satisfies the basic output invariants for non-sentinel results.
    fn check_output_invariants(out: &str, source: &str) {
        let is_sentinel = out.starts_with('<') && out.ends_with('>');
        if !is_sentinel {
            assert!(
                !out.starts_with('/'),
                "output must not be absolute (source={source:?}): got {out:?}",
            );
            assert!(
                !is_drive_qualified(out),
                "output must not be drive-qualified (source={source:?}): got {out:?}",
            );
        }
    }

    // ── Three security bypasses — must be RED before implementation is complete ──

    /// Backslash-on-Unix bypass: `..\..\` looks like a filename on Unix without
    /// prior separator unification, leaking the raw string through the old guard.
    #[test]
    fn bypass_backslash_on_unix() {
        let out = relativize_source(
            r"..\..\Users\alice\secret.mds",
            Some(p("/proj")),
            Some(p("/proj")),
        );
        assert_eq!(
            out, "secret.mds",
            "backslash-on-Unix must degrade to basename"
        );
        check_output_invariants(&out, r"..\..\Users\alice\secret.mds");
    }

    /// Leading dot-slash bypass: `./../../` resolves to an upward escape.
    #[test]
    fn bypass_leading_dot_slash() {
        let out = relativize_source("./../../etc/passwd", Some(p("/proj")), Some(p("/proj")));
        assert_eq!(
            out, "passwd",
            "leading-dot-slash escape must degrade to basename"
        );
        check_output_invariants(&out, "./../../etc/passwd");
    }

    /// Interior dot-dot bypass: `/proj/a/../../etc/passwd` normalizes to `/etc/passwd`
    /// which is outside root `/proj`.
    #[test]
    fn bypass_interior_dot_dot() {
        let out = relativize_source(
            "/proj/a/../../etc/passwd",
            Some(p("/proj/build")),
            Some(p("/proj")),
        );
        assert_eq!(
            out, "passwd",
            "interior-dot-dot escape must degrade to basename"
        );
        check_output_invariants(&out, "/proj/a/../../etc/passwd");
    }

    // ── Core rule pair — both rows MUST hold simultaneously ──

    /// Source in /proj/src, map in /proj/build, root /proj → map-relative `../src/a.mds`.
    #[test]
    fn core_rule_map_relative() {
        let out = relativize_source("/proj/src/a.mds", Some(p("/proj/build")), Some(p("/proj")));
        assert_eq!(out, "../src/a.mds");
        check_output_invariants(&out, "/proj/src/a.mds");
    }

    /// Source outside root → basename fallback.
    /// (root = /proj/build, source = /proj/src/a.mds — source not under root)
    #[test]
    fn core_rule_source_outside_root() {
        let out = relativize_source(
            "/proj/src/a.mds",
            Some(p("/proj/build")),
            Some(p("/proj/build")),
        );
        assert_eq!(out, "a.mds");
        check_output_invariants(&out, "/proj/src/a.mds");
    }

    // ── Additional matrix rows ──

    #[test]
    fn sentinel_angle_bracket_passes_through_verbatim() {
        let out = relativize_source("<stdin>", Some(p("/proj")), Some(p("/proj")));
        assert_eq!(out, "<stdin>");
    }

    #[test]
    fn sentinel_source_label_no_root() {
        let out = relativize_source("<source>", None, None);
        assert_eq!(out, "<source>");
    }

    /// Binding (napi / Python) surfaces pass `base = None` → root-relative output.
    #[test]
    fn binding_no_base_emits_root_relative() {
        let out = relativize_source("/proj/src/a.mds", None, Some(p("/proj")));
        assert_eq!(out, "src/a.mds");
        check_output_invariants(&out, "/proj/src/a.mds");
    }

    /// Base directory is outside root → fall back to root as anchor (not basename).
    #[test]
    fn base_outside_root_uses_root_as_anchor() {
        let out = relativize_source("/proj/src/a.mds", Some(p("/other")), Some(p("/proj")));
        assert_eq!(out, "src/a.mds");
        check_output_invariants(&out, "/proj/src/a.mds");
    }

    /// Windows `\\?\` verbatim prefix is stripped before all other processing.
    #[test]
    fn verbatim_prefix_stripped() {
        // After unification: `//?/C:/secret/foo.mds` → strip `//?/` → `C:/secret/foo.mds`
        // Drive-qualified → abs_comps = ["C:", "secret", "foo.mds"]
        // Root = /proj → comps ["proj"] → containment fails → basename.
        let out = relativize_source(r"\\?\C:\secret\foo.mds", Some(p("/proj")), Some(p("/proj")));
        assert_eq!(out, "foo.mds");
        check_output_invariants(&out, r"\\?\C:\secret\foo.mds");
    }

    /// Forward-slash-normalised Windows drive path must degrade to basename (SEC-2).
    #[test]
    fn forward_slash_drive_path_degrades_to_filename() {
        let out = relativize_source("C:/secret/foo.mds", Some(p("/proj")), Some(p("/proj")));
        assert_eq!(out, "foo.mds");
        check_output_invariants(&out, "C:/secret/foo.mds");
    }

    /// Cross-drive path: source on D:, root on C: → containment fails → basename.
    #[test]
    fn cross_drive_degrades_to_filename() {
        let out = relativize_source("D:/other/file.mds", Some(p("C:/proj")), Some(p("C:/proj")));
        assert_eq!(out, "file.mds");
        check_output_invariants(&out, "D:/other/file.mds");
    }

    /// VirtualFs / WASM: `root = None` → pass-through with separator unification.
    #[test]
    fn root_none_pass_through_virtual_path() {
        let out = relativize_source("src/templates/foo.mds", None, None);
        assert_eq!(out, "src/templates/foo.mds");
    }

    /// VirtualFs / WASM: relative path that lexically escapes → basename.
    #[test]
    fn root_none_escape_degrades_to_basename() {
        let out = relativize_source("../../escape/secret.mds", None, None);
        assert_eq!(out, "secret.mds");
        check_output_invariants(&out, "../../escape/secret.mds");
    }

    /// `root = None` must NOT be an escape hatch around the never-absolute
    /// invariant.  `FileSystem::source_root` is a *defaulted* trait method
    /// returning `None`, so an impl that forgets to override it lands on this
    /// branch — it must still refuse to emit filesystem layout (PF-005).
    #[test]
    fn root_none_absolute_source_degrades_to_basename() {
        let out = relativize_source("/Users/alice/proj/src/a.mds", None, None);
        assert_eq!(
            out, "a.mds",
            "an absolute source with no root must degrade to its basename, \
             never echo the directory chain"
        );
        check_output_invariants(&out, "/Users/alice/proj/src/a.mds");
    }

    /// Same as above for a drive-qualified key: without the guard the unified
    /// string `C:/secret/foo.mds` was returned verbatim, which is exactly the
    /// "never drive-qualified" invariant this module documents.
    #[test]
    fn root_none_drive_qualified_source_degrades_to_basename() {
        let out = relativize_source(r"C:\secret\foo.mds", None, None);
        assert_eq!(out, "foo.mds");
        check_output_invariants(&out, r"C:\secret\foo.mds");
    }

    /// The guards above must be inert for every *legitimate* virtual key.
    /// `buildModulesMap` emits project-root-relative slash paths, so WASM
    /// `sources[]` output stays byte-identical (ADR-005 byte-parity clause).
    #[test]
    fn root_none_relative_keys_are_unchanged_by_the_absolute_guard() {
        for key in [
            "a.mds",
            "src/a.mds",
            "src/templates/deep/nested.mds",
            "./src/a.mds",
        ] {
            let out = relativize_source(key, None, None);
            let expected = key.strip_prefix("./").unwrap_or(key);
            assert_eq!(
                out, expected,
                "legitimate virtual key {key:?} must pass through unchanged"
            );
        }
    }

    /// Degenerate: source = "/" → empty components → containment fails → "source".
    #[test]
    fn degenerate_slash_only() {
        let out = relativize_source("/", Some(p("/proj")), Some(p("/proj")));
        assert_eq!(out, "source");
        check_output_invariants(&out, "/");
    }

    /// Degenerate: source = ".." → escapes anchor → basename of components → "source".
    #[test]
    fn degenerate_dot_dot() {
        let out = relativize_source("..", Some(p("/proj")), Some(p("/proj")));
        // norm_comps = [".."], apply_relative(["proj"], [".."]) = Some([])
        // starts_with_comps([], ["proj"]) → false → basename_fallback([])
        // comps.last() = None → "source"
        assert_eq!(out, "source");
        check_output_invariants(&out, "..");
    }

    /// Degenerate: empty source → "source".
    #[test]
    fn degenerate_empty_source() {
        let out = relativize_source("", Some(p("/proj")), Some(p("/proj")));
        assert_eq!(out, "source");
    }

    // ── Source inside root at the same level as the map directory ──

    #[test]
    fn source_in_same_dir_as_map() {
        // Source and map both in /proj/build → relative = "out.mds"
        let out = relativize_source(
            "/proj/build/out.mds",
            Some(p("/proj/build")),
            Some(p("/proj")),
        );
        assert_eq!(out, "out.mds");
        check_output_invariants(&out, "/proj/build/out.mds");
    }

    // ── Tests migrated from mds-cli/src/build.rs (AC-SEC-01) ──────────────────
    //
    // These tests were moved here when the CLI's `relativize_source_path` helper
    // was deleted and its behaviour subsumed by this single choke-point (PF-004).
    // Signatures are adapted to `relativize_source(source, base, root)`.

    /// Both source and map directory share a common ancestor → clean map-relative path.
    /// This was the discriminating assertion that `a7ef84f` accidentally regressed.
    #[test]
    fn relativize_absolute_source_with_absolute_mapdir_is_clean_relative() {
        let got = relativize_source("/proj/src/a.mds", Some(p("/proj/build")), Some(p("/proj")));
        assert_eq!(
            got, "../src/a.mds",
            "same-root absolute paths must relativize to a clean map-relative path"
        );
        check_output_invariants(&got, "/proj/src/a.mds");
    }

    /// Source outside the project root with any base → basename fallback, never absolute.
    /// (Adapted from the `relative_mapdir` variant: the new guard catches it regardless
    /// of whether the old map_dir was relative or absolute.)
    #[test]
    fn relativize_absolute_source_with_relative_mapdir_never_leaks_absolute() {
        // Source `/tmp/deep/nested/abs_input.mds` is not under root `/proj` →
        // containment fails → basename fallback.
        let got = relativize_source(
            "/tmp/deep/nested/abs_input.mds",
            Some(p("/proj/build")),
            Some(p("/proj")),
        );
        assert!(!got.starts_with('/'), "must not be absolute: {got}");
        assert!(
            got.ends_with("abs_input.mds"),
            "must still resolve to the source filename: {got}"
        );
        check_output_invariants(&got, "/tmp/deep/nested/abs_input.mds");
    }

    /// Source outside root with no base (root-relative / binding mode) →
    /// basename fallback, never absolute.
    /// (Adapted from the `empty_mapdir` variant: `base = None` is the
    /// canonical "no explicit map directory" encoding.)
    #[test]
    fn relativize_absolute_source_with_empty_mapdir_never_leaks_absolute() {
        let got = relativize_source("/tmp/deep/abs_input.mds", None, Some(p("/proj")));
        assert!(!got.starts_with('/'), "must not be absolute: {got}");
        assert!(
            got.ends_with("abs_input.mds"),
            "must still resolve to the source filename: {got}"
        );
        check_output_invariants(&got, "/tmp/deep/abs_input.mds");
    }

    /// Forward-slash-normalised Windows drive path must degrade to basename (SEC-2).
    /// `C:/secret/foo.mds` is not seen as absolute by `Path::is_absolute()` on
    /// Unix, so a separator-only check would pass it through unchanged.
    #[test]
    fn relativize_forward_slashed_drive_path_degrades_to_filename() {
        let got = relativize_source(
            "C:/secret/foo.mds",
            Some(p("/proj/build")),
            Some(p("/proj")),
        );
        assert_eq!(
            got, "foo.mds",
            "forward-slashed drive path must degrade to filename"
        );
        check_output_invariants(&got, "C:/secret/foo.mds");
    }

    /// A cross-drive path suffix (`../../D:/x.mds`) containing `:/` must be
    /// caught and degrade to the bare filename (SEC-2 — the `:/` gap).
    #[test]
    fn relativize_cross_drive_relative_path_degrades_to_filename() {
        let got = relativize_source("../../D:/x.mds", Some(p("/proj/build")), Some(p("/proj")));
        assert_eq!(got, "x.mds", "cross-drive :/ path must degrade to filename");
        check_output_invariants(&got, "../../D:/x.mds");
    }

    /// Lexically normalize `base/relative` without hitting the filesystem.
    ///
    /// Applies each `/`-delimited component of `relative` to `base`, resolving
    /// `..` by popping and ignoring `.` / empty components.  Used in the
    /// round-trip property assertion below.
    fn lexical_join(base: &Path, relative: &str) -> PathBuf {
        let mut result = base.to_path_buf();
        for comp in relative.split('/') {
            match comp {
                "" | "." => {}
                ".." => {
                    let _ = result.pop();
                }
                c => result.push(c),
            }
        }
        result
    }

    /// For every test case, the output must never be absolute and never
    /// drive-qualified, and for non-sentinel outputs the round-trip
    /// `normalize(effective_b.join(out))` must stay inside `root`.
    ///
    /// The round-trip invariant is enforced in production at
    /// `source_path.rs:191-197`; this matrix catches regressions there.
    #[test]
    fn property_outputs_never_absolute_or_drive_qualified() {
        struct Case {
            source: &'static str,
            base: Option<&'static str>,
            root: Option<&'static str>,
        }
        let cases: &[Case] = &[
            // Three security bypasses
            Case {
                source: r"..\..\Users\alice\secret.mds",
                base: Some("/proj"),
                root: Some("/proj"),
            },
            Case {
                source: "./../../etc/passwd",
                base: Some("/proj"),
                root: Some("/proj"),
            },
            Case {
                source: "/proj/a/../../etc/passwd",
                base: Some("/proj/build"),
                root: Some("/proj"),
            },
            // Core rule pair
            Case {
                source: "/proj/src/a.mds",
                base: Some("/proj/build"),
                root: Some("/proj"),
            },
            Case {
                source: "/proj/src/a.mds",
                base: Some("/proj/build"),
                root: Some("/proj/build"),
            },
            // Sentinels
            Case {
                source: "<stdin>",
                base: Some("/proj"),
                root: Some("/proj"),
            },
            Case {
                source: "<source>",
                base: None,
                root: None,
            },
            // Binding / base-outside-root
            Case {
                source: "/proj/src/a.mds",
                base: None,
                root: Some("/proj"),
            },
            Case {
                source: "/proj/src/a.mds",
                base: Some("/other"),
                root: Some("/proj"),
            },
            // Verbatim prefix and drive paths
            Case {
                source: r"\\?\C:\secret\foo.mds",
                base: Some("/proj"),
                root: Some("/proj"),
            },
            Case {
                source: "C:/secret/foo.mds",
                base: Some("/proj"),
                root: Some("/proj"),
            },
            Case {
                source: "D:/other/file.mds",
                base: Some("C:/proj"),
                root: Some("C:/proj"),
            },
            // VirtualFs pass-through and escape
            Case {
                source: "src/templates/foo.mds",
                base: None,
                root: None,
            },
            Case {
                source: "../../escape/secret.mds",
                base: None,
                root: None,
            },
            // Degenerate inputs
            Case {
                source: "/",
                base: Some("/proj"),
                root: Some("/proj"),
            },
            Case {
                source: "..",
                base: Some("/proj"),
                root: Some("/proj"),
            },
            Case {
                source: "",
                base: Some("/proj"),
                root: Some("/proj"),
            },
            // Same-dir source
            Case {
                source: "/proj/build/out.mds",
                base: Some("/proj/build"),
                root: Some("/proj"),
            },
        ];

        for c in cases {
            let base = c.base.map(Path::new);
            let root = c.root.map(Path::new);
            let out = relativize_source(c.source, base, root);

            let is_sentinel = out.starts_with('<') && out.ends_with('>');
            if !is_sentinel {
                assert!(
                    !out.starts_with('/'),
                    "output must not be absolute (source={:?}): got {out:?}",
                    c.source,
                );
                assert!(
                    !is_drive_qualified(&out),
                    "output must not be drive-qualified (source={:?}): got {out:?}",
                    c.source,
                );
                assert!(
                    !out.is_empty(),
                    "output must not be empty (source={:?})",
                    c.source,
                );

                // Round-trip: normalize(effective_b.join(out)) must be inside root.
                //
                // `effective_b` mirrors the production logic (source_path.rs:170-180):
                // use `base` when base is inside root, else fall back to root.
                // This catches a regression in the round-trip guard at lines 191-197.
                if let Some(r) = root {
                    let effective_b: &Path = match base {
                        Some(b) if b.starts_with(r) => b,
                        _ => r,
                    };
                    let round_trip = lexical_join(effective_b, &out);
                    assert!(
                        round_trip.starts_with(r),
                        "round-trip: normalize(effective_b.join(out)) must be inside root \
                         (source={:?} out={out:?} effective_b={effective_b:?} root={r:?}): \
                         got {round_trip:?}",
                        c.source,
                    );
                }
            }
        }
    }
}
