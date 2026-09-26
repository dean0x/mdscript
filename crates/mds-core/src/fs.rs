//! Filesystem abstraction for module resolution.
//!
//! Provides the [`FileSystem`] trait and two implementations:
//! - [`NativeFs`] — OS filesystem with symlink rejection and traversal prevention
//! - [`VirtualFs`] — in-memory HashMap-backed filesystem for testing and WASM

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::error::MdsError;
use crate::limits::{MAX_FILE_SIZE, MAX_TRAVERSAL_DEPTH};

/// Maximum number of path segments allowed in a single import path.
///
/// Defense-in-depth against adversarial inputs that could create unbounded
/// allocations in the segment-accumulation loop. 256 segments is far more
/// than any realistic import path would contain.
const MAX_PATH_SEGMENTS: usize = 256;

/// Filesystem abstraction for module resolution.
///
/// Implementations provide entry and import path resolution, base-directory
/// anchoring, file reading, and file-type detection. Security properties
/// (symlink rejection, traversal prevention) are implementation-specific:
/// [`NativeFs`] enforces them for OS access, while [`VirtualFs`] relies on its
/// closed key-space.
///
/// # Security Contract
///
/// The resolver ([`crate::resolver::ModuleCache`]) checks every path a caller
/// supplies before any backend method sees it, so these refusals cover a custom
/// backend passed to [`crate::resolver::ModuleCache::with_fs`] as well as the
/// built-in ones:
///
/// - an entry path or virtual entry key that is empty, contains `\0`, or carries
///   a [`crate::is_forbidden_path_char`] codepoint is refused with `mds::io` before
///   [`FileSystem::resolve_entry`] is called;
/// - an import string that is not `./`/`../`-relative, contains `\0`, or carries
///   a forbidden codepoint is refused with `mds::import` before
///   [`FileSystem::normalize_in_dir`] is called;
/// - a base directory carrying a forbidden codepoint is refused with `mds::io`
///   before [`FileSystem::anchor_base_dir`] is called.
///
/// These are pinned for a custom backend by the `custom_backend_never_sees_a_*`
/// tests in `crates/mds-core/tests/forbidden_path_chars.rs` and
/// `custom_backend_entry_validation_runs_before_backend` in
/// `crates/mds-core/tests/api_surface.rs`.
///
/// A custom implementation MUST uphold the rest itself:
///
/// - **Forbidden path characters** (#265): every path the backend produces — a
///   key it rewrites, a symlink it follows, a canonical form it computes — MUST be
///   refused when it carries a codepoint for which [`crate::is_forbidden_path_char`]
///   returns `true`. The resolver never sees those paths, so this is the one check
///   it cannot make on the backend's behalf. [`NativeFs`] scans every canonical path
///   it resolves (`mds::io`); [`VirtualFs`] composes its keys only from entry keys,
///   import strings and base directories the resolver has already checked.
/// - **Path traversal prevention**: `resolve_entry` and `normalize_in_dir` must
///   reject paths that escape the intended root (e.g., `../../../etc/passwd`).
/// - **Direct calls**: `resolve_entry` and `normalize_in_dir` must refuse an empty
///   path and one containing `\0` or a forbidden path character even when called
///   directly rather than through the resolver, as both built-in backends do.
/// - **Segment cap**: `resolve_entry` and `normalize_in_dir` must refuse a path
///   of more than 256 segments with [`MdsError::ResourceLimit`]. Both built-in
///   backends enforce it on entry paths and on imports.
/// - **File size limits**: `read` must refuse content larger than
///   [`crate::MAX_FILE_SIZE`] bytes (10 MB) to prevent resource exhaustion.
/// - **Base directories**: a backend whose keys are host paths must override
///   `anchor_base_dir` to validate the base directory of a string compile —
///   refusing a symlinked final component and a canonical form that carries a
///   forbidden path character — and establish the containment root there; the
///   default returns the directory unchanged and anchors nothing.
/// - **`dir == ""`** (empty string) in `normalize_in_dir` means "virtual root" or
///   "no directory prefix" — resolve `relative` from the root of the key-space.
///
/// Failing to implement these controls silently bypasses the security enforced
/// by [`NativeFs`] and may expose the host system to arbitrary file reads or
/// denial-of-service attacks.
pub trait FileSystem: Send + Sync {
    /// Resolve an entry path — the file a compile starts from — to its key.
    ///
    /// The resolver has already refused an empty entry path or one containing a
    /// NUL byte or another forbidden path character (`mds::io`); both built-in
    /// backends refuse them again so a direct call is covered too. [`NativeFs`]
    /// returns the canonical absolute path, refuses a symlinked final component or
    /// a canonical path carrying a forbidden character, and anchors the project root
    /// on its first call. [`VirtualFs`] returns the key unchanged.
    ///
    /// # Errors
    ///
    /// - [`MdsError::Io`] when `path` is empty or contains a null byte (`\0`) or
    ///   another [`crate::is_forbidden_path_char`] codepoint; on [`NativeFs`] also
    ///   when its canonical path carries one or is not valid UTF-8.
    /// - [`MdsError::ResourceLimit`] when `path` has more than 256 segments.
    /// - [`MdsError::FileNotFound`] when the path does not exist ([`NativeFs`] only).
    /// - [`MdsError::ImportError`] when the final component is a symlink or the
    ///   path escapes an already-established project root ([`NativeFs`] only).
    fn resolve_entry(&self, path: &str) -> Result<String, MdsError>;

    /// Resolve `relative` directly within directory `dir` (import resolution).
    ///
    /// The directory is passed explicitly — callers derive it from the importing
    /// module's key with [`FileSystem::parent_dir`] — so there is no sentinel
    /// filename and no Windows verbatim-path hazard (PF-003).
    ///
    /// `dir == ""` means resolve from the root of the key-space (the same as an
    /// import from a top-level file). Implementations must enforce all traversal,
    /// null-byte, empty-path and segment-cap guards as described in the security
    /// contract above.
    ///
    /// # Errors
    ///
    /// Returns [`MdsError::ImportError`] when:
    /// - `relative` is empty
    /// - `relative` contains a null byte (`\0`) or another
    ///   [`crate::is_forbidden_path_char`] codepoint
    /// - the resolved path traverses above the key-space root (`..` from root)
    /// - the resolved path is a symlink ([`NativeFs`] only)
    /// - the resolved path escapes the established project root ([`NativeFs`] only)
    ///
    /// Returns [`MdsError::ResourceLimit`] when the resolved path exceeds
    /// `MAX_PATH_SEGMENTS` segments, and [`MdsError::Io`] when the canonical path
    /// carries a forbidden path character or is not valid UTF-8 ([`NativeFs`] only).
    fn normalize_in_dir(&self, dir: &str, relative: &str) -> Result<String, MdsError>;

    /// Return the directory portion of a normalized file key.
    ///
    /// For `NativeFs` this is `Path::new(key).parent()` (verbatim-path-safe).
    /// For `VirtualFs` this is everything before the last `/`.
    ///
    /// Returns `""` when `key` has no directory component (e.g., a top-level key
    /// or a rootless path). An empty string here means "root of the key-space"
    /// when passed to `normalize_in_dir`.
    fn parent_dir(&self, key: &str) -> String;

    /// Read the content of a normalized key.
    fn read(&self, normalized: &str) -> Result<String, MdsError>;

    /// Return `true` if the key refers to a `.md` (Markdown) file rather than `.mds`.
    fn is_markdown(&self, normalized: &str) -> bool;

    /// Anchor the base directory of a string compile and return the directory
    /// imports resolve from.
    ///
    /// [`crate::resolver::ModuleCache::resolve_source`] and its variants call this
    /// once, before any import resolves, and pass the returned string to
    /// [`FileSystem::normalize_in_dir`] as the importing directory. They refuse a
    /// `dir` carrying a forbidden path character (`mds::io`, #265) before calling
    /// it.
    ///
    /// The default is the identity: `dir` is returned unchanged and nothing is
    /// anchored, which suits an in-memory key-space with no host directories
    /// ([`VirtualFs`] uses it; `""` is the key-space root). [`NativeFs`] returns
    /// the canonical absolute directory, refuses one whose final component is a
    /// symlink, and anchors the project root at the first call — a later call
    /// never moves a root that is already established.
    ///
    /// # Errors
    ///
    /// The default never fails. [`NativeFs`] returns:
    /// - [`MdsError::Io`] when `dir` does not exist or cannot be resolved, when
    ///   `dir` or its canonical path carries a [`crate::is_forbidden_path_char`]
    ///   codepoint, or when its canonical path is not valid UTF-8.
    /// - [`MdsError::ImportError`] when the final component of `dir` is a symlink.
    fn anchor_base_dir(&self, dir: &str) -> Result<String, MdsError> {
        Ok(dir.to_string())
    }

    /// Return the established project root directory as a string, if any.
    ///
    /// Used by the source-map path-relativization choke-point
    /// ([`crate::source_path::relativize_source`]) to determine whether a
    /// resolved source path is contained within the project root and should be
    /// emitted as a root-relative (or map-relative) reference rather than
    /// degraded to a bare filename.
    ///
    /// # Default
    ///
    /// Returns `None` — suitable for virtual / in-memory filesystems
    /// ([`VirtualFs`] / WASM) where there is no containment concept.
    ///
    /// # Override
    ///
    /// [`NativeFs`] returns the path established by `init_root` (the project
    /// root found by walking up from the entry-point directory).  Returns
    /// `None` if the root has not been established yet (before any
    /// `resolve_entry` or `anchor_base_dir` call).
    ///
    /// # Contract
    ///
    /// Implementations must return `None` rather than a lossy string for a root
    /// that is not valid UTF-8 — a lossy anchor is not byte-faithful and must not
    /// participate in containment (#217).
    fn source_root(&self) -> Option<String> {
        None
    }
}

// ── Shared path guards ───────────────────────────────────────────────────────

/// The first [`crate::is_forbidden_path_char`] character of `path`, if any (#265).
pub(crate) fn first_forbidden_char(path: &str) -> Option<char> {
    path.chars()
        .find(|&ch| crate::lint::is_forbidden_path_char(ch))
}

/// The message for a path refused because it carries a forbidden path character
/// (#265): `<what> contains forbidden character U+XXXX: "<shown>"`.
///
/// `shown` is the path as the caller typed it, escaped with
/// [`crate::escape_path_for_message`], so the message itself carries none of the
/// 80 forbidden codepoints (TAB included) and never substitutes a resolved absolute
/// path for what the caller passed.
pub(crate) fn forbidden_char_message(what: &str, ch: char, shown: &str) -> String {
    format!(
        "{what} contains forbidden character U+{:04X}: \"{}\"",
        u32::from(ch),
        crate::lint::escape_path_for_message(shown)
    )
}

/// Refuse a caller-supplied path — an entry path or a base directory, named by
/// `what` — that carries a forbidden path character (`mds::io`, #265).
pub(crate) fn reject_forbidden_path_chars(what: &str, path: &str) -> Result<(), MdsError> {
    match first_forbidden_char(path) {
        Some(ch) => Err(MdsError::io(forbidden_char_message(what, ch, path))),
        None => Ok(()),
    }
}

/// Refuse a resolved path that carries a forbidden path character anywhere in it
/// (`mds::io`, #265).
///
/// The typed path has already been checked by the time a path is resolved; this
/// catches what the typed form cannot show — a symlinked directory whose target has a
/// hostile name, or a project that lives under one. The WHOLE path is scanned, not
/// only its final component. The message names `shown`, the path the caller typed,
/// never the absolute resolved path (R3 / CWE-209).
///
/// A path that is not valid UTF-8 is scanned lossily: every forbidden codepoint that
/// is validly encoded survives the conversion, and `key_of` then refuses the path
/// rather than turn it into a key.
pub(crate) fn reject_forbidden_in_path(resolved: &Path, shown: &str) -> Result<(), MdsError> {
    match first_forbidden_char(&resolved.to_string_lossy()) {
        Some(ch) => Err(MdsError::io(forbidden_char_message(
            "resolved path",
            ch,
            shown,
        ))),
        None => Ok(()),
    }
}

/// The key of a resolved `canonical` path: its exact UTF-8 string form.
///
/// A resolved path that is not valid UTF-8 is refused (`mds::io`), naming `shown`,
/// the path as written. Its lossy form would replace each invalid sequence with
/// U+FFFD and so name a DIFFERENT path — a valid-UTF-8 "twin" that `read` would then
/// open with none of the symlink, containment and forbidden-character checks the
/// real path passed. A symlinked parent directory can lead into such a name even
/// though every path a caller writes is UTF-8.
fn key_of(canonical: &Path, shown: &str) -> Result<String, MdsError> {
    canonical.to_str().map(str::to_owned).ok_or_else(|| {
        MdsError::io(format!(
            "resolved path is not valid UTF-8: \"{}\"",
            crate::lint::escape_path_for_message(shown)
        ))
    })
}

