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
/// Implementations provide path normalization, file reading, and file-type
/// detection. Security properties (symlink rejection, traversal prevention)
/// are implementation-specific: [`NativeFs`] enforces them for OS access,
/// while [`VirtualFs`] relies on its closed key-space.
///
/// # Security Contract
///
/// Custom implementations provided via [`crate::resolver::ModuleCache::with_fs`]
/// MUST uphold the following minimum obligations:
///
/// - **Path traversal prevention**: `resolve_entry` and `normalize_in_dir` must
///   reject paths that escape the intended root (e.g., `../../../etc/passwd`).
/// - **Null-byte rejection**: `normalize_in_dir` must reject paths containing
///   `\0`. The resolver refuses an entry path containing `\0` before
///   `resolve_entry` is called.
/// - **Segment cap**: `resolve_entry` and `normalize_in_dir` must refuse a path
///   of more than 256 segments with [`MdsError::ResourceLimit`].
/// - **File size limits**: `read` must refuse content larger than
///   [`crate::MAX_FILE_SIZE`] bytes (10 MB) to prevent resource exhaustion.
/// - **Input sanitization**: `normalize_in_dir` must reject empty paths. The
///   resolver refuses an empty entry path before `resolve_entry` is called.
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
    /// NUL byte (`mds::io`); both built-in backends refuse them again so a direct
    /// call is covered too. [`NativeFs`] returns the canonical absolute path,
    /// refuses a symlinked final component, and anchors the project root on its
    /// first call. [`VirtualFs`] returns the key unchanged.
    ///
    /// # Errors
    ///
    /// - [`MdsError::Io`] when `path` is empty or contains a null byte (`\0`).
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
    /// - `relative` contains a null byte (`\0`)
    /// - the resolved path traverses above the key-space root (`..` from root)
    /// - the resolved path is a symlink ([`NativeFs`] only)
    /// - the resolved path escapes the established project root ([`NativeFs`] only)
    ///
    /// Returns [`MdsError::ResourceLimit`] when the resolved path exceeds
    /// `MAX_PATH_SEGMENTS` segments.
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

    /// Pre-initialize the project root before imports resolve.
    ///
    /// Default: no-op. [`VirtualFs`] ignores this; [`NativeFs`] uses it for
    /// `resolve_source` paths that don't go through [`FileSystem::resolve_entry`].
    fn set_root(&self, _base: &str) -> Result<(), MdsError> {
        Ok(())
    }

    /// Resolve a path to its canonical (absolute, symlink-free) form.
    ///
    /// The default implementation is an identity function — suitable for
    /// virtual or in-memory filesystems where canonicalization is a no-op.
    ///
    /// [`NativeFs`] overrides this to call [`std::fs::canonicalize`].
    fn canonicalize(&self, path: &str) -> Result<String, MdsError> {
        Ok(path.to_string())
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
    /// `resolve_entry` or `set_root` call).
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

/// Reject empty import paths and import paths containing null bytes.
///
/// Called by `normalize_in_dir` on both `NativeFs` and `VirtualFs`, so the error
/// strings stay identical across backends.
fn validate_relative_import(relative: &str) -> Result<(), MdsError> {
    if relative.is_empty() {
        return Err(MdsError::import_error("import path is empty"));
    }
    if relative.contains('\0') {
        return Err(MdsError::import_error("import path contains null byte"));
    }
    Ok(())
}

/// Reject an entry path that is empty or contains a null byte (`mds::io`).
///
/// An entry path names the file a compile starts from. It is caller input, not an
/// `@import` string, so it reports `mds::io` like the other entry-path checks at
/// the API boundary (a path that is not valid UTF-8). The resolver runs this
/// before [`FileSystem::resolve_entry`], so a custom backend is covered; both
/// built-in backends run it again so a direct trait call is covered too.
///
/// The path is WIRE-escaped in the message: it is the string the caller passed,
/// never a resolved absolute path.
pub(crate) fn validate_entry_path(path: &str) -> Result<(), MdsError> {
    if path.is_empty() {
        return Err(MdsError::io("entry path is empty"));
    }
    if path.contains('\0') {
        return Err(MdsError::io(format!(
            "entry path contains null byte: \"{}\"",
            crate::lint::sanitize_control_chars_wire(path)
        )));
    }
    Ok(())
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
    /// the module map exactly. Rejects empty keys and null bytes (`mds::io`) and
    /// keys of more than 256 segments.
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
    /// or [`FileSystem::set_root`].
    pub fn new() -> Self {
        Self {
            root_dir: OnceLock::new(),
        }
    }

    /// Canonicalize `path` and detect symlinks without a TOCTOU window.
    ///
    /// Strategy: canonicalize parent dir (resolves dir-level symlinks), then
    /// canonicalize the full path. If they differ, the final component is a symlink.
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
    pub fn check_symlink(path: &Path) -> Result<PathBuf, MdsError> {
        // Delegate to the named variant using path.display() as the shown string.
        // External callers (CLI lint/fmt/watch/output) pass absolute paths where
        // showing the full path in error messages is appropriate.
        Self::check_symlink_named(path, &path.display().to_string())
    }

    /// Canonicalize a directory path, handling the filesystem-root edge case (#371).
    ///
    /// A filesystem root (`/` on Unix, or a drive root such as `C:\` on
    /// Windows) has no parent component, so `check_symlink_named`'s
    /// parent-then-child double-canonicalize dance — which canonicalizes the
    /// PARENT directory before joining the final path component — has no
    /// parent to canonicalize; `Path::file_name()` returns `None` for a root,
    /// so `check_symlink_named` fails immediately with `FileNotFound`, before
    /// any syscall (`#371`).
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
    fn check_symlink_named(path: &Path, shown: &str) -> Result<PathBuf, MdsError> {
        let file_name = path
            .file_name()
            .ok_or_else(|| MdsError::file_not_found(shown.to_string()))?;

        let parent = effective_parent(path);
        let canonical_parent = parent
            .canonicalize()
            .map_err(|_| MdsError::file_not_found(shown.to_string()))?;
        let canonical_without_following_last = canonical_parent.join(file_name);

        let canonical = canonical_without_following_last
            .canonicalize()
            .map_err(|_| MdsError::file_not_found(shown.to_string()))?;

        if canonical != canonical_without_following_last {
            return Err(MdsError::import_error(format!(
                "symlinks are not allowed in imports: {shown}"
            )));
        }
        Ok(canonical)
    }

    /// Resolve `relative` within `dir` (given as a `&Path`) using the established
    /// security primitives — no `Path`→`String`→`Path` round-trip on the hot path.
    ///
    /// Validates `relative` (null-byte, empty, segment cap), joins with `dir` via
    /// `Path::join` (verbatim-path-safe on Windows; avoids PF-003 / #133), then runs
    /// `check_symlink` and `check_path_traversal` before returning the canonical
    /// key string.
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
        Ok(canonical.display().to_string())
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
        // Anchor the security root on first entry-point resolution.
        // effective_parent is safe even if canonical is somehow relative — avoids PF-006.
        let entry_dir = effective_parent(&canonical);
        self.init_root(entry_dir);
        self.check_path_traversal(&canonical, path)?;
        Ok(canonical.display().to_string())
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

    fn set_root(&self, base: &str) -> Result<(), MdsError> {
        let canonical = Path::new(base)
            .canonicalize()
            .map_err(|e| MdsError::io(format!("cannot resolve base directory {base}: {e}")))?;
        self.init_root(&canonical);
        Ok(())
    }

    fn canonicalize(&self, path: &str) -> Result<String, MdsError> {
        // Delegate to canonical_dir() rather than calling check_symlink()
        // directly so that a filesystem-root path (`/`, a Windows drive root)
        // is handled correctly instead of hitting the `path.file_name() ==
        // None` cwd trap (#371) — see canonical_dir's doc comment.
        //
        // For every other path, canonical_dir is exactly check_symlink_named,
        // so symlinked directories are still rejected before they can
        // re-anchor the security root to an attacker-controlled location
        // (issue #21).
        //
        // canonical_dir returns ImportError (symlink detected), FileNotFound
        // (path does not exist), or an already-Io error (root branch).
        // ImportError and Io pass through; FileNotFound is re-wrapped as Io
        // because canonicalize is a resolution operation, not an import step.
        Self::canonical_dir(Path::new(path), path)
            .map(|p| p.display().to_string())
            .map_err(|e| match e {
                MdsError::ImportError { .. } => e,
                MdsError::FileNotFound { .. } => {
                    MdsError::io(format!("cannot resolve path {path}: {e}"))
                }
                other => other,
            })
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

    /// Creates a symlink for a test, tolerating Windows' unprivileged restriction.
    ///
    /// Unix symlink creation needs no special privilege. On Windows it needs
    /// either Developer Mode or `SeCreateSymbolicLinkPrivilege` (an elevated
    /// process) — GitHub's `windows-latest` runners have Developer Mode enabled,
    /// so a failure there is a genuine regression and must panic. Locally,
    /// without that privilege, the OS reports `ERROR_PRIVILEGE_NOT_HELD` (raw
    /// error 1314); this helper treats exactly that failure as a skip (never a
    /// false pass) when the `CI` env var is unset, printing a one-line reason.
    /// Returns `false` when the caller should skip the rest of the test.
    fn make_symlink(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        let result = std::os::unix::fs::symlink(target, link);
        #[cfg(windows)]
        let result = if target.is_dir() {
            std::os::windows::fs::symlink_dir(target, link)
        } else {
            std::os::windows::fs::symlink_file(target, link)
        };

        match result {
            Ok(()) => true,
            Err(err) => {
                #[cfg(windows)]
                {
                    const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;
                    if err.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD)
                        && std::env::var_os("CI").is_none()
                    {
                        eprintln!(
                            "skipping: symlink creation needs Developer Mode or an elevated process on Windows"
                        );
                        return false;
                    }
                }
                panic!(
                    "failed to create symlink {} -> {}: {err}",
                    target.display(),
                    link.display()
                );
            }
        }
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
    fn native_set_root_rejects_paths_outside_root() {
        // set_root should initialize the root directory so that subsequent
        // imports reject paths outside that root.
        let project_dir = TempDir::new().unwrap();
        let outside_dir = TempDir::new().unwrap();

        let entry = make_temp_file(&project_dir, "main.mds", "hello");
        let outside = make_temp_file(&outside_dir, "secret.mds", "secret");

        let fs = NativeFs::new();
        // Initialize root explicitly via set_root, not via resolve_entry.
        fs.set_root(&project_dir.path().display().to_string())
            .expect("set_root should succeed for a real directory");

        // Establish a valid base key by resolving the entry point (set_root already
        // won the OnceLock race, so the root stays as project_dir), then test the
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
            "expected 'escapes project' after set_root, got: {msg}"
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

    // ── FileSystem::canonicalize ──────────────────────────────────────────────

    #[test]
    fn vfs_canonicalize_returns_identity() {
        // VirtualFs inherits the default implementation — returns path unchanged.
        let key = "some/virtual/path.mds";
        let result = vfs().canonicalize(key);
        assert_eq!(
            result.unwrap(),
            key,
            "VirtualFs canonicalize should be identity"
        );
    }

    #[test]
    fn native_canonicalize_resolves_real_path() {
        // NativeFs should resolve a real file to its canonical absolute path.
        let dir = TempDir::new().unwrap();
        let file = make_temp_file(&dir, "real.mds", "content");
        let fs = NativeFs::new();
        let result = fs.canonicalize(&file.display().to_string());
        let canonical = result.expect("canonicalize should succeed for real file");
        // The canonical path must be absolute and contain the filename.
        assert!(
            canonical.contains("real.mds"),
            "canonical path should contain filename, got: {canonical}"
        );
        // Must be an absolute path.
        assert!(
            Path::new(&canonical).is_absolute(),
            "canonical path should be absolute, got: {canonical}"
        );
    }

    #[test]
    fn native_canonicalize_nonexistent_errors() {
        // NativeFs should return an Io error for a nonexistent path.
        let fs = NativeFs::new();
        let result = fs.canonicalize("/nonexistent/path/does/not/exist.mds");
        let err = result.unwrap_err();
        assert!(
            matches!(err, MdsError::Io { .. }),
            "expected Io error for nonexistent path, got: {err:?}"
        );
    }

    #[test]
    fn native_canonicalize_root_returns_root_not_cwd() {
        // #371 cwd trap regression: canonicalize() on a filesystem root must
        // anchor AT the root, never silently fall back to the current working
        // directory. A naive fix that funneled the root case through
        // `effective_parent` (which maps an absent/empty parent to ".") would
        // reintroduce exactly this bug.
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
        let result = fs.canonicalize(&root.display().to_string());
        let canonical = result.expect("canonicalize should succeed for a filesystem root");

        assert_eq!(
            Path::new(&canonical),
            root.as_path(),
            "canonicalize(root) must return the root itself, got: {canonical}"
        );
        assert_ne!(
            Path::new(&canonical),
            cwd.as_path(),
            "canonicalize(root) must not resolve to cwd, got: {canonical}"
        );
    }

    #[test]
    fn native_canonicalize_symlink_rejected() {
        // Security boundary: canonicalize() must reject symlinked directories so that
        // a symlinked base_dir cannot re-anchor the security root to an arbitrary location.
        let real_dir = TempDir::new().unwrap();
        let link_parent = TempDir::new().unwrap();
        let link_path = link_parent.path().join("link_to_dir");
        if !make_symlink(real_dir.path(), &link_path) {
            return;
        }

        let fs = NativeFs::new();
        let result = fs.canonicalize(&link_path.display().to_string());
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("symlinks"),
            "expected 'symlinks' in error when canonicalizing a symlink, got: {msg}"
        );
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

    // ── source_root ───────────────────────────────────────────────────────────

    #[test]
    fn native_source_root_none_before_any_resolve_entry() {
        // Before resolve_entry() or set_root() is called, root has not been established.
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
        fs.set_root(root.to_str().unwrap()).unwrap();
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
