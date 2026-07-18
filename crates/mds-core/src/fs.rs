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
/// - **Path traversal prevention**: `normalize` and `normalize_in_dir` must
///   reject paths that escape the intended root (e.g., `../../../etc/passwd`).
/// - **Null-byte rejection**: `normalize` and `normalize_in_dir` must reject
///   paths containing `\0`.
/// - **File size limits**: `read` must refuse content larger than
///   [`crate::MAX_FILE_SIZE`] bytes (10 MB) to prevent resource exhaustion.
/// - **Input sanitization**: `normalize` and `normalize_in_dir` must reject
///   empty paths.
/// - **`dir == ""`** (empty string) in `normalize_in_dir` means "virtual root" or
///   "no directory prefix" — resolve `relative` from the root of the key-space.
///
/// Failing to implement these controls silently bypasses the security enforced
/// by [`NativeFs`] and may expose the host system to arbitrary file reads or
/// denial-of-service attacks.
pub trait FileSystem: Send + Sync {
    /// Normalize a path relative to a base key.
    ///
    /// - `base == ""` means entry point (root-level resolution)
    /// - `base != ""` means importing from within an already-resolved module
    fn normalize(&self, base: &str, relative: &str) -> Result<String, MdsError>;

    /// Resolve `relative` directly within directory `dir`.
    ///
    /// Unlike `normalize`, which derives the directory by calling `parent()` on a
    /// *file* key, this method receives the directory explicitly — no sentinel
    /// filename, no `parent()` coupling, no Windows verbatim-path hazard (PF-003).
    ///
    /// `dir == ""` means resolve from the root of the key-space (the same as an
    /// import from a top-level file). Implementations must enforce all traversal,
    /// null-byte, and empty-path guards as described in the security contract above.
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
    /// [`MAX_PATH_SEGMENTS`] segments.
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
    /// `resolve_source` paths that don't go through [`FileSystem::normalize`].
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
}

// ── VirtualFs shared segment logic ───────────────────────────────────────────