/// Reject an import path that is empty, contains a null byte, or carries any other
/// forbidden path character (#265).
///
/// Called by `normalize_in_dir` on both `NativeFs` and `VirtualFs`, so the error
/// strings stay identical across backends. The null-byte check runs before the
/// forbidden-character check (U+0000 is in that class) and keeps its own message.
fn validate_relative_import(relative: &str) -> Result<(), MdsError> {
    if relative.is_empty() {
        return Err(MdsError::import_error("import path is empty"));
    }
    if relative.contains('\0') {
        return Err(MdsError::import_error("import path contains null byte"));
    }
    if let Some(ch) = first_forbidden_char(relative) {
        return Err(MdsError::import_error(forbidden_char_message(
            "import path",
            ch,
            relative,
        )));
    }
    Ok(())
}

/// Reject an entry path that is empty, contains a null byte, or carries any other
/// forbidden path character (`mds::io`, #265).
///
/// An entry path names the file a compile starts from. It is caller input, not an
/// `@import` string, so it reports `mds::io` like the other entry-path checks at
/// the API boundary (a path that is not valid UTF-8). The resolver runs this
/// before [`FileSystem::resolve_entry`], so a custom backend is covered; both
/// built-in backends run it again so a direct trait call is covered too.
///
/// The path is escaped in the message: it is the string the caller passed, never a
/// resolved absolute path. The null-byte check runs before the forbidden-character
/// check and keeps its own message.
pub(crate) fn validate_entry_path(path: &str) -> Result<(), MdsError> {
    if path.is_empty() {
        return Err(MdsError::io("entry path is empty"));
    }
    if path.contains('\0') {
        return Err(MdsError::io(format!(
            "entry path contains null byte: \"{}\"",
            crate::lint::escape_path_for_message(path)
        )));
    }
    reject_forbidden_path_chars("entry path", path)
}

/// Refuse a path of more than [`MAX_PATH_SEGMENTS`] segments (`mds::resource_limit`).
///
/// Counts the non-empty segments other than `.`, split on the platform's path
/// separators (`/`, and also `\` on Windows); `..` counts. Shared by both
/// backends' `resolve_entry` and by `NativeFs::normalize_in_dir`, so the cap and
/// its message are the same wherever it applies.
fn check_segment_count(path: &str) -> Result<(), MdsError> {
    let segments = path
        .split(std::path::is_separator)
        .filter(|s| !s.is_empty() && *s != ".")
        .count();
    if segments > MAX_PATH_SEGMENTS {
        return Err(MdsError::resource_limit(format!(
            "import path exceeds maximum segment count ({MAX_PATH_SEGMENTS}): \"{}\"",
            crate::lint::sanitize_control_chars_wire(path)
        )));
    }
    Ok(())
}

// ── VirtualFs segment logic ──────────────────────────────────────────────────

/// Resolve a relative path string against a pre-split directory segment stack.
///
/// `dir_segments` is a `Vec<&str>` seeded from the importing directory — slices
/// borrowed from the original `dir` string, no per-segment allocation.
/// `relative` is the raw relative import path (may contain `.`, `..`, `/`).
///
/// Returns the resolved key (joined by `/`) or an error for:
/// - traversal above root (`..` when segments is empty)
/// - empty resolved key
/// - exceeding [`MAX_PATH_SEGMENTS`]
///
/// The path-resolution core of [`VirtualFs::normalize_in_dir`].
fn resolve_relative_segments<'a>(
    mut dir_segments: Vec<&'a str>,
    relative: &'a str,
) -> Result<String, MdsError> {
    for part in relative.split('/') {
        match part {
            "" | "." => {
                // Skip empty parts (leading "./") and "." segments.
            }
            ".." => {
                if dir_segments.is_empty() {
                    return Err(MdsError::import_error(format!(
                        "import path escapes project directory: \"{relative}\""
                    )));
                }
                dir_segments.pop();
            }
            seg => {
                if dir_segments.len() >= MAX_PATH_SEGMENTS {
                    return Err(MdsError::resource_limit(format!(
                        "import path exceeds maximum segment count ({MAX_PATH_SEGMENTS}): \"{relative}\""
                    )));
                }
                dir_segments.push(seg);
            }
        }
    }

    if dir_segments.is_empty() {
        return Err(MdsError::import_error(format!(
            "import path resolves to empty key: \"{relative}\""
        )));
    }

    Ok(dir_segments.join("/"))
}

// ── VirtualFs ────────────────────────────────────────────────────────────────

/// Virtual filesystem backed by an in-memory `HashMap`.
///
/// Keys use `/` as separator regardless of host OS.
/// Designed for WASM environments and testing.
#[derive(Debug)]
pub struct VirtualFs {
    modules: HashMap<String, String>,
}

impl VirtualFs {
    /// Create a new `VirtualFs` from a map of key → content.
    pub fn new(modules: HashMap<String, String>) -> Self {
        Self { modules }
    }
}

impl FileSystem for VirtualFs {
    /// Return the entry key unchanged.
    ///
    /// The key is not rewritten (no `.`/`..` collapsing): it must match a key of
    /// the module map exactly. Rejects an empty key and one containing a null byte
    /// or another [`crate::is_forbidden_path_char`] codepoint (`mds::io`), and keys
    /// of more than 256 segments (`mds::resource_limit`).
    fn resolve_entry(&self, path: &str) -> Result<String, MdsError> {
        validate_entry_path(path)?;
        check_segment_count(path)?;
        Ok(path.to_string())
    }

    fn normalize_in_dir(&self, dir: &str, relative: &str) -> Result<String, MdsError> {
        validate_relative_import(relative)?;

        // Seed segment stack from the explicit directory (not a file key — no parent() needed).
        // Borrow slices from `dir` directly — no per-segment allocation; only the final
        // `join("/")` inside `resolve_relative_segments` allocates.
        let dir_segments: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();

        resolve_relative_segments(dir_segments, relative)
    }

    fn parent_dir(&self, key: &str) -> String {
        key.rsplit_once('/')
            .map(|(d, _)| d)
            .unwrap_or("")
            .to_string()
    }

    fn read(&self, normalized: &str) -> Result<String, MdsError> {
        let content = self
            .modules
            .get(normalized)
            .ok_or_else(|| MdsError::module_not_found(normalized.to_string()))?;
        if content.len() as u64 > MAX_FILE_SIZE {
            return Err(MdsError::resource_limit(format!(
                "file too large ({} bytes, max {} bytes): {normalized}",
                content.len(),
                MAX_FILE_SIZE,
            )));
        }
        Ok(content.clone())
    }

    fn is_markdown(&self, normalized: &str) -> bool {
        Path::new(normalized).extension().and_then(|e| e.to_str()) == Some("md")
    }
}

// ── NativeFs ─────────────────────────────────────────────────────────────────

/// Native OS filesystem implementation.
///
/// Enforces symlink rejection, path traversal prevention,
/// file size limits, and UTF-8 validation.
#[derive(Debug)]
pub struct NativeFs {
    root_dir: OnceLock<PathBuf>,
}

/// Return the effective parent directory of `path`, always resolving to
/// `Path::new(".")` for bare filenames.
///
/// `Path::parent()` returns `Some("")` (an empty path) for bare relative
/// filenames like `"hello.mds"`, NOT `None`. An empty path fails
/// `canonicalize()` with a file-not-found error, which is the root cause of
/// the bare-filename release blocker.  This function maps both `Some("")` and
/// `None` to `Path::new(".")` so that `check_symlink` resolves bare filenames
/// against the current working directory, matching the behaviour users expect.
///
/// Absolute paths and paths with a non-empty parent component are returned
/// unchanged.
pub fn effective_parent(path: &Path) -> &Path {
    match path.parent() {
        None => Path::new("."),
        Some(p) if p.as_os_str().is_empty() => Path::new("."),
        Some(p) => p,
    }
}

impl NativeFs {
    /// Create a new `NativeFs` with no root directory set.
    ///
    /// The root is established on the first call to [`FileSystem::resolve_entry`]
    /// or [`FileSystem::anchor_base_dir`].
    pub fn new() -> Self {
        Self {
            root_dir: OnceLock::new(),
        }
    }

    /// Canonicalize `path`, refusing it when its final component is a symlink.
    ///
    /// Symlinks in the parent directories are followed (the parent is
    /// canonicalized first); only the final component is checked, by its file
    /// type — see `check_symlink_named` for the exact rule.
    ///
    /// Returns the canonical `PathBuf` for valid (non-symlinked) paths, or an error
    /// if the final path component is a symlink. This makes it a drop-in replacement
    /// for `std::fs::canonicalize` at security boundaries (PF-004).
    ///
    /// Error messages use `path.display()` so callers that pass an already-safe
    /// path (e.g. a CLI-provided absolute path) get a useful diagnostic.  For
    /// import-path resolution use the private `check_symlink_named` instead, which
    /// accepts a separate `shown` string to avoid leaking the absolute joined path.
    ///
    /// # Errors
    ///
    /// - `MdsError::ImportError` — the final path component is a symlink.
    /// - `MdsError::FileNotFound` — the path does not exist or the parent cannot
    ///   be resolved.
    /// - `MdsError::Io` — `path`, or the canonical path it resolves to, carries a
    ///   [`crate::is_forbidden_path_char`] codepoint (#265). The typed form is
    ///   checked before the filesystem is touched.
    pub fn check_symlink(path: &Path) -> Result<PathBuf, MdsError> {
        // Delegate to the named variant using path.display() as the shown string.
        // External callers (CLI lint/fmt/watch/output) pass absolute paths where
        // showing the full path in error messages is appropriate.
        let shown = path.display().to_string();
        reject_forbidden_path_chars("path", &shown)?;
        Self::check_symlink_named(path, &shown)
    }

    /// Canonicalize a directory path, handling the filesystem-root edge case (#371).
    ///
    /// A filesystem root (`/` on Unix, or a drive root such as `C:\` on
    /// Windows) has no parent component, so `check_symlink_named` — which
    /// canonicalizes the PARENT directory before checking the final path
    /// component — has no parent to canonicalize; `Path::file_name()` returns
    /// `None` for a root, so `check_symlink_named` fails immediately with
    /// `FileNotFound`, before any syscall (`#371`).
    ///
    /// Detect that case via `path.has_root() && path.parent().is_none()` and
    /// canonicalize the root directly instead: a filesystem root can never
    /// itself be a symlink (there is nothing "above" it to substitute it
    /// with), so a plain `canonicalize()` plus an `is_dir()` check is
    /// sufficient and correct. Every other path — including a file that
    /// happens to live directly under a root, like `/x.mds`, whose
    /// `parent()` is `Some("/")`, not `None` — still goes through the
    /// unchanged `check_symlink_named` path.
    ///
    /// Deliberately never calls [`effective_parent`] on the root branch:
    /// `effective_parent` exists to map a BARE FILENAME's empty parent to
    /// `"."` (current working directory) so single-segment relative paths
    /// resolve. A root path's absent parent means something different —
    /// there is no parent because the root IS the top of the filesystem —
    /// and mapping it to `"."` would silently re-anchor resolution at the
    /// current working directory instead of the root the caller asked for
    /// (the cwd trap, #371).
    ///
    /// A base directory of `/` (or a drive root) is intended, supported
    /// behavior, not a privilege escalation: the base directory is always
    /// caller-chosen, and this function only proves the path exists and is a
    /// directory — it grants no access beyond what the caller already had.
    ///
    /// Both branches refuse a canonical path that carries a forbidden path
    /// character (#265): the root branch here, every other path inside
    /// `check_symlink_named`.
    fn canonical_dir(path: &Path, shown: &str) -> Result<PathBuf, MdsError> {
        if path.has_root() && path.parent().is_none() {
            let canonical = path
                .canonicalize()
                .map_err(|e| MdsError::io(format!("cannot resolve path {shown}: {e}")))?;
            if !canonical.is_dir() {
                return Err(MdsError::io(format!(
                    "cannot resolve path {shown}: not a directory"
                )));
            }
            reject_forbidden_in_path(&canonical, shown)?;
            Ok(canonical)
        } else {
            Self::check_symlink_named(path, shown)
        }
    }

    /// Walk up from a directory to find the project root.
    ///
    /// Looks for `.git` or `.mdsroot` markers.
    /// Falls back to the given directory if no marker is found.
    fn find_project_root(start: &Path) -> PathBuf {
        let mut dir = start.to_path_buf();
        for _ in 0..MAX_TRAVERSAL_DEPTH {
            for marker in [".git", ".mdsroot"] {
                if dir.join(marker).exists() {
                    return dir;
                }
            }
            if !dir.pop() {
                return start.to_path_buf();
            }
        }
        start.to_path_buf()
    }

    /// Return a display-safe path for `path` relative to the established project root.
    ///
    /// Uses [`crate::source_path::relativize_source`] so the result is never an
    /// absolute path (R3 / CWE-209).  Falls back to the basename when the root has
    /// not been established yet (e.g. before the first `resolve_entry` call).
    fn display_of(&self, path: &Path) -> String {
        let path_str = path.display().to_string();
        let root_str = self.source_root();
        let root = root_str.as_deref().map(Path::new);
        crate::source_path::relativize_source(&path_str, None, root)
    }

    /// Check that `canonical` stays within the established root directory.
    ///
    /// `shown` is the user-supplied import path string; it appears in the error
    /// message so the user sees what they typed, not an absolute resolved path
    /// (matches the `VirtualFs` sibling message format / R3 CWE-209).
    fn check_path_traversal(&self, canonical: &Path, shown: &str) -> Result<(), MdsError> {
        if let Some(root) = self.root_dir.get() {
            if !canonical.starts_with(root) {
                return Err(MdsError::import_error(format!(
                    "import path escapes project directory: \"{shown}\""
                )));
            }
        }
        Ok(())
    }

    /// Symlink check that uses a caller-supplied `shown` name in error messages.
    ///
    /// Separates the path used for OS calls from the string shown to the user,
    /// so import errors report the relative import string rather than the
    /// absolute joined path (R3 / CWE-209).
    ///
    /// The parent directory is canonicalized (its symlinks are followed) and the
    /// final component is joined to it as written. That component is refused when
    /// its own file type — `symlink_metadata`, which does not follow it — is a
    /// symlink. On Windows `is_symlink` is true for every name-surrogate reparse
    /// point, so a junction is refused as well as a symbolic link.
    ///
    /// The file type decides, not a comparison of the canonical path with the
    /// joined one: on a case-insensitive volume (default macOS APFS, NTFS) the
    /// canonical path carries the on-disk spelling of the name (its case), so
    /// `./Header.mds` for `header.mds` differs from its canonical form without
    /// being a symlink (#408). The canonical path is what this returns, so a
    /// module's key is its on-disk spelling and every spelling of one file shares
    /// one key.
    ///
    /// Canonicalizing a non-symlink final component only respells its name; it
    /// never changes the directory. A canonical path whose directory is not the
    /// canonical parent therefore means the component was replaced by a link
    /// between the two calls, and it is refused the same way.
    ///
    /// Finally the whole canonical path is refused (`mds::io`) when it carries a
    /// forbidden path character (#265): a parent directory followed through a
    /// symlink can have a hostile name the caller never typed.
    fn check_symlink_named(path: &Path, shown: &str) -> Result<PathBuf, MdsError> {
        let file_name = path
            .file_name()
            .ok_or_else(|| MdsError::file_not_found(shown.to_string()))?;

        let parent = effective_parent(path);
        let canonical_parent = parent
            .canonicalize()
            .map_err(|_| MdsError::file_not_found(shown.to_string()))?;
        let joined = canonical_parent.join(file_name);
        let symlink_error =
            || MdsError::import_error(format!("symlinks are not allowed in imports: {shown}"));

        let file_type = std::fs::symlink_metadata(&joined)
            .map_err(|_| MdsError::file_not_found(shown.to_string()))?
            .file_type();
        if file_type.is_symlink() {
            return Err(symlink_error());
        }

        let canonical = joined
            .canonicalize()
            .map_err(|_| MdsError::file_not_found(shown.to_string()))?;
        if canonical.parent() != Some(canonical_parent.as_path()) {
            return Err(symlink_error());
        }
        reject_forbidden_in_path(&canonical, shown)?;
        Ok(canonical)
    }

    /// Resolve `relative` within `dir` (given as a `&Path`) using the established
    /// security primitives — no `Path`→`String`→`Path` round-trip on the hot path.
    ///
    /// Validates `relative` (empty, null byte, forbidden path characters, segment
    /// cap), joins with `dir` via `Path::join` (verbatim-path-safe on Windows;
    /// avoids PF-003 / #133), then runs `check_symlink_named` (which also refuses a
    /// forbidden character anywhere in the canonical path) and
    /// `check_path_traversal` before returning the canonical key string.
    ///
    /// Does NOT call `init_root` — only entry-point resolution
    /// ([`FileSystem::resolve_entry`]) anchors the security root.
    fn normalize_in_dir_impl(&self, dir: &Path, relative: &str) -> Result<String, MdsError> {
        validate_relative_import(relative)?;
        check_segment_count(relative)?;
        let path = dir.join(relative);
        // Use check_symlink_named so the error message shows the relative import
        // string (what the user typed) rather than the absolute joined path (R3 / CWE-209).
        let canonical = Self::check_symlink_named(&path, relative)?;
        self.check_path_traversal(&canonical, relative)?;
        key_of(&canonical, relative)
    }

    /// Initialize root_dir from a canonical entry-point directory.
    fn init_root(&self, canonical_dir: &Path) {
        // Skip the up-to-256 exists() calls if the root is already established.
        if self.root_dir.get().is_some() {
            return;
        }
        // OnceLock: set() silently no-ops if another thread raced here.
        let root = Self::find_project_root(canonical_dir);
        let _ = self.root_dir.set(root);
    }
}

impl Default for NativeFs {
    fn default() -> Self {
        Self::new()
    }
}

impl FileSystem for NativeFs {
    fn resolve_entry(&self, path: &str) -> Result<String, MdsError> {
        validate_entry_path(path)?;
        check_segment_count(path)?;
        // Use check_symlink (shows path.display()) here — `path` is the
        // caller-supplied entry path and path.display() == path, so there is no
        // leakage risk.
        let canonical = Self::check_symlink(Path::new(path))?;
        // Before the root is anchored: a path refused here must not anchor it.
        let key = key_of(&canonical, path)?;
        // Anchor the security root on first entry-point resolution.
        // effective_parent is safe even if canonical is somehow relative — avoids PF-006.
        let entry_dir = effective_parent(&canonical);
        self.init_root(entry_dir);
        self.check_path_traversal(&canonical, path)?;
        Ok(key)
    }

    fn normalize_in_dir(&self, dir: &str, relative: &str) -> Result<String, MdsError> {
        self.normalize_in_dir_impl(Path::new(dir), relative)
    }

    fn parent_dir(&self, key: &str) -> String {
        Path::new(key)
            .parent()
            .unwrap_or(Path::new(""))
            .display()
            .to_string()
    }

    fn read(&self, normalized: &str) -> Result<String, MdsError> {
        let path = Path::new(normalized);
        // Compute a display-safe (root-relative) path before any IO so errors
        // always show a relative path rather than the canonical absolute key (R3 / CWE-209).
        let display = self.display_of(path);
        // Read bytes first, then check size — this is the TOCTOU-safe pattern.
        // A metadata() pre-check would introduce a race window between the size
        // check and the actual read. Read first, reject after.
        let bytes =
            std::fs::read(path).map_err(|e| MdsError::io(format!("cannot read {display}: {e}")))?;
        if bytes.len() as u64 > MAX_FILE_SIZE {
            return Err(MdsError::resource_limit(format!(
                "file too large ({} bytes, max {} bytes): {display}",
                bytes.len(),
                MAX_FILE_SIZE,
            )));
        }
        String::from_utf8(bytes)
            .map_err(|e| MdsError::io(format!("invalid UTF-8 in {display}: {e}")))
    }

    fn is_markdown(&self, normalized: &str) -> bool {
        Path::new(normalized).extension().and_then(|e| e.to_str()) == Some("md")
    }

    fn anchor_base_dir(&self, dir: &str) -> Result<String, MdsError> {
        // canonical_dir, not check_symlink, so that a filesystem-root base
        // directory (`/`, a Windows drive root) anchors at the root instead of
        // hitting the `file_name() == None` cwd trap (#371). For every other
        // path canonical_dir is check_symlink_named, so a symlinked directory is
        // refused BEFORE it can anchor the security root at an
        // attacker-controlled location (#21) — the root is only anchored at a
        // directory that passed the check.
        //
        // canonical_dir returns ImportError (symlink), FileNotFound (missing
        // path) or Io (root branch, or a forbidden character in the canonical
        // path). FileNotFound is re-wrapped as Io: resolving a base directory is
        // caller input, not an import step.
        //
        // The typed form is refused first (#265), so no later message can carry a
        // forbidden character from it; the resolver has already run the same check
        // for every backend, this covers a direct trait call.
        reject_forbidden_path_chars("base directory", dir)?;
        let canonical = Self::canonical_dir(Path::new(dir), dir).map_err(|e| match e {
            MdsError::FileNotFound { .. } => {
                MdsError::io(format!("cannot resolve path {dir}: {e}"))
            }
            other => other,
        })?;
        // Before the root is anchored: a directory refused here must not anchor it.
        let key = key_of(&canonical, dir)?;
        // First writer wins: init_root never moves an established root.
        self.init_root(&canonical);
        Ok(key)
    }