/// Reject empty paths and paths containing null bytes.
///
/// Called at every normalization entry point; extracted to eliminate copy-paste
/// and ensure the error strings remain consistent across `normalize` and
/// `normalize_in_dir` on both `NativeFs` and `VirtualFs`.
fn validate_relative_import(relative: &str) -> Result<(), MdsError> {
    if relative.is_empty() {
        return Err(MdsError::import_error("import path is empty"));
    }
    if relative.contains('\0') {
        return Err(MdsError::import_error("import path contains null byte"));
    }
    Ok(())
}

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
/// Shared by [`VirtualFs::normalize`] and [`VirtualFs::normalize_in_dir`] so
/// both can never silently drift in their path-resolution semantics.
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
    /// Resolve `relative` against the directory portion of `base`.
    ///
    /// When `base == ""` the relative path is used as-is (root entry point).
    /// Rejects: empty paths, null bytes, traversal above the virtual root.
    fn normalize(&self, base: &str, relative: &str) -> Result<String, MdsError> {
        validate_relative_import(relative)?;

        if base.is_empty() {
            // Root entry point — use key as-is, but still enforce the segment limit.
            let segment_count = relative
                .split('/')
                .filter(|s| !s.is_empty() && *s != ".")
                .count();
            if segment_count > MAX_PATH_SEGMENTS {
                return Err(MdsError::resource_limit(format!(
                    "import path exceeds maximum segment count ({MAX_PATH_SEGMENTS}): \"{relative}\""
                )));
            }
            return Ok(relative.to_string());
        }

        // Delegate to normalize_in_dir with the parent directory of `base`.
        // `rsplit_once('/')` gives the directory part of the key, or "" for a
        // top-level key (no directory component).
        let base_dir = base.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        self.normalize_in_dir(base_dir, relative)
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
            .ok_or_else(|| MdsError::file_not_found(normalized.to_string()))?;
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
    /// The root is established on the first call to [`FileSystem::normalize`]
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
    /// # Errors
    ///
    /// - `MdsError::ImportError` — the final path component is a symlink.
    /// - `MdsError::FileNotFound` — the path does not exist or the parent cannot
    ///   be resolved.
    pub fn check_symlink(path: &Path) -> Result<PathBuf, MdsError> {
        let file_name = path
            .file_name()
            .ok_or_else(|| MdsError::file_not_found(path.display().to_string()))?;

        // Use effective_parent rather than path.parent().unwrap_or(".") because
        // Path::parent() on a bare filename (e.g. "hello.mds") returns Some("") —
        // an empty string — NOT None, so the unwrap_or fallback is dead code and
        // "".canonicalize() fails with a file-not-found error on every bare-filename
        // invocation of any subcommand.  effective_parent maps both Some("") and None
        // to Path::new("."), making bare relative filenames work correctly.
        let parent = effective_parent(path);
        let canonical_parent = parent
            .canonicalize()
            .map_err(|_| MdsError::file_not_found(path.display().to_string()))?;
        let canonical_without_following_last = canonical_parent.join(file_name);

        let canonical = canonical_without_following_last
            .canonicalize()
            .map_err(|_| MdsError::file_not_found(path.display().to_string()))?;

        if canonical != canonical_without_following_last {
            return Err(MdsError::import_error(format!(
                "symlinks are not allowed in imports: {}",
                path.display()
            )));
        }
        Ok(canonical)
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

    /// Check that `canonical` stays within the established root directory.
    fn check_path_traversal(&self, canonical: &Path) -> Result<(), MdsError> {
        if let Some(root) = self.root_dir.get() {
            if !canonical.starts_with(root) {
                return Err(MdsError::import_error(format!(
                    "import path escapes project directory: {}",
                    canonical.display()
                )));
            }
        }
        Ok(())
    }

    /// Resolve `relative` within `dir` (given as a `&Path`) using the established
    /// security primitives — no `Path`→`String`→`Path` round-trip on the hot path.
    ///
    /// Validates `relative` (null-byte, empty), joins with `dir` via `Path::join`
    /// (verbatim-path-safe on Windows; avoids PF-003 / #133), then runs
    /// `check_symlink` and `check_path_traversal` before returning the canonical
    /// key string.
    ///
    /// Does NOT call `init_root` — only entry-point resolution (the `base == ""`
    /// branch in `normalize`) anchors the security root.
    fn normalize_in_dir_impl(&self, dir: &Path, relative: &str) -> Result<String, MdsError> {
        validate_relative_import(relative)?;
        let path = dir.join(relative);
        let canonical = Self::check_symlink(&path)?;
        self.check_path_traversal(&canonical)?;
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
    fn normalize(&self, base: &str, relative: &str) -> Result<String, MdsError> {
        validate_relative_import(relative)?;

        if base.is_empty() {
            // Root entry point: treat `relative` as a filesystem path.
            let canonical = Self::check_symlink(Path::new(relative))?;
            // Anchor the security root on first entry-point resolution.
            // effective_parent is safe even if canonical is somehow relative — avoids PF-006.
            let entry_dir = effective_parent(&canonical);
            self.init_root(entry_dir);
            self.check_path_traversal(&canonical)?;
            Ok(canonical.display().to_string())
        } else {
            // Import from within a resolved module: resolve against the parent
            // directory of `base` via the Path-typed helper (no String round-trip).
            // effective_parent guards against an empty parent — avoids PF-006.
            let base_dir = effective_parent(Path::new(base));
            self.normalize_in_dir_impl(base_dir, relative)
        }
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
        // Read bytes first, then check size — this is the TOCTOU-safe pattern.
        // A metadata() pre-check would introduce a race window between the size
        // check and the actual read. Read first, reject after.
        let bytes = std::fs::read(path)
            .map_err(|e| MdsError::io(format!("cannot read {normalized}: {e}")))?;
        if bytes.len() as u64 > MAX_FILE_SIZE {
            return Err(MdsError::resource_limit(format!(
                "file too large ({} bytes, max {} bytes): {normalized}",
                bytes.len(),
                MAX_FILE_SIZE,
            )));
        }
        String::from_utf8(bytes)
            .map_err(|e| MdsError::io(format!("invalid UTF-8 in {normalized}: {e}")))
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
        // Use check_symlink() rather than std::fs::canonicalize() directly so that
        // symlinked directories are rejected before they can re-anchor the security
        // root to an attacker-controlled location (issue #21).
        //
        // check_symlink() returns ImportError (symlink detected) or FileNotFound
        // (path does not exist). ImportError passes through; FileNotFound is
        // re-wrapped as Io because canonicalize is a resolution operation, not
        // an import step.
        Self::check_symlink(Path::new(path))
            .map(|p| p.display().to_string())
            .map_err(|e| match e {
                MdsError::ImportError { .. } => e,
                MdsError::FileNotFound { .. } => {
                    MdsError::io(format!("cannot resolve path {path}: {e}"))
                }
                other => other,
            })
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    // ── VirtualFs::normalize ──────────────────────────────────────────────────

    fn vfs() -> VirtualFs {
        VirtualFs::new(HashMap::new())
    }

    #[test]
    fn vfs_normalize_same_dir_sibling() {
        let result = vfs().normalize("components/header.mds", "./footer.mds");
        assert_eq!(result.unwrap(), "components/footer.mds");
    }

    #[test]
    fn vfs_normalize_parent_dir() {
        let result = vfs().normalize("components/header.mds", "../shared.mds");
        assert_eq!(result.unwrap(), "shared.mds");
    }

    #[test]
    fn vfs_normalize_two_levels_up() {
        let result = vfs().normalize("a/b/c.mds", "../../d.mds");
        assert_eq!(result.unwrap(), "d.mds");
    }

    #[test]
    fn vfs_normalize_escapes_root() {
        // "a.mds" has no parent directory segment, so ".." would go above root.
        let result = vfs().normalize("a.mds", "../../x.mds");
        assert!(result.is_err(), "expected Err, got {result:?}");
    }

    #[test]
    fn vfs_normalize_dot_segments_collapsed() {
        let result = vfs().normalize("a/b.mds", "./././c.mds");
        assert_eq!(result.unwrap(), "a/c.mds");
    }

    #[test]
    fn vfs_normalize_empty_path_errors() {
        let result = vfs().normalize("a.mds", "");
        assert!(result.is_err(), "expected Err for empty path");
    }

    #[test]
    fn vfs_normalize_null_byte_errors() {
        let result = vfs().normalize("a.mds", "./\0bad.mds");
        assert!(result.is_err(), "expected Err for null byte");
    }

    #[test]
    fn vfs_normalize_root_entry_point() {
        let result = vfs().normalize("", "main.mds");
        assert_eq!(result.unwrap(), "main.mds");
    }

    #[test]
    fn vfs_normalize_deep_traversal_at_boundary() {
        // "deep/nested/file.mds" has dir = "deep/nested"; three ".." would escape.
        let result = vfs().normalize("deep/nested/file.mds", "../../../x.mds");
        assert!(result.is_err(), "expected Err when escaping root");
    }

    #[test]
    fn vfs_normalize_sibling_flat() {
        let result = vfs().normalize("a.mds", "./b.mds");
        assert_eq!(result.unwrap(), "b.mds");
    }

    #[test]
    fn vfs_normalize_subdirectory() {
        let result = vfs().normalize("a/b.mds", "./c/d.mds");
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
    fn vfs_read_missing_key_file_not_found() {
        let fs = VirtualFs::new(HashMap::new());
        let err = fs.read("missing.mds").unwrap_err();
        assert!(
            matches!(err, MdsError::FileNotFound { .. }),
            "expected FileNotFound, got {err:?}"
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

    #[test]
    fn native_normalize_entry_point() {
        let dir = TempDir::new().unwrap();
        let file = make_temp_file(&dir, "main.mds", "hello");
        let fs = NativeFs::new();
        let result = fs.normalize("", &file.display().to_string());
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        // The result should be a canonical absolute path string.
        let key = result.unwrap();
        assert!(key.contains("main.mds"), "key should contain filename");
    }

    #[test]
    fn native_normalize_import_from_base() {
        let dir = TempDir::new().unwrap();
        let file = make_temp_file(&dir, "main.mds", "hello");
        make_temp_file(&dir, "sibling.mds", "world");

        let fs = NativeFs::new();
        // First normalize the entry point to establish the root and get its key.
        let base_key = fs
            .normalize("", &file.display().to_string())
            .expect("entry point normalize failed");
        // Now resolve a sibling relative to it.
        let result = fs.normalize(&base_key, "./sibling.mds");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        let key = result.unwrap();
        assert!(key.contains("sibling.mds"));
    }

    #[test]
    fn native_normalize_symlink_rejected() {
        let dir = TempDir::new().unwrap();
        let target = make_temp_file(&dir, "target.mds", "hello");
        let link_path = dir.path().join("link.mds");
        std::os::unix::fs::symlink(&target, &link_path).unwrap();

        let fs = NativeFs::new();
        let result = fs.normalize("", &link_path.display().to_string());
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("symlinks"),
            "expected symlinks in error, got: {msg}"
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
        std::os::unix::fs::symlink(&target, &link_path).unwrap();

        let fs = NativeFs::new();
        // Establish root via a real (non-symlinked) entry point.
        fs.normalize("", &target.display().to_string())
            .expect("entry normalize should succeed");

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
    fn native_normalize_absolute_path_injection_rejected() {
        // Security boundary: an absolute path outside the established project root
        // must be rejected with "escapes project directory".
        let project_dir = TempDir::new().unwrap();
        let outside_dir = TempDir::new().unwrap();

        let entry = make_temp_file(&project_dir, "main.mds", "hello");
        let outside = make_temp_file(&outside_dir, "secret.mds", "secret");

        let fs = NativeFs::new();
        // Establish root via entry point.
        let base_key = fs
            .normalize("", &entry.display().to_string())
            .expect("entry point normalize should succeed");

        // Absolute path pointing outside the project root.
        let result = fs.normalize(&base_key, &outside.display().to_string());
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("escapes project"),
            "expected 'escapes project' in error, got: {msg}"
        );
    }

    #[test]
    fn native_normalize_relative_traversal_rejected() {
        // Security boundary: a relative `../` sequence that escapes the project
        // root must be rejected with "escapes project directory".
        let project_dir = TempDir::new().unwrap();
        let outside_dir = TempDir::new().unwrap();

        let entry = make_temp_file(&project_dir, "main.mds", "hello");
        // Place a real file outside so canonicalization has a target to resolve.
        let outside = make_temp_file(&outside_dir, "secret.mds", "secret");

        let fs = NativeFs::new();
        // Establish root via entry point.
        let base_key = fs
            .normalize("", &entry.display().to_string())
            .expect("entry point normalize should succeed");

        // Build a relative path using many ".." segments to escape the project
        // dir, then re-root into the outside directory.
        let outside_str = outside.display().to_string();
        let escape = "../".repeat(20) + outside_str.trim_start_matches('/');
        let result = fs.normalize(&base_key, &escape);
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
        // normalize calls reject paths outside that root.
        let project_dir = TempDir::new().unwrap();
        let outside_dir = TempDir::new().unwrap();

        let entry = make_temp_file(&project_dir, "main.mds", "hello");
        let outside = make_temp_file(&outside_dir, "secret.mds", "secret");

        let fs = NativeFs::new();
        // Initialize root explicitly via set_root, not via normalize.
        fs.set_root(&project_dir.path().display().to_string())
            .expect("set_root should succeed for a real directory");

        // normalize uses "" base for entry points, which would re-init root;
        // test the already-set root by normalizing a non-entry import instead.
        // First establish a valid base key by normalizing the entry point
        // (set_root already won the OnceLock race, so root stays as project_dir).
        let base_key = fs
            .normalize("", &entry.display().to_string())
            .expect("entry point normalize should succeed");

        // A path outside the root must be rejected.
        let result = fs.normalize(&base_key, &outside.display().to_string());
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
            .normalize("", &file.display().to_string())
            .expect("normalize");
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
    fn native_normalize_null_byte_errors() {
        let fs = NativeFs::new();
        let result = fs.normalize("", "./\0evil.mds");
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("null byte"),
            "expected null byte in error, got: {msg}"
        );
    }

    #[test]
    fn native_normalize_null_byte_in_import_errors() {
        let dir = TempDir::new().unwrap();
        let file = make_temp_file(&dir, "main.mds", "hello");
        let fs = NativeFs::new();
        let base_key = fs
            .normalize("", &file.display().to_string())
            .expect("entry normalize");
        let result = fs.normalize(&base_key, "./\0evil.mds");
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("null byte"),
            "expected null byte in error, got: {msg}"
        );
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

    // ── NativeFs::normalize empty-path guard ─────────────────────────────────

    #[test]
    fn native_normalize_empty_path_errors() {
        let fs = NativeFs::new();
        let result = fs.normalize("", "");
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("empty"),
            "expected 'empty' in error, got: {msg}"
        );
    }

    #[test]
    fn native_normalize_empty_import_path_errors() {
        let dir = TempDir::new().unwrap();
        let file = make_temp_file(&dir, "main.mds", "hello");
        let fs = NativeFs::new();
        let base_key = fs
            .normalize("", &file.display().to_string())
            .expect("entry normalize");
        let result = fs.normalize(&base_key, "");
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
    fn native_canonicalize_symlink_rejected() {
        // Security boundary: canonicalize() must reject symlinked directories so that
        // a symlinked base_dir cannot re-anchor the security root to an arbitrary location.
        let real_dir = TempDir::new().unwrap();
        let link_parent = TempDir::new().unwrap();
        let link_path = link_parent.path().join("link_to_dir");
        std::os::unix::fs::symlink(real_dir.path(), &link_path).unwrap();

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
    fn vfs_normalize_too_many_segments_errors() {
        // Import paths (non-root base) are bounded by MAX_PATH_SEGMENTS.
        // Use "root.mds" as base so base_dir_segments is empty, then provide
        // MAX_PATH_SEGMENTS + 1 segments in the relative path to exceed the cap.
        let long_relative = (0..=MAX_PATH_SEGMENTS)
            .map(|i| format!("seg{i}"))
            .collect::<Vec<_>>()
            .join("/");
        let result = vfs().normalize("root.mds", &long_relative);
        assert!(
            result.is_err(),
            "expected Err for path with too many segments, got {result:?}"
        );
        let err = result.unwrap_err();
        assert!(
            matches!(err, MdsError::ResourceLimit { .. }),
            "expected ResourceLimit variant, got {err:?}"
        );
    }

    #[test]
    fn vfs_normalize_exactly_at_segment_limit_ok() {
        // Exactly MAX_PATH_SEGMENTS segments must succeed.
        // Use "root.mds" as base so base_dir_segments is empty, then provide
        // exactly MAX_PATH_SEGMENTS segments in the relative path.
        let exactly = (0..MAX_PATH_SEGMENTS)
            .map(|i| format!("seg{i}"))
            .collect::<Vec<_>>()
            .join("/");
        let result = vfs().normalize("root.mds", &exactly);
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

    // ── normalize_in_dir consistency: must match normalize for same inputs ────

    #[test]
    fn vfs_normalize_in_dir_matches_normalize() {
        // For a non-empty base, normalize_in_dir(parent_dir(base), rel)
        // must produce the same result as normalize(base, rel).
        let base = "components/header.mds";
        let rel = "./footer.mds";
        let via_normalize = vfs().normalize(base, rel).unwrap();
        let via_in_dir = vfs()
            .normalize_in_dir(vfs().parent_dir(base).as_str(), rel)
            .unwrap();
        assert_eq!(
            via_normalize, via_in_dir,
            "normalize and normalize_in_dir must agree for same effective directory"
        );
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
        fs.normalize("", &entry).expect("entry normalize");

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
        let project_dir = TempDir::new().unwrap();
        let outside_dir = TempDir::new().unwrap();

        let entry = make_temp_file(&project_dir, "main.mds", "hello");
        let outside = make_temp_file(&outside_dir, "secret.mds", "secret");

        let fs = NativeFs::new();
        fs.normalize("", &entry.display().to_string())
            .expect("entry normalize");

        let dir_str = project_dir.path().display().to_string();
        let outside_str = outside.display().to_string();
        let escape = "../".repeat(20) + outside_str.trim_start_matches('/');
        let result = fs.normalize_in_dir(&dir_str, &escape);
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("escapes project"),
            "expected 'escapes project' in error, got: {msg}"
        );
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
        fn normalize(&self, base: &str, relative: &str) -> Result<String, MdsError> {
            if relative.is_empty() {
                return Err(MdsError::import_error("empty path"));
            }
            if base.is_empty() {
                return Ok(relative.to_string());
            }
            self.normalize_in_dir(&self.parent_dir(base), relative)
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
    #[cfg(unix)]
    fn check_symlink_symlinked_file_is_rejected() {
        // A symlinked file must be rejected, regardless of whether it is reached
        // via a bare name or an absolute path.
        let dir = TempDir::new().unwrap();
        let target = make_temp_file(&dir, "target.mds", "hello");
        let link_path = dir.path().join("link.mds");
        std::os::unix::fs::symlink(&target, &link_path).unwrap();
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
                "@import \"./lib.mds\" as lib\n{lib.greet(\"World\")}\n".to_string(),
            ),
            (
                "lib.mds".to_string(),
                "@define greet(x):\nHello {x}!\n@end\n".to_string(),
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
}