    fn source_root(&self) -> Option<String> {
        // `to_str`, not `display()`: a root that is not valid UTF-8 has no
        // byte-faithful string form, and `None` is the documented "no containment
        // concept" value every consumer already guards (#217).  Containment itself
        // is unaffected — `check_path_traversal` compares `Path`s from `root_dir`
        // directly and never goes through this string.
        self.root_dir
            .get()
            .and_then(|p| p.to_str().map(str::to_owned))
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::make_symlink;
    use std::io::Write;
    use tempfile::TempDir;

    // ── VirtualFs::resolve_entry ──────────────────────────────────────────────

    fn vfs() -> VirtualFs {
        VirtualFs::new(HashMap::new())
    }

    /// `n` path segments `seg0/seg1/…` joined by `/`.
    fn segments(n: usize) -> String {
        (0..n)
            .map(|i| format!("seg{i}"))
            .collect::<Vec<_>>()
            .join("/")
    }

    /// The `mds::…` diagnostic code of an error.
    fn code_of(err: &MdsError) -> Option<String> {
        miette::Diagnostic::code(err).map(|c| c.to_string())
    }

    #[test]
    fn vfs_resolve_entry_returns_key_unchanged() {
        assert_eq!(vfs().resolve_entry("main.mds").unwrap(), "main.mds");
        // The key is NOT rewritten: `.`/`..` collapsing is import resolution
        // (`normalize_in_dir`), and an entry key must match a module-map key
        // exactly. Control: the same string through normalize_in_dir IS rewritten.
        let raw = "./a/../main.mds";
        assert_eq!(vfs().resolve_entry(raw).unwrap(), raw);
        assert_eq!(vfs().normalize_in_dir("", raw).unwrap(), "main.mds");
    }

    #[test]
    fn vfs_resolve_entry_null_byte_is_io_error() {
        let err = vfs().resolve_entry("a\0b.mds").unwrap_err();
        assert!(
            matches!(err, MdsError::Io { .. }),
            "expected Io, got {err:?}"
        );
        assert_eq!(code_of(&err).as_deref(), Some("mds::io"));
        let msg = err.to_string();
        // The key is shown WIRE-escaped: the escape is present, the raw NUL is not.
        let escaped = format!("a\\u{:04X}b.mds", 0);
        assert!(
            msg.contains("null byte") && msg.contains(&escaped),
            "expected {escaped:?} in the message, got: {msg}"
        );
        assert!(!msg.contains('\0'), "raw NUL must not reach the message");
    }

    #[test]
    fn vfs_resolve_entry_empty_is_io_error() {
        let err = vfs().resolve_entry("").unwrap_err();
        assert!(
            matches!(err, MdsError::Io { .. }),
            "expected Io, got {err:?}"
        );
        assert_eq!(code_of(&err).as_deref(), Some("mds::io"));
        assert!(
            err.to_string().contains("entry path is empty"),
            "got: {err}"
        );
    }

    #[test]
    fn vfs_resolve_entry_segment_cap() {
        let err = vfs()
            .resolve_entry(&segments(MAX_PATH_SEGMENTS + 1))
            .unwrap_err();
        assert!(
            matches!(err, MdsError::ResourceLimit { .. }),
            "expected ResourceLimit, got {err:?}"
        );
        // Control: exactly at the cap is accepted, unchanged.
        let at_cap = segments(MAX_PATH_SEGMENTS);
        assert_eq!(vfs().resolve_entry(&at_cap).unwrap(), at_cap);
    }

    // ── VirtualFs::normalize_in_dir relative to a module's parent_dir ─────────

    #[test]
    fn vfs_normalize_in_dir_two_levels_up() {
        let fs = vfs();
        let result = fs.normalize_in_dir(&fs.parent_dir("a/b/c.mds"), "../../d.mds");
        assert_eq!(result.unwrap(), "d.mds");
    }

    #[test]
    fn vfs_normalize_in_dir_dot_segments_collapsed() {
        let fs = vfs();
        let result = fs.normalize_in_dir(&fs.parent_dir("a/b.mds"), "./././c.mds");
        assert_eq!(result.unwrap(), "a/c.mds");
    }

    #[test]
    fn vfs_normalize_in_dir_deep_traversal_at_boundary() {
        // "deep/nested/file.mds" has dir = "deep/nested"; three ".." would escape.
        let fs = vfs();
        let result = fs.normalize_in_dir(&fs.parent_dir("deep/nested/file.mds"), "../../../x.mds");
        assert!(result.is_err(), "expected Err when escaping root");
        // Control: two ".." stay at the root.
        let ok = fs.normalize_in_dir(&fs.parent_dir("deep/nested/file.mds"), "../../x.mds");
        assert_eq!(ok.unwrap(), "x.mds");
    }

    #[test]
    fn vfs_normalize_in_dir_subdirectory() {
        let fs = vfs();
        let result = fs.normalize_in_dir(&fs.parent_dir("a/b.mds"), "./c/d.mds");
        assert_eq!(result.unwrap(), "a/c/d.mds");
    }

    // ── VirtualFs::read ───────────────────────────────────────────────────────

    #[test]
    fn vfs_read_existing_key() {
        let fs = VirtualFs::new(HashMap::from([(
            "main.mds".to_string(),
            "hello".to_string(),
        )]));
        assert_eq!(fs.read("main.mds").unwrap(), "hello");
    }

    #[test]
    fn vfs_read_missing_key_module_not_found() {
        // R6: VirtualFs::read on a missing key produces ModuleNotFound, not FileNotFound.
        let fs = VirtualFs::new(HashMap::new());
        let err = fs.read("missing.mds").unwrap_err();
        assert!(
            matches!(err, MdsError::ModuleNotFound { .. }),
            "expected ModuleNotFound, got {err:?}"
        );
    }

    // ── VirtualFs::is_markdown ────────────────────────────────────────────────

    #[test]
    fn vfs_is_markdown_md_extension() {
        assert!(vfs().is_markdown("readme.md"));
    }

    #[test]
    fn vfs_is_markdown_mds_extension() {
        assert!(!vfs().is_markdown("main.mds"));
    }

    #[test]
    fn vfs_is_markdown_no_extension() {
        assert!(!vfs().is_markdown("no_extension"));
    }

    // ── NativeFs tests ────────────────────────────────────────────────────────

    fn make_temp_file(dir: &TempDir, name: &str, content: &str) -> PathBuf {
        let path = dir.path().join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    /// A relative import that climbs out of any project directory and lands on
    /// `target`: `"../"` × 20, then `target`'s normal components joined by `/`.
    ///
    /// The root/drive prefix is dropped on purpose — `..` clamps at the filesystem
    /// root on Unix and Windows alike (`Path::join` + canonicalize pop only normal
    /// components), so this resolves to `target` wherever the importing directory
    /// is, and the containment check is what must reject it on every platform.
    /// Embedding the absolute path instead would put a `C:` component mid-path on
    /// Windows and fail as "file not found" before containment is ever reached.
    fn escape_to(target: &Path) -> String {
        let tail: Vec<String> = target
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        "../".repeat(20) + &tail.join("/")
    }

    #[test]
    fn native_resolve_entry_returns_canonical_key() {
        let dir = TempDir::new().unwrap();
        let file = make_temp_file(&dir, "main.mds", "hello");
        let fs = NativeFs::new();
        let key = fs
            .resolve_entry(&file.display().to_string())
            .expect("resolve_entry");
        assert_eq!(
            Path::new(&key),
            file.canonicalize().unwrap(),
            "the entry key must be the canonical absolute path"
        );
    }

    #[test]
    fn native_normalize_in_dir_from_entry_parent_dir() {
        let dir = TempDir::new().unwrap();
        let file = make_temp_file(&dir, "main.mds", "hello");
        let sibling = make_temp_file(&dir, "sibling.mds", "world");

        let fs = NativeFs::new();
        // Resolve the entry point to establish the root and get its key.
        let base_key = fs
            .resolve_entry(&file.display().to_string())
            .expect("resolve_entry failed");
        // Now resolve a sibling from the entry's directory, as the resolver does.
        let key = fs
            .normalize_in_dir(&fs.parent_dir(&base_key), "./sibling.mds")
            .expect("sibling import");
        assert_eq!(Path::new(&key), sibling.canonicalize().unwrap());
    }

    #[test]
    fn native_resolve_entry_symlink_rejected() {
        let dir = TempDir::new().unwrap();
        let target = make_temp_file(&dir, "target.mds", "hello");
        let link_path = dir.path().join("link.mds");
        if !make_symlink(&target, &link_path) {
            return;
        }

        let fs = NativeFs::new();
        let result = fs.resolve_entry(&link_path.display().to_string());
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("symlinks"),
            "expected symlinks in error, got: {msg}"
        );
        // Control: the symlink's real target resolves.
        assert!(fs.resolve_entry(&target.display().to_string()).is_ok());
    }

    #[test]
    fn native_resolve_entry_segment_cap() {
        // A relative entry path, resolved against the test's cwd: nothing on disk
        // is needed, because the cap fires before the filesystem is touched.
        let err = NativeFs::new()
            .resolve_entry(&segments(MAX_PATH_SEGMENTS + 1))
            .unwrap_err();
        assert!(
            matches!(err, MdsError::ResourceLimit { .. }),
            "expected ResourceLimit, got {err:?}"
        );
        // Control: exactly at the cap passes the check and reaches the
        // filesystem, where the path does not exist.
        let err = NativeFs::new()
            .resolve_entry(&segments(MAX_PATH_SEGMENTS))
            .unwrap_err();
        assert!(
            matches!(err, MdsError::FileNotFound { .. }),
            "control: expected FileNotFound at the cap, got {err:?}"
        );
    }

    #[test]
    fn native_normalize_in_dir_segment_cap() {
        let dir = TempDir::new().unwrap();
        let dir_str = dir.path().display().to_string();
        let fs = NativeFs::new();
        let over = format!("./{}", segments(MAX_PATH_SEGMENTS + 1));
        let err = fs.normalize_in_dir(&dir_str, &over).unwrap_err();
        assert!(
            matches!(err, MdsError::ResourceLimit { .. }),
            "expected ResourceLimit, got {err:?}"
        );
        // Control: exactly at the cap (the leading "." does not count) reaches
        // the filesystem, where the path does not exist.
        let at_cap = format!("./{}", segments(MAX_PATH_SEGMENTS));
        let err = fs.normalize_in_dir(&dir_str, &at_cap).unwrap_err();
        assert!(
            matches!(err, MdsError::FileNotFound { .. }),
            "control: expected FileNotFound at the cap, got {err:?}"
        );
    }

    #[test]
    fn native_normalize_in_dir_symlink_rejected() {
        // Security boundary: normalize_in_dir (the import branch) must reject symlinks
        // via check_symlink inside normalize_in_dir_impl. A regression that dropped
        // check_symlink from normalize_in_dir_impl would pass the entry-point symlink
        // test above yet silently allow symlinks through all @import resolution.
        let dir = TempDir::new().unwrap();
        let target = make_temp_file(&dir, "target.mds", "hello");
        let link_path = dir.path().join("link.mds");
        if !make_symlink(&target, &link_path) {
            return;
        }

        let fs = NativeFs::new();
        // Establish root via a real (non-symlinked) entry point.
        fs.resolve_entry(&target.display().to_string())
            .expect("resolve_entry should succeed");

        let dir_str = dir.path().display().to_string();
        let result = fs.normalize_in_dir(&dir_str, "./link.mds");
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("symlinks"),
            "expected 'symlinks' in error when normalize_in_dir encounters a symlink, got: {msg}"
        );
    }

    #[test]
    fn native_normalize_in_dir_absolute_path_injection_rejected() {
        // Security boundary: an absolute path outside the established project root
        // must be rejected with "escapes project directory".
        let project_dir = TempDir::new().unwrap();
        let outside_dir = TempDir::new().unwrap();

        let entry = make_temp_file(&project_dir, "main.mds", "hello");
        let outside = make_temp_file(&outside_dir, "secret.mds", "secret");

        let fs = NativeFs::new();
        // Establish root via entry point.
        let base_key = fs
            .resolve_entry(&entry.display().to_string())
            .expect("resolve_entry should succeed");

        // Absolute path pointing outside the project root.
        let result = fs.normalize_in_dir(&fs.parent_dir(&base_key), &outside.display().to_string());
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("escapes project"),
            "expected 'escapes project' in error, got: {msg}"
        );
    }

    #[test]
    fn native_anchor_base_dir_rejects_paths_outside_root() {
        // anchor_base_dir initializes the root directory so that subsequent
        // imports reject paths outside that root.
        let project_dir = TempDir::new().unwrap();
        let outside_dir = TempDir::new().unwrap();

        let entry = make_temp_file(&project_dir, "main.mds", "hello");
        let outside = make_temp_file(&outside_dir, "secret.mds", "secret");

        let fs = NativeFs::new();
        // Initialize root explicitly via anchor_base_dir, not via resolve_entry.
        fs.anchor_base_dir(&project_dir.path().display().to_string())
            .expect("anchor_base_dir should succeed for a real directory");

        // Establish a valid base key by resolving the entry point (the anchor
        // already set the root, so it stays at project_dir), then test the
        // already-set root with an import rather than another entry.
        let base_key = fs
            .resolve_entry(&entry.display().to_string())
            .expect("resolve_entry should succeed");

        // A path outside the root must be rejected.
        let result = fs.normalize_in_dir(&fs.parent_dir(&base_key), &outside.display().to_string());
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("escapes project"),
            "expected 'escapes project' after anchor_base_dir, got: {msg}"
        );
    }

    #[test]
    fn native_anchor_base_dir_first_writer_wins() {
        // The first anchor establishes the root; a later anchor validates and
        // returns its own directory but never moves the root. Each project
        // carries a `.mdsroot` marker so the walk-up stops at it.
        let first = TempDir::new().unwrap();
        let second = TempDir::new().unwrap();
        let first_root = first.path().canonicalize().unwrap();
        let second_root = second.path().canonicalize().unwrap();
        std::fs::write(first_root.join(".mdsroot"), "").unwrap();
        std::fs::write(second_root.join(".mdsroot"), "").unwrap();
        let second_file = make_temp_file(&second, "x.mds", "x");

        let fs = NativeFs::new();
        fs.anchor_base_dir(first_root.to_str().unwrap()).unwrap();
        let returned = fs.anchor_base_dir(second_root.to_str().unwrap()).unwrap();

        assert_eq!(
            Path::new(&returned),
            second_root,
            "a later anchor still returns its own canonical directory"
        );
        assert_eq!(
            fs.source_root().map(PathBuf::from),
            Some(first_root.clone()),
            "the first anchor's root must stay in place"
        );
        // The unmoved root is what containment enforces: a file in the second
        // directory is outside it.
        let err = fs.normalize_in_dir(&returned, "./x.mds").unwrap_err();
        assert!(
            err.to_string().contains("escapes project"),
            "containment must use the first root, got: {err}"
        );
        // Control: a fresh backend anchored at the second directory accepts it.
        let control = NativeFs::new();
        let dir = control
            .anchor_base_dir(second_root.to_str().unwrap())
            .unwrap();
        assert_eq!(
            Path::new(&control.normalize_in_dir(&dir, "./x.mds").unwrap()),
            second_file.canonicalize().unwrap()
        );
    }

    #[test]
    fn native_read_file_content() {
        let dir = TempDir::new().unwrap();
        let file = make_temp_file(&dir, "hello.mds", "Hello World!");
        let fs = NativeFs::new();
        let key = fs
            .resolve_entry(&file.display().to_string())
            .expect("resolve_entry");
        let content = fs.read(&key).expect("read");
        assert_eq!(content, "Hello World!");
    }

    #[test]
    fn native_read_nonexistent_errors() {
        let fs = NativeFs::new();
        let result = fs.read("/nonexistent/path/that/does/not/exist.mds");
        assert!(result.is_err(), "expected Err for missing file");
    }

    #[test]
    fn native_is_markdown_md() {
        let fs = NativeFs::new();
        assert!(fs.is_markdown("file.md"));
    }

    #[test]
    fn native_is_markdown_mds() {
        let fs = NativeFs::new();
        assert!(!fs.is_markdown("file.mds"));
    }

    // ── VirtualFs size limit ──────────────────────────────────────────────────

    #[test]
    fn vfs_read_over_size_limit_errors() {
        // Content slightly over 10 MB.
        let big = "x".repeat((MAX_FILE_SIZE + 1) as usize);
        let fs = VirtualFs::new(HashMap::from([("big.mds".to_string(), big)]));
        let err = fs.read("big.mds").unwrap_err();
        assert!(
            matches!(err, MdsError::ResourceLimit { .. }),
            "expected ResourceLimit, got {err:?}"
        );
    }

    #[test]
    fn vfs_read_at_size_limit_ok() {
        // Content exactly at 10 MB should be allowed.
        let exact = "x".repeat(MAX_FILE_SIZE as usize);
        let fs = VirtualFs::new(HashMap::from([("exact.mds".to_string(), exact.clone())]));
        let content = fs.read("exact.mds").unwrap();
        assert_eq!(content.len(), MAX_FILE_SIZE as usize);
    }

    // ── NativeFs null-byte rejection ──────────────────────────────────────────

    #[test]
    fn native_resolve_entry_null_byte_is_io_error() {
        // An entry path is caller input, not an @import string: mds::io.
        let err = NativeFs::new().resolve_entry("./\0evil.mds").unwrap_err();
        assert!(
            matches!(err, MdsError::Io { .. }),
            "expected Io, got {err:?}"
        );
        assert_eq!(code_of(&err).as_deref(), Some("mds::io"));
        let msg = err.to_string();
        // The path is shown WIRE-escaped: the escape is present, the raw NUL is not.
        let escaped = format!("./\\u{:04X}evil.mds", 0);
        assert!(
            msg.contains("null byte") && msg.contains(&escaped),
            "expected {escaped:?} in the message, got: {msg}"
        );
        assert!(!msg.contains('\0'), "raw NUL must not reach the message");
    }

    // ── Forbidden path characters (#265) ──────────────────────────────────────

    /// Every forbidden codepoint, with a non-vacuity pin on the class size.
    fn forbidden_chars() -> Vec<char> {
        let all: Vec<char> = (0..=0x10_FFFF_u32)
            .filter_map(char::from_u32)
            .filter(|&c| crate::lint::is_forbidden_path_char(c))
            .collect();
        assert_eq!(all.len(), 80, "non-vacuity: the class is 80 codepoints");
        all
    }

    /// `(U+XXXX, six-char escape text)` for `ch`, built at runtime (PF-018).
    fn names(ch: char) -> (String, String) {
        (
            format!("U+{:04X}", u32::from(ch)),
            format!("\\u{:04X}", u32::from(ch)),
        )
    }

    fn assert_no_forbidden(msg: &str) {
        assert!(
            first_forbidden_char(msg).is_none(),
            "message carries a forbidden char: {msg:?}"
        );
    }

    /// A direct `normalize_in_dir` call refuses all 80 in the import string on both
    /// backends, before the filesystem is touched; NUL keeps its own message.
    #[test]
    fn normalize_in_dir_refuses_every_forbidden_char() {
        let dir = TempDir::new().unwrap();
        let native_dir = dir.path().display().to_string();
        let native = NativeFs::new();
        for ch in forbidden_chars() {
            let (u, esc) = names(ch);
            let relative = format!("./a{ch}.mds");
            let expected = if ch == '\0' {
                "import path contains null byte".to_string()
            } else {
                format!("import path contains forbidden character {u}: \"./a{esc}.mds\"")
            };
            for (backend, result) in [
                ("vfs", vfs().normalize_in_dir("", &relative)),
                ("native", native.normalize_in_dir(&native_dir, &relative)),
            ] {
                let err = result.unwrap_err();
                assert_eq!(
                    code_of(&err).as_deref(),
                    Some("mds::import"),
                    "{backend} {u}"
                );
                let msg = err.to_string();
                assert!(msg.contains(&expected), "{backend} {u}: {msg}");
                assert_no_forbidden(&msg);
            }
        }
        // Control: a clean name resolves on both.
        assert_eq!(vfs().normalize_in_dir("", "./a.mds").unwrap(), "a.mds");
        make_temp_file(&dir, "a.mds", "x");
        assert!(native.normalize_in_dir(&native_dir, "./a.mds").is_ok());
    }

    /// A direct `resolve_entry` call refuses all 80 with `mds::io` on both backends.
    #[test]
    fn resolve_entry_refuses_every_forbidden_char() {
        for ch in forbidden_chars() {
            let (u, esc) = names(ch);
            let path = format!("./main{ch}.mds");
            let expected = if ch == '\0' {
                format!("entry path contains null byte: \"./main{esc}.mds\"")
            } else {
                format!("entry path contains forbidden character {u}: \"./main{esc}.mds\"")
            };
            for (backend, result) in [
                ("vfs", vfs().resolve_entry(&path)),
                ("native", NativeFs::new().resolve_entry(&path)),
            ] {
                let err = result.unwrap_err();
                assert!(matches!(err, MdsError::Io { .. }), "{backend} {u}: {err:?}");
                let msg = err.to_string();
                assert!(msg.contains(&expected), "{backend} {u}: {msg}");
                assert_no_forbidden(&msg);
            }
        }
    }

    /// `anchor_base_dir` refuses a typed base directory carrying a forbidden
    /// character before it touches the filesystem (runs on every platform).
    #[test]
    fn native_anchor_base_dir_refuses_forbidden_chars() {
        for ch in ['\x1b', '\t', '\n', '\u{202E}'] {
            let (u, esc) = names(ch);
            let err = NativeFs::new()
                .anchor_base_dir(&format!("base{ch}dir"))
                .unwrap_err();
            assert_eq!(code_of(&err).as_deref(), Some("mds::io"), "{u}");
            let msg = err.to_string();
            assert!(
                msg.contains(&format!(
                    "base directory contains forbidden character {u}: \"base{esc}dir\""
                )),
                "{msg}"
            );
            assert_no_forbidden(&msg);
        }
    }

    /// The canonical-path scan. Unix-only: a C0 control cannot appear in a Windows
    /// file name, so the hostile directory cannot be created there.
    #[cfg(unix)]
    #[test]
    fn native_canonical_path_through_a_hostile_directory_is_refused() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let hostile = root.join("evil\ndir");
        let clean = root.join("clean");
        for d in [&hostile, &clean] {
            std::fs::create_dir(d).unwrap();
            std::fs::create_dir(d.join("sub")).unwrap();
            std::fs::write(d.join("x.mds"), "x").unwrap();
        }
        assert!(make_symlink(&hostile, &root.join("alias")));
        assert!(make_symlink(&clean, &root.join("alias2")));
        let escaped_lf = format!("\\u{:04X}", 0x0A);

        // anchor_base_dir below the alias (a symlinked FINAL component is refused
        // as a symlink): the typed form is clean, the canonical one is not. The
        // message names what was passed, not the canonical path.
        let alias = root.join("alias").join("sub").display().to_string();
        let err = NativeFs::new().anchor_base_dir(&alias).unwrap_err();
        assert_eq!(code_of(&err).as_deref(), Some("mds::io"));
        let msg = err.to_string();
        assert!(
            msg.contains(&format!(
                "resolved path contains forbidden character U+000A: \"{alias}\""
            )),
            "{msg}"
        );
        assert!(
            !msg.contains(&escaped_lf),
            "canonical path must not be shown: {msg}"
        );

        // normalize_in_dir through the alias, and check_symlink (the CLI's reader).
        let fs = NativeFs::new();
        let err = fs
            .normalize_in_dir(&root.display().to_string(), "./alias/x.mds")
            .unwrap_err();
        assert_eq!(code_of(&err).as_deref(), Some("mds::io"));
        assert!(
            err.to_string()
                .contains("resolved path contains forbidden character U+000A: \"./alias/x.mds\""),
            "{err}"
        );
        let err = NativeFs::check_symlink(&root.join("alias").join("x.mds")).unwrap_err();
        assert_eq!(code_of(&err).as_deref(), Some("mds::io"));

        // Controls: the clean alias resolves on every path (a fresh NativeFs each,
        // so one anchored root does not contain the next).
        assert!(NativeFs::new()
            .anchor_base_dir(&root.join("alias2").join("sub").display().to_string())
            .is_ok());
        assert!(NativeFs::new()
            .normalize_in_dir(&root.display().to_string(), "./alias2/x.mds")
            .is_ok());
        assert!(NativeFs::check_symlink(&root.join("alias2").join("x.mds")).is_ok());
    }

    // ── Non-UTF-8 resolved paths: refused, never keyed lossily ────────────────

    /// A resolved path that is not valid UTF-8 has no exact string key. Its lossy
    /// form replaces each invalid sequence with U+FFFD and so names a DIFFERENT
    /// path, which a later `read` would open with none of the checks the real path
    /// passed. `key_of` refuses it instead (`mds::io`), naming the path as written.
    ///
    /// `#[cfg(unix)]`: builds the non-UTF-8 path with `OsStringExt` (arbitrary bytes),
    /// a Unix-only API; Windows paths are UTF-16. It is only built in memory, so it
    /// runs on macOS too, whose filesystem refuses such a name.
    #[cfg(unix)]
    #[test]
    fn key_of_refuses_a_non_utf8_resolved_path() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let resolved = PathBuf::from(OsString::from_vec(b"/p/sub\xFF/x.mds".to_vec()));
        let err = key_of(&resolved, "./link/x.mds").expect_err("a non-UTF-8 path has no key");
        assert_eq!(code_of(&err).as_deref(), Some("mds::io"));
        assert_eq!(
            err.to_string(),
            "resolved path is not valid UTF-8: \"./link/x.mds\""
        );

        // Control: a UTF-8 path is its own key, unchanged.
        assert_eq!(
            key_of(Path::new("/p/sub/x.mds"), "./x.mds").unwrap(),
            "/p/sub/x.mds"
        );
    }

    /// End to end: an entry, import or base directory reached through a symlink into
    /// a directory whose name is not valid UTF-8 is refused — where a lossy key would
    /// have opened the valid-UTF-8 "twin", the same name with U+FFFD, unchecked. In
    /// an untrusted repository the twin's file can be a symlink to anything.
    ///
    /// `#[cfg(unix)]`: builds the non-UTF-8 name with `OsStringExt` (arbitrary bytes),
    /// a Unix-only API. macOS (APFS / HFS+) refuses the name, so the on-disk half is
    /// a Linux-CI gate; `key_of_refuses_a_non_utf8_resolved_path` covers the refusal
    /// on every unix host.
    #[cfg(unix)]
    #[test]
    fn native_non_utf8_resolved_path_is_refused_not_read_as_its_twin() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let hostile = root.join(OsString::from_vec(b"sub\xFF".to_vec()));
        if std::fs::create_dir(&hostile).is_err() {
            // Any unix filesystem other than macOS's must accept the name and reach
            // the assertions below — never a silent skip there.
            #[cfg(not(target_os = "macos"))]
            panic!("a non-UTF-8 directory name was rejected by the filesystem");
            #[cfg(target_os = "macos")]
            return;
        }
        std::fs::create_dir(hostile.join("sub")).unwrap();
        std::fs::write(hostile.join("x.mds"), "real\n").unwrap();
        assert!(make_symlink(&hostile, &root.join("link")));
        // The twin the lossy key names, holding a different file.
        let twin = root.join(format!("sub{}", char::REPLACEMENT_CHARACTER));
        std::fs::create_dir(&twin).unwrap();
        std::fs::create_dir(twin.join("sub")).unwrap();
        std::fs::write(twin.join("x.mds"), "twin\n").unwrap();
        let root_str = root.display().to_string();

        let assert_refused = |err: MdsError, shown: &str| {
            assert_eq!(code_of(&err).as_deref(), Some("mds::io"), "{err}");
            assert_eq!(
                err.to_string(),
                format!("resolved path is not valid UTF-8: \"{shown}\"")
            );
        };

        let err = NativeFs::new()
            .normalize_in_dir(&root_str, "./link/x.mds")
            .expect_err("an import through the link must be refused, not read as its twin");
        assert_refused(err, "./link/x.mds");

        let entry = root.join("link").join("x.mds").display().to_string();
        let err = NativeFs::new()
            .resolve_entry(&entry)
            .expect_err("an entry through the link must be refused");
        assert_refused(err, &entry);

        let base = root.join("link").join("sub").display().to_string();
        let fs = NativeFs::new();
        let err = fs
            .anchor_base_dir(&base)
            .expect_err("a base directory through the link must be refused");
        assert_refused(err, &base);
        assert_eq!(
            fs.source_root(),
            None,
            "a refused base must not anchor the root"
        );

        // Control: the twin, named directly, resolves and reads as itself — the file
        // a lossy key would have read in place of the real one.
        let fs = NativeFs::new();
        let key = fs
            .normalize_in_dir(
                &root_str,
                &format!("./sub{}/x.mds", char::REPLACEMENT_CHARACTER),
            )
            .unwrap();
        assert_eq!(fs.read(&key).unwrap(), "twin\n");
    }

    // ── is_markdown consistency ───────────────────────────────────────────────

    #[test]
    fn vfs_is_markdown_path_extension_precision() {
        // "foo.cmd" ends with "md" but extension is "cmd" — must not match.
        assert!(!vfs().is_markdown("script.cmd"));
    }

    #[test]
    fn vfs_is_markdown_matches_native_behavior() {
        // Both implementations should agree on the same key.
        let native = NativeFs::new();
        let virt = vfs();
        for key in ["readme.md", "main.mds", "no_ext", "script.cmd", "a/b/c.md"] {
            assert_eq!(
                virt.is_markdown(key),
                native.is_markdown(key),
                "is_markdown disagreement on key: {key}"
            );
        }
    }

    // ── NativeFs::read size check (TOCTOU-safe post-read) ────────────────────

    #[test]
    fn native_read_rejects_large_file() {
        let dir = TempDir::new().unwrap();
        // Write a file just over the limit.
        let big_content = "x".repeat((MAX_FILE_SIZE + 1) as usize);
        let path = dir.path().join("big.mds");
        std::fs::write(&path, big_content.as_bytes()).unwrap();

        let fs = NativeFs::new();
        let key = path.display().to_string();
        let err = fs.read(&key).unwrap_err();
        assert!(
            matches!(err, MdsError::ResourceLimit { .. }),
            "expected ResourceLimit for oversized file, got {err:?}"
        );
    }

    // ── NativeFs empty-path guards ────────────────────────────────────────────

    #[test]
    fn native_resolve_entry_empty_path_is_io_error() {
        let err = NativeFs::new().resolve_entry("").unwrap_err();
        assert!(
            matches!(err, MdsError::Io { .. }),
            "expected Io, got {err:?}"
        );
        assert_eq!(code_of(&err).as_deref(), Some("mds::io"));
        assert!(
            err.to_string().contains("entry path is empty"),
            "got: {err}"
        );
    }

    #[test]
    fn native_normalize_in_dir_empty_relative_errors() {
        let dir = TempDir::new().unwrap();
        let file = make_temp_file(&dir, "main.mds", "hello");
        let fs = NativeFs::new();
        let base_key = fs
            .resolve_entry(&file.display().to_string())
            .expect("resolve_entry");
        let result = fs.normalize_in_dir(&fs.parent_dir(&base_key), "");
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("empty"),
            "expected 'empty' in error for empty import, got: {msg}"
        );
    }

    // ── FileSystem::anchor_base_dir ───────────────────────────────────────────

    #[test]
    fn vfs_anchor_base_dir_is_identity() {
        // VirtualFs inherits the default implementation — returns dir unchanged.
        let dir = "some/virtual/dir";
        assert_eq!(
            vfs().anchor_base_dir(dir).unwrap(),
            dir,
            "VirtualFs anchor_base_dir should be identity"
        );
        assert_eq!(vfs().anchor_base_dir("").unwrap(), "", "the key-space root");
    }

    #[test]
    fn native_anchor_base_dir_resolves_real_dir() {
        // NativeFs resolves a real directory to its canonical absolute path and
        // anchors the root.
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("real");
        std::fs::create_dir(&sub).unwrap();
        let fs = NativeFs::new();
        let canonical = fs
            .anchor_base_dir(&sub.display().to_string())
            .expect("anchor_base_dir should succeed for a real directory");
        assert_eq!(Path::new(&canonical), sub.canonicalize().unwrap());
        assert!(
            fs.source_root().is_some(),
            "the first anchor establishes the root"
        );
    }

    #[test]
    fn native_anchor_base_dir_nonexistent_is_io_error() {
        let fs = NativeFs::new();
        let err = fs
            .anchor_base_dir("/nonexistent/path/does/not/exist")
            .unwrap_err();
        assert!(
            matches!(err, MdsError::Io { .. }),
            "expected Io error for nonexistent path, got: {err:?}"
        );
        assert_eq!(fs.source_root(), None, "a failed anchor anchors nothing");
    }

    #[test]
    fn native_anchor_base_dir_root_returns_root_not_cwd() {
        // #371 cwd trap regression: anchoring a filesystem root must anchor AT
        // the root, never silently fall back to the current working directory.
        // A naive fix that funneled the root case through `effective_parent`
        // (which maps an absent/empty parent to ".") would reintroduce exactly
        // this bug.
        //
        // The root is obtained portably -- the topmost ancestor of a real
        // tempdir path -- so this runs unchanged on the Windows CI leg (a
        // drive root there, `/` on Unix), never a hardcoded "/".
        //
        // The test process's cwd during `cargo test`/nextest is the crate
        // directory, never the filesystem root, so this naturally exercises
        // "cwd != root" without mutating global process state (which would
        // race other tests running in parallel).
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().ancestors().last().unwrap().to_path_buf();
        let cwd = std::env::current_dir().unwrap();
        assert_ne!(cwd, root, "test assumption: cwd is not the filesystem root");

        let fs = NativeFs::new();
        let canonical = fs
            .anchor_base_dir(&root.display().to_string())
            .expect("anchor_base_dir should succeed for a filesystem root");

        // Compare against the CANONICAL root, not the literal one: `canonical_dir`'s
        // root branch (used by `anchor_base_dir`) calls `Path::canonicalize`, and on
        // Windows `std::fs::canonicalize` always returns the verbatim form
        // (`\\?\C:\`) even for a root that was already absolute (`C:\`). That is the
        // same, uniform contract the non-root branch has (see
        // `native_anchor_base_dir_resolves_real_dir` above, which compares against
        // `sub.canonicalize()`) — containment elsewhere compares verbatim paths
        // against each other, so the verbatim form is the correct one to assert.
        let expected = root.canonicalize().unwrap();
        assert_eq!(
            Path::new(&canonical),
            expected,
            "anchor_base_dir(root) must return the canonical root itself, got: {canonical}"
        );
        // Structural check, independent of the exact string form on any platform:
        // the result has a root and no parent, i.e. it IS a filesystem root.
        assert!(
            Path::new(&canonical).has_root() && Path::new(&canonical).parent().is_none(),
            "anchor_base_dir(root) must return a filesystem root, got: {canonical}"
        );
        // Canonicalize cwd too, so this compares like with like: a non-canonical cwd
        // and a canonical (possibly verbatim) root are never equal even when the test
        // fails to reject a cwd fallback, which would make this assertion vacuous.
        assert_ne!(
            Path::new(&canonical),
            cwd.canonicalize().unwrap(),
            "anchor_base_dir(root) must not resolve to cwd, got: {canonical}"
        );
    }

    #[test]
    fn native_anchor_base_dir_symlink_rejected() {
        // Security boundary: a symlinked base directory is refused BEFORE it can
        // anchor the security root at an arbitrary location (#21), so a failed
        // check leaves no root behind.
        let real_dir = TempDir::new().unwrap();
        let link_parent = TempDir::new().unwrap();
        let link_path = link_parent.path().join("link_to_dir");
        if !make_symlink(real_dir.path(), &link_path) {
            return;
        }

        let fs = NativeFs::new();
        let err = fs
            .anchor_base_dir(&link_path.display().to_string())
            .unwrap_err();
        assert!(
            matches!(err, MdsError::ImportError { .. }) && err.to_string().contains("symlinks"),
            "expected a symlink rejection, got: {err:?}"
        );
        assert_eq!(fs.source_root(), None, "the root must not be anchored");

        // Control: the link's real target is accepted and anchors the root.
        fs.anchor_base_dir(&real_dir.path().display().to_string())
            .expect("control: the real directory anchors");
        assert!(fs.source_root().is_some());
    }

    /// The base directory handed to `ModuleCache::resolve_source*` goes through
    /// `anchor_base_dir`, so a symlinked one is refused there. The string-compile
    /// functions canonicalize their base directory first (`resolve_base_dir`), so
    /// the same symlink is followed and the compile succeeds — spec §4.6
    /// "Symlink rejection" states exactly this split.
    #[test]
    fn symlinked_base_dir_refused_by_resolve_source_followed_by_string_api() {
        let real_dir = TempDir::new().unwrap();
        make_temp_file(&real_dir, "lib.mds", "@define hi():\nHi\n@end\n");
        let link_parent = TempDir::new().unwrap();
        let link = link_parent.path().join("link_to_dir");
        if !make_symlink(real_dir.path(), &link) {
            return;
        }
        let source = "@import \"./lib.mds\" as lib\n{{lib.hi()}}\n";

        let mut cache = crate::resolver::ModuleCache::native();
        let err = cache
            .resolve_source_intrinsic(source, link.to_str().unwrap(), &HashMap::new(), &mut vec![])
            .unwrap_err();
        assert!(
            err.to_string().contains("symlinks are not allowed"),
            "resolve_source_intrinsic must refuse a symlinked base dir, got: {err}"
        );

        let out = crate::compile_str_with(source, Some(&link), None)
            .expect("the string API follows a symlinked base dir");
        assert_eq!(out.into_markdown().unwrap(), "Hi\n");
    }

    // ── VirtualFs segment limit ───────────────────────────────────────────────

    #[test]
    fn vfs_normalize_in_dir_exactly_at_segment_limit_ok() {
        // Exactly MAX_PATH_SEGMENTS segments must succeed. An import from the
        // top-level "root.mds" resolves from the key-space root (parent_dir is
        // ""), so the relative path carries every segment.
        let fs = vfs();
        let result = fs.normalize_in_dir(&fs.parent_dir("root.mds"), &segments(MAX_PATH_SEGMENTS));
        assert!(
            result.is_ok(),
            "expected Ok for path at segment limit, got {result:?}"
        );
    }

    // ── VirtualFs::parent_dir ─────────────────────────────────────────────────

    #[test]
    fn vfs_parent_dir_nested() {
        assert_eq!(vfs().parent_dir("a/b/c.mds"), "a/b");
    }

    #[test]
    fn vfs_parent_dir_top_level() {
        // A top-level key has no directory component → empty string.
        assert_eq!(vfs().parent_dir("main.mds"), "");
    }

    #[test]
    fn vfs_parent_dir_single_slash() {
        assert_eq!(vfs().parent_dir("a/b.mds"), "a");
    }

    // ── VirtualFs::normalize_in_dir ───────────────────────────────────────────

    #[test]
    fn vfs_normalize_in_dir_sibling() {
        let result = vfs().normalize_in_dir("components", "./footer.mds");
        assert_eq!(result.unwrap(), "components/footer.mds");
    }

    #[test]
    fn vfs_normalize_in_dir_parent_traversal() {
        let result = vfs().normalize_in_dir("a/b", "../sibling.mds");
        assert_eq!(result.unwrap(), "a/sibling.mds");
    }

    #[test]
    fn vfs_normalize_in_dir_traversal_escape_errors() {
        // Traversal above root must be rejected.
        let result = vfs().normalize_in_dir("", "../escape.mds");
        assert!(
            result.is_err(),
            "expected Err for traversal above root: {result:?}"
        );
    }

    #[test]
    fn vfs_normalize_in_dir_empty_key_errors() {
        // Climbing back to the key-space root without naming a file resolves to
        // no key at all, which must be refused rather than read as "".
        let err = vfs().normalize_in_dir("a", "../").unwrap_err();
        assert!(
            matches!(err, MdsError::ImportError { .. }),
            "expected ImportError, got {err:?}"
        );
        assert!(
            err.to_string().contains("resolves to empty key"),
            "got: {err}"
        );
        // Control: the same climb that names a file resolves.
        assert_eq!(vfs().normalize_in_dir("a", "../b.mds").unwrap(), "b.mds");
    }

    #[test]
    fn vfs_normalize_in_dir_null_byte_errors() {
        let result = vfs().normalize_in_dir("a", "./\0evil.mds");
        assert!(result.is_err(), "expected Err for null byte in path");
    }

    #[test]
    fn vfs_normalize_in_dir_empty_relative_errors() {
        let result = vfs().normalize_in_dir("a", "");
        assert!(result.is_err(), "expected Err for empty relative path");
    }

    #[test]
    fn vfs_normalize_in_dir_segment_cap_errors() {
        // MAX_PATH_SEGMENTS + 1 segments in relative must be rejected.
        let long_relative = (0..=MAX_PATH_SEGMENTS)
            .map(|i| format!("seg{i}"))
            .collect::<Vec<_>>()
            .join("/");
        let result = vfs().normalize_in_dir("", &long_relative);
        let err = result.unwrap_err();
        assert!(
            matches!(err, MdsError::ResourceLimit { .. }),
            "expected ResourceLimit, got {err:?}"
        );
    }

    #[test]
    fn vfs_normalize_in_dir_empty_dir_is_root() {
        // dir == "" means root; sibling resolves as a top-level key.
        let result = vfs().normalize_in_dir("", "./main.mds");
        assert_eq!(result.unwrap(), "main.mds");
    }

    // ── NativeFs::parent_dir ──────────────────────────────────────────────────

    #[test]
    fn native_parent_dir_normal_path() {
        let dir = TempDir::new().unwrap();
        let file = make_temp_file(&dir, "main.mds", "hello");
        let key = file.display().to_string();
        let parent = NativeFs::new().parent_dir(&key);
        assert!(
            parent.contains(dir.path().to_str().unwrap()),
            "parent_dir should contain the temp dir path, got: {parent}"
        );
    }

    // ── NativeFs::normalize_in_dir ────────────────────────────────────────────

    #[test]
    fn native_normalize_in_dir_sibling_resolve() {
        let dir = TempDir::new().unwrap();
        make_temp_file(&dir, "main.mds", "hello");
        make_temp_file(&dir, "sibling.mds", "world");

        let fs = NativeFs::new();
        // Establish root first.
        let entry = dir.path().join("main.mds").display().to_string();
        fs.resolve_entry(&entry).expect("resolve_entry");

        let dir_str = dir.path().display().to_string();
        let result = fs.normalize_in_dir(&dir_str, "./sibling.mds");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        let key = result.unwrap();
        assert!(
            key.contains("sibling.mds"),
            "expected sibling.mds in key, got: {key}"
        );
    }

    #[test]
    fn native_normalize_in_dir_traversal_rejected() {
        // Security boundary: a relative `../` sequence that escapes the project
        // root must be rejected with "escapes project directory".
        let project_dir = TempDir::new().unwrap();
        let outside_dir = TempDir::new().unwrap();

        let entry = make_temp_file(&project_dir, "main.mds", "hello");
        // Place a real file outside so canonicalization has a target to resolve.
        let outside = make_temp_file(&outside_dir, "secret.mds", "secret");

        let fs = NativeFs::new();
        fs.resolve_entry(&entry.display().to_string())
            .expect("resolve_entry");

        let dir_str = project_dir.path().display().to_string();
        let result = fs.normalize_in_dir(&dir_str, &escape_to(&outside));
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("escapes project"),
            "expected 'escapes project' in error, got: {msg}"
        );

        // Control: the same vector shape aimed at a file INSIDE the project
        // resolves to it, proving the traversal lands on its target and that the
        // rejection above comes from containment, not from a malformed path.
        let inside = fs
            .normalize_in_dir(&dir_str, &escape_to(&entry))
            .expect("control: escape_to(entry) must resolve inside the project");
        assert_eq!(Path::new(&inside), entry.canonicalize().unwrap());
    }

    #[test]
    fn native_normalize_in_dir_null_byte_errors() {
        let dir = TempDir::new().unwrap();
        let dir_str = dir.path().display().to_string();
        let fs = NativeFs::new();
        let result = fs.normalize_in_dir(&dir_str, "./\0evil.mds");
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("null byte"),
            "expected null byte in error, got: {msg}"
        );
    }

    // ── External impl: clean-break contract for custom FileSystem ─────────────
    //
    // Verifies that a minimal custom FileSystem implementation satisfying all
    // REQUIRED trait methods compiles and resolves an import correctly via
    // `ModuleCache::with_fs`. Locks the D1 clean-break contract: zero external
    // impls exist today, and the next release will be ≥0.4.0.

    struct TestFs {
        modules: std::collections::HashMap<String, String>,
    }

    impl TestFs {
        fn new(modules: std::collections::HashMap<String, String>) -> Self {
            Self { modules }
        }
    }

    impl super::FileSystem for TestFs {
        fn resolve_entry(&self, path: &str) -> Result<String, MdsError> {
            Ok(path.to_string())
        }

        fn normalize_in_dir(&self, dir: &str, relative: &str) -> Result<String, MdsError> {
            if relative.is_empty() {
                return Err(MdsError::import_error("empty path"));
            }
            let relative = relative.trim_start_matches("./");
            if dir.is_empty() {
                return Ok(relative.to_string());
            }
            Ok(format!("{dir}/{relative}"))
        }

        fn parent_dir(&self, key: &str) -> String {
            key.rsplit_once('/')
                .map(|(d, _)| d)
                .unwrap_or("")
                .to_string()
        }

        fn read(&self, normalized: &str) -> Result<String, MdsError> {
            self.modules
                .get(normalized)
                .cloned()
                .ok_or_else(|| MdsError::file_not_found(normalized.to_string()))
        }

        fn is_markdown(&self, normalized: &str) -> bool {
            Path::new(normalized).extension().and_then(|e| e.to_str()) == Some("md")
        }
    }

    // ── effective_parent ──────────────────────────────────────────────────────

    #[test]
    fn effective_parent_bare_name_returns_dot() {
        // "hello.mds" — no directory component; Path::parent() returns Some("").
        // effective_parent must return Path::new("."), not the empty path.
        assert_eq!(effective_parent(Path::new("hello.mds")), Path::new("."));
    }

    #[test]
    fn effective_parent_dot_slash_prefix_returns_dot() {
        // "./hello.mds" — parent is "." (non-empty); returned as-is.
        assert_eq!(effective_parent(Path::new("./hello.mds")), Path::new("."));
    }

    #[test]
    fn effective_parent_subdir_path_unchanged() {
        // "sub/hello.mds" — parent is "sub"; returned unchanged.
        assert_eq!(
            effective_parent(Path::new("sub/hello.mds")),
            Path::new("sub")
        );
    }

    #[test]
    fn effective_parent_absolute_path_unchanged() {
        // Absolute path: parent is the directory, which is non-empty.
        let p = Path::new("/tmp/hello.mds");
        assert_eq!(effective_parent(p), Path::new("/tmp"));
    }

    // ── check_symlink unit tests (absolute paths) ─────────────────────────────
    //
    // Note: bare-filename (PF-006) integration testing lives at CLI level in
    // cli_build::build_load_config_finds_grandparent_mds_json,
    // cli_fmt::fmt_bare_filename_propagates_syntax_error, and
    // cli_lint::lint_fix_bare_filename_applies_fix.

    #[test]
    fn check_symlink_real_absolute_file_is_accepted() {
        // A real file reached via an absolute path must succeed.
        // Uses an absolute path to avoid mutating std::env::current_dir (process-global,
        // races under nextest); this is the same code path that effective_parent enables
        // for a bare filename resolved from cwd.
        let dir = TempDir::new().unwrap();
        let file = make_temp_file(&dir, "bare.mds", "hello");
        let result = NativeFs::check_symlink(&file);
        assert!(
            result.is_ok(),
            "check_symlink should succeed for a real absolute-path file: {result:?}"
        );
    }

    #[test]
    fn check_symlink_symlinked_file_is_rejected() {
        // A symlinked file must be rejected, regardless of whether it is reached
        // via a bare name or an absolute path.
        let dir = TempDir::new().unwrap();
        let target = make_temp_file(&dir, "target.mds", "hello");
        let link_path = dir.path().join("link.mds");
        if !make_symlink(&target, &link_path) {
            return;
        }
        let result = NativeFs::check_symlink(&link_path);
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("symlinks"),
            "expected symlink rejection, got: {msg}"
        );
    }

    #[test]
    fn external_impl_resolves_import_via_with_fs() {
        use crate::resolver::ModuleCache;
        let modules = std::collections::HashMap::from([
            (
                "main.mds".to_string(),
                "@import \"./lib.mds\" as lib\n{{lib.greet(\"World\")}}\n".to_string(),
            ),
            (
                "lib.mds".to_string(),
                "@define greet(x):\nHello {{x}}!\n@end\n".to_string(),
            ),
        ]);
        let fs = Box::new(TestFs::new(modules));
        let mut cache = ModuleCache::with_fs(fs);
        let mut warnings = vec![];
        let result = cache.resolve_key("main.mds", &Default::default(), &mut warnings);
        assert!(
            result.is_ok(),
            "custom FileSystem impl should resolve imports: {result:?}"
        );
        let resolved = result.unwrap();
        let output = resolved.prompt_body.as_deref().unwrap_or("");
        assert!(
            output.contains("Hello World!"),
            "expected 'Hello World!' from custom fs import, got: {output}"
        );
    }

    // ── ModuleCache::resolve_key on NativeFs (#155) ───────────────────────────

    /// A project directory with a `.mdsroot` marker (so the root walk-up stops
    /// there) holding `main.mds` with `content`. Returns the guard and the
    /// canonical project directory.
    fn marked_project(content: &str) -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join(".mdsroot"), "").unwrap();
        std::fs::write(root.join("main.mds"), content).unwrap();
        (dir, root)
    }

    fn resolve_key_native(key: &Path) -> Result<Option<String>, MdsError> {
        let mut cache = crate::resolver::ModuleCache::native();
        cache
            .resolve_key(key.to_str().unwrap(), &HashMap::new(), &mut vec![])
            .map(|m| m.prompt_body.clone())
    }

    #[test]
    fn native_resolve_key_plain_entry_resolves() {
        let (_guard, root) = marked_project("@import \"./lib.mds\" as lib\n{{lib.hi()}}\n");
        std::fs::write(root.join("lib.mds"), "@define hi():\nHello!\n@end\n").unwrap();
        let body = resolve_key_native(&root.join("main.mds"))
            .expect("a plain entry key resolves")
            .unwrap_or_default();
        assert!(body.contains("Hello!"), "got: {body}");
    }

    #[test]
    fn native_resolve_key_rejects_symlinked_entry() {
        let (_guard, root) = marked_project("Hello!\n");
        let link = root.join("link.mds");
        if !make_symlink(&root.join("main.mds"), &link) {
            return;
        }
        let err = resolve_key_native(&link).unwrap_err();
        assert!(
            err.to_string().contains("symlinks are not allowed"),
            "a symlinked entry key must be refused, got: {err}"
        );
    }

    #[test]
    fn native_resolve_key_anchors_the_root_for_imports() {
        // The entry key anchors the project root, so an import that climbs out of
        // it is refused by containment.
        let outside_dir = TempDir::new().unwrap();
        let outside = make_temp_file(&outside_dir, "secret.mds", "secret\n");
        let (_guard, root) = marked_project(&format!("@import \"{}\" as s\n", escape_to(&outside)));
        let err = resolve_key_native(&root.join("main.mds")).unwrap_err();
        assert!(
            err.to_string().contains("escapes project directory"),
            "an import escaping the anchored root must be refused, got: {err}"
        );
    }

    #[test]
    fn native_resolve_key_outside_anchored_root_rejected() {
        // Once a key has anchored the root, a later `..` key that leaves it is
        // refused — the same rule `resolve_path` applies to a second entry.
        let (_guard, root) = marked_project("Hello!\n");
        let outside_dir = TempDir::new().unwrap();
        make_temp_file(&outside_dir, "outside.mds", "outside\n");
        let outside_name = outside_dir.path().file_name().unwrap();
        let escaping = root.join("..").join(outside_name).join("outside.mds");

        let mut cache = crate::resolver::ModuleCache::native();
        cache
            .resolve_key(
                root.join("main.mds").to_str().unwrap(),
                &HashMap::new(),
                &mut vec![],
            )
            .expect("the first key anchors the root");
        let err = cache
            .resolve_key(escaping.to_str().unwrap(), &HashMap::new(), &mut vec![])
            .unwrap_err();
        assert!(
            err.to_string().contains("escapes project directory"),
            "a `..` key leaving the anchored root must be refused, got: {err}"
        );
    }

    // ── #408: a case-mismatched name is not a symlink ─────────────────────────
    //
    // Each test probes the tempdir volume instead of assuming a platform: a
    // case-insensitive volume (default macOS APFS, NTFS on the Windows CI leg)
    // resolves the mismatched spelling, a case-sensitive one (Linux CI) reports
    // it missing. Neither may report a symlink.

    /// Whether the volume holding `dir` resolves names case-insensitively.
    fn case_insensitive(dir: &Path) -> bool {
        let probe = dir.join("probe.txt");
        std::fs::write(&probe, "").unwrap();
        let insensitive = dir.join("PROBE.TXT").exists();
        std::fs::remove_file(&probe).unwrap();
        insensitive
    }

    fn assert_not_a_symlink_error(result: &Result<crate::CompileResult, MdsError>) {
        if let Err(err) = result {
            assert!(
                !err.to_string().contains("symlink"),
                "a case-mismatched name must never be reported as a symlink: {err}"
            );
        }
    }

    #[test]
    fn case_mismatched_import_is_not_a_symlink() {
        let (_guard, root) = marked_project("@import \"./Header.mds\" as h\n{{h.hi()}}\n");
        std::fs::write(root.join("header.mds"), "@define hi():\nHi\n@end\n").unwrap();

        let result = crate::compile_with_deps(root.join("main.mds"), None);
        assert_not_a_symlink_error(&result);
        if case_insensitive(&root) {
            let compiled = result.expect("the OS resolves the mismatched spelling");
            assert_eq!(
                compiled.output,
                crate::CompiledOutput::Markdown("Hi\n".into())
            );
            // The module key is the on-disk spelling, not the typed one.
            let [dep] = compiled.dependencies.as_slice() else {
                panic!("expected one dependency, got {:?}", compiled.dependencies);
            };
            assert_eq!(
                Path::new(dep).file_name().and_then(|n| n.to_str()),
                Some("header.mds")
            );
        } else {
            let err = result.unwrap_err();
            assert!(
                matches!(err, MdsError::FileNotFound { .. }),
                "case-sensitive volume: expected FileNotFound, got {err:?}"
            );
        }
    }

    #[test]
    fn case_mismatched_entry_is_not_a_symlink() {
        let (_guard, root) = marked_project("Hello!\n");
        let typed = root.join("MAIN.mds");

        let result = crate::compile_with_deps(&typed, None);
        assert_not_a_symlink_error(&result);
        let key = NativeFs::new().resolve_entry(typed.to_str().unwrap());
        if case_insensitive(&root) {
            assert_eq!(
                result
                    .expect("the OS resolves the mismatched spelling")
                    .output,
                crate::CompiledOutput::Markdown("Hello!\n".into())
            );
            // The entry key is the on-disk spelling too.
            assert_eq!(Path::new(&key.unwrap()), root.join("main.mds"));
        } else {
            let err = result.unwrap_err();
            assert!(
                matches!(err, MdsError::FileNotFound { .. }),
                "case-sensitive volume: expected FileNotFound, got {err:?}"
            );
            assert!(matches!(key, Err(MdsError::FileNotFound { .. })));
        }
    }

    /// Every spelling of one file shares its on-disk key, so a module imported
    /// under two spellings (twice from one file, and again through a diamond) is
    /// loaded once — no duplicate dependency and no false cycle.
    #[test]
    fn case_variant_imports_share_one_module() {
        let (_guard, root) = marked_project(
            "@import \"./Header.mds\" as a\n\
             @import \"./header.mds\" as b\n\
             @import \"./footer.mds\" as f\n\
             {{a.hi()}}{{b.hi()}}{{f.bye()}}\n",
        );
        if !case_insensitive(&root) {
            eprintln!("skipping: two spellings name two files on a case-sensitive volume");
            return;
        }
        std::fs::write(root.join("header.mds"), "@define hi():\nHi\n@end\n").unwrap();
        std::fs::write(
            root.join("footer.mds"),
            "@import \"./HEADER.mds\" as h\n@define bye():\n{{h.hi()}} bye\n@end\n",
        )
        .unwrap();

        let compiled = crate::compile_with_deps(root.join("main.mds"), None)
            .expect("one module under several spellings compiles");
        let names: Vec<_> = compiled
            .dependencies
            .iter()
            .map(|d| {
                Path::new(d)
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(
            names,
            ["header.mds", "footer.mds"],
            "each file once, under its on-disk spelling"
        );
    }

    /// Control: the file-type check reads the final component as written, so a
    /// symlink is refused under any spelling of its own name.
    #[test]
    fn case_mismatched_symlink_is_still_refused() {
        let (_guard, root) = marked_project("@import \"./LINK.mds\" as l\n");
        std::fs::write(root.join("target.mds"), "@define hi():\nHi\n@end\n").unwrap();
        if !make_symlink(&root.join("target.mds"), &root.join("link.mds")) {
            return;
        }
        let err = crate::compile(root.join("main.mds"), None).unwrap_err();
        if case_insensitive(&root) {
            assert!(
                err.to_string().contains("symlinks are not allowed"),
                "a mismatched spelling of a symlink must still be refused, got: {err}"
            );
        } else {
            assert!(
                matches!(err, MdsError::FileNotFound { .. }),
                "case-sensitive volume: expected FileNotFound, got {err:?}"
            );
        }
        // Exact spelling: refused on every volume.
        std::fs::write(root.join("main.mds"), "@import \"./link.mds\" as l\n").unwrap();
        let err = crate::compile(root.join("main.mds"), None).unwrap_err();
        assert!(
            err.to_string().contains("symlinks are not allowed"),
            "got: {err}"
        );
    }

    /// On Windows `is_symlink` covers every name-surrogate reparse point, so a
    /// junction — which needs no privilege to create — is refused like a
    /// symbolic link.
    #[cfg(windows)]
    #[test]
    fn junction_final_component_is_refused() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let junction = dir.path().join("junction");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .status()
            .unwrap();
        assert!(status.success(), "mklink /J failed");

        let err = NativeFs::check_symlink(&junction).unwrap_err();
        assert!(
            err.to_string().contains("symlinks are not allowed"),
            "a junction must be refused, got: {err}"
        );
        // Control: the junction's target directory is accepted.
        NativeFs::check_symlink(&target).expect("control: a plain directory");
    }

    // ── source_root ───────────────────────────────────────────────────────────

    #[test]
    fn native_source_root_none_before_any_resolve_entry() {
        // Before resolve_entry() or anchor_base_dir() is called, root has not been established.
        let fs = NativeFs::new();
        assert_eq!(
            fs.source_root(),
            None,
            "source_root() must be None before any resolve_entry call"
        );
    }

    #[test]
    fn native_source_root_set_after_resolve_entry() {
        // After the first resolve_entry() call the root is established and
        // source_root() returns Some.
        let dir = TempDir::new().unwrap();
        let file = make_temp_file(&dir, "main.mds", "hello");
        let fs = NativeFs::new();
        fs.resolve_entry(&file.display().to_string()).unwrap();
        let root = fs
            .source_root()
            .expect("source_root() must be Some after resolve_entry");
        // The returned root must be an absolute path (`\\?\C:\…` on Windows, so
        // test with `Path::is_absolute`, never a leading `/`).
        assert!(
            Path::new(&root).is_absolute(),
            "source_root() must be absolute, got {root:?}"
        );
    }

    #[test]
    fn native_source_root_no_marker_falls_back_to_entry_dir() {
        // In a temp directory with no .git / .mdsroot marker, the root should
        // fall back to the entry-point directory itself (not a parent).
        //
        // Exact equality is required: an ancestor check (starts_with) would
        // pass even if find_project_root walked up to /tmp or /, which would
        // silently widen the containment envelope the security guard rests on.
        let dir = TempDir::new().unwrap();
        let file = make_temp_file(&dir, "main.mds", "hello");
        let fs = NativeFs::new();
        fs.resolve_entry(&file.display().to_string()).unwrap();
        let root = fs
            .source_root()
            .expect("root must be set after resolve_entry");
        let file_canon = file.canonicalize().unwrap();
        let root_path = std::path::PathBuf::from(&root);
        // Canonicalize root_path to resolve macOS /var → /private/var so the
        // comparison is not flaky across platforms.
        let root_canon = root_path.canonicalize().unwrap_or(root_path);
        assert_eq!(
            root_canon,
            effective_parent(&file_canon),
            "source_root must be exactly the entry-point directory (not a parent); \
             root={root:?} file_canon={file_canon:?}"
        );
    }

    /// #217: a root that is not valid UTF-8 has no byte-faithful string form, so
    /// `source_root()` must report `None` — the documented "no containment concept"
    /// value, which every consumer already handles with a guarded branch — rather
    /// than a lossy stand-in. A lossy anchor names a directory that does not exist
    /// and cannot be compared component-wise against a real source path.
    ///
    /// The containment check itself is unaffected: `check_path_traversal` compares
    /// `Path`s from `root_dir` directly and never goes through this string.
    ///
    /// The invalid byte is built at RUNTIME from a numeric value; no escape sequence
    /// or raw byte appears in this source file (Source hygiene gate).
    ///
    /// Positive control: a valid-UTF-8 root set the same way must still be reported.
    ///
    /// `#[cfg(unix)]`: builds the non-UTF-8 name with `OsStringExt` (arbitrary bytes), a
    /// Unix-only API; Windows paths are UTF-16 and have no such construction (#147).
    #[cfg(unix)]
    #[test]
    fn source_root_is_none_for_non_utf8_root() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        // 0xFF is not a legal UTF-8 lead byte in any position.
        let mut raw = b"/tmp/".to_vec();
        raw.push(0xff);

        let fs = NativeFs::new();
        fs.root_dir
            .set(PathBuf::from(OsString::from_vec(raw)))
            .expect("root_dir is unset on a fresh NativeFs");
        assert_eq!(
            fs.source_root(),
            None,
            "a root that is not valid UTF-8 must be reported as absent, never lossily"
        );

        // CONTROL ARM: a valid-UTF-8 root set the same way is still reported.
        let control = NativeFs::new();
        control
            .root_dir
            .set(PathBuf::from("/tmp/proj"))
            .expect("root_dir is unset on a fresh NativeFs");
        assert_eq!(
            control.source_root(),
            Some("/tmp/proj".to_string()),
            "control: a usable root must still be reported"
        );
    }

    #[test]
    fn vfs_source_root_always_none() {
        // VirtualFs has no containment concept — source_root() always returns None.
        let fs = VirtualFs::new(std::collections::HashMap::new());
        assert_eq!(
            fs.source_root(),
            None,
            "VirtualFs source_root() must always be None"
        );
    }

    // ── R3 / CWE-209: read() error messages show root-relative display ────────
    //
    // Each test pairs the "no absolute path" absence assertion with a PF-013
    // positive control proving the harness would catch the absolute form if it
    // leaked (same pattern as the display_path_for tests in source_path.rs).

    /// Root-anchored NativeFs over a temp project with a `sub/` directory.
    ///
    /// Returns the TempDir guard (keeps the tree alive), the canonicalized
    /// project root (macOS resolves `/var` → `/private/var`), and a NativeFs
    /// whose display root is anchored at that root.
    fn r3_read_display_project() -> (TempDir, PathBuf, NativeFs) {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        // Deterministic walk-up anchor: without a marker, find_project_root
        // would scan every ancestor for .git/.mdsroot.
        std::fs::write(root.join(".mdsroot"), "").unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let fs = NativeFs::new();
        fs.anchor_base_dir(root.to_str().unwrap()).unwrap();
        (dir, root, fs)
    }

    #[test]
    fn native_read_missing_file_error_shows_root_relative_display() {
        let (_guard, root, fs) = r3_read_display_project();
        let path = root.join("sub").join("missing.mds");

        let err = fs.read(path.to_str().unwrap()).unwrap_err();
        let msg = err.to_string();

        let root_str = root.to_str().unwrap();
        // Positive control (PF-013): a message carrying the absolute path WOULD
        // be caught by the absence assertion below.
        let planted = format!("cannot read {}: No such file", path.display());
        assert!(
            planted.contains(root_str),
            "positive control: planted absolute form must match the absence predicate"
        );
        assert!(
            msg.contains("cannot read sub/missing.mds"),
            "read error must show the root-relative path; got: {msg}"
        );
        assert!(
            !msg.contains(root_str),
            "read error must not leak the absolute root prefix; got: {msg}"
        );
    }

    #[test]
    fn native_read_oversize_error_shows_root_relative_display() {
        let (_guard, root, fs) = r3_read_display_project();
        let path = root.join("sub").join("big.mds");
        std::fs::write(&path, "x".repeat((MAX_FILE_SIZE + 1) as usize)).unwrap();

        let err = fs.read(path.to_str().unwrap()).unwrap_err();
        assert!(
            matches!(err, MdsError::ResourceLimit { .. }),
            "expected ResourceLimit, got {err:?}"
        );
        let msg = err.to_string();

        let root_str = root.to_str().unwrap();
        // Positive control (PF-013): the absolute form would be caught below.
        let planted = format!("file too large (… bytes): {}", path.display());
        assert!(
            planted.contains(root_str),
            "positive control: planted absolute form must match the absence predicate"
        );
        assert!(
            msg.contains("sub/big.mds"),
            "oversize error must show the root-relative path; got: {msg}"
        );
        assert!(
            !msg.contains(root_str),
            "oversize error must not leak the absolute root prefix; got: {msg}"
        );
    }

    #[test]
    fn native_read_invalid_utf8_error_shows_root_relative_display() {
        let (_guard, root, fs) = r3_read_display_project();
        let path = root.join("sub").join("bad.mds");
        std::fs::write(&path, b"Hello \xFF\xFE world\n").unwrap();

        let err = fs.read(path.to_str().unwrap()).unwrap_err();
        let msg = err.to_string();

        let root_str = root.to_str().unwrap();
        // Positive control (PF-013): the absolute form would be caught below.
        let planted = format!("invalid UTF-8 in {}: …", path.display());
        assert!(
            planted.contains(root_str),
            "positive control: planted absolute form must match the absence predicate"
        );
        assert!(
            msg.contains("invalid UTF-8 in sub/bad.mds"),
            "UTF-8 error must show the root-relative path; got: {msg}"
        );
        assert!(
            !msg.contains(root_str),
            "UTF-8 error must not leak the absolute root prefix; got: {msg}"
        );
    }
}
