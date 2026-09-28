//! Filesystem abstraction for module resolution.
//!
//! Provides the [`FileSystem`] trait and two implementations:
//! - [`NativeFs`] — OS filesystem with symlink rejection and traversal prevention
//! - [`VirtualFs`] — in-memory HashMap-backed filesystem for testing and WASM

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use crate::error::MdsError;
use crate::limits::{
    MAX_FILE_SIZE, MAX_MODULE_ALIASES, MAX_MODULE_ALIASES_SIZE, MAX_TRAVERSAL_DEPTH,
};

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
///   import strings and base directories the resolver has already checked, and from
///   alias targets, each of which [`VirtualFs::with_aliases`] checks as a key an
///   import can resolve to.
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
/// (#265): `<what> contains forbidden character U+XXXX: "<typed>"`.
///
/// `typed` is the path as the caller typed it, raw: it is escaped here, with
/// [`crate::escape_path_for_message`], so the message itself carries none of the
/// 80 forbidden codepoints (TAB included) and never substitutes a resolved absolute
/// path for what the caller passed.
pub(crate) fn forbidden_char_message(what: &str, ch: char, typed: &str) -> String {
    format!(
        "{what} contains forbidden character U+{:04X}: \"{}\"",
        u32::from(ch),
        crate::lint::escape_path_for_message(typed)
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

/// Refuse `path` when it carries a [`crate::is_forbidden_path_char`] codepoint
/// anywhere in it (#265): [`MdsError::Io`], `<what> contains forbidden character
/// U+XXXX: "<typed>"`, naming the first such codepoint.
///
/// `path` is what is scanned and `typed` what the message names: the path as the
/// caller typed it, raw — the message escapes it with
/// [`crate::escape_path_for_message`], so pass it unescaped. It differs from `path`
/// when `path` is the form it resolves to, so the message never shows a resolved
/// absolute path the caller did not type. A path that is not valid UTF-8 is scanned
/// lossily: every forbidden codepoint that is validly encoded survives the conversion.
///
/// mds-core words its own refusals of a path with the same message, so a caller that
/// checks a path mds-core never sees (an output location, say) refuses it in the same
/// words.
///
/// # Errors
///
/// [`MdsError::Io`] when `path` carries a forbidden path character.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// mds::reject_forbidden_path("output", Path::new("out/page.md"), "out/page.md")?;
///
/// let err = mds::reject_forbidden_path("output", Path::new("out\tdir"), "link").unwrap_err();
/// assert_eq!(
///     err.to_string(),
///     "output contains forbidden character U+0009: \"link\""
/// );
/// # Ok::<(), mds::MdsError>(())
/// ```
pub fn reject_forbidden_path(what: &str, path: &Path, typed: &str) -> Result<(), MdsError> {
    match first_forbidden_char(&path.to_string_lossy()) {
        Some(ch) => Err(MdsError::io(forbidden_char_message(what, ch, typed))),
        None => Ok(()),
    }
}

/// Refuse a resolved path that carries a forbidden path character anywhere in it
/// (`mds::io`, #265): [`reject_forbidden_path`] as `resolved path`.
///
/// The typed path has already been checked by the time a path is resolved; this
/// catches what the typed form cannot show — a symlinked directory whose target has a
/// hostile name, or a project that lives under one. The WHOLE path is scanned, not
/// only its final component. The message names `typed`, the path the caller typed
/// (raw, escaped in the message), never the absolute resolved path (R3 / CWE-209). A path that is not valid UTF-8 is
/// scanned lossily, and `key_of` then refuses it rather than turn it into a key.
pub(crate) fn reject_forbidden_in_path(resolved: &Path, typed: &str) -> Result<(), MdsError> {
    reject_forbidden_path("resolved path", resolved, typed)
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

// ── Module bytes ─────────────────────────────────────────────────────────────

/// Check a module file's bytes as [`NativeFs`] checks every file it reads, and
/// return its text: more than [`crate::MAX_FILE_SIZE`] bytes is refused with
/// [`MdsError::ResourceLimit`] (`file too large (<n> bytes, max <max> bytes):
/// <display>`), and bytes that are not valid UTF-8 with [`MdsError::Io`] (`invalid
/// UTF-8 in <display>: <reason>`). A leading byte-order mark is kept, as the
/// template's first character.
///
/// `display` names the file in both messages — [`NativeFs`] passes its path relative
/// to the project root — escaped with [`crate::escape_path_for_message`]. It is the
/// one implementation of these checks for a module read as bytes: [`NativeFs`] reads a
/// module and calls it, `mds::lint` calls it when it re-reads its entry, and so does
/// `@mdscript/mds`'s WASM backend, whose JS pre-scanner reads each file itself
/// (`preflightModule`), so both backends refuse the same bytes with the same error
/// (#414). [`VirtualFs`] holds text, not bytes: its `read` keeps its own size check.
///
/// # Errors
///
/// [`MdsError::ResourceLimit`] over the size cap; [`MdsError::Io`] for invalid UTF-8,
/// including an incomplete sequence at the end.
///
/// # Examples
///
/// ```
/// assert_eq!(mds::check_module_bytes(b"Hello!\n".to_vec(), "hi.mds")?, "Hello!\n");
///
/// let err = mds::check_module_bytes(vec![b'h', 0xff], "bad.mds").unwrap_err();
/// assert_eq!(
///     err.to_string(),
///     "invalid UTF-8 in bad.mds: invalid utf-8 sequence of 1 bytes from index 1"
/// );
/// # Ok::<(), mds::MdsError>(())
/// ```
pub fn check_module_bytes(bytes: Vec<u8>, display: &str) -> Result<String, MdsError> {
    if bytes.len() as u64 > MAX_FILE_SIZE {
        return Err(file_too_large(bytes.len() as u64, display));
    }
    String::from_utf8(bytes).map_err(|e| {
        MdsError::io(format!(
            "invalid UTF-8 in {}: {e}",
            crate::lint::escape_path_for_message(display)
        ))
    })
}

/// The refusal of a module file of `size` bytes, over [`crate::MAX_FILE_SIZE`], named
/// by `display`, escaped.
fn file_too_large(size: u64, display: &str) -> MdsError {
    MdsError::resource_limit(format!(
        "file too large ({size} bytes, max {MAX_FILE_SIZE} bytes): {}",
        crate::lint::escape_path_for_message(display)
    ))
}

/// Read the module file at `path` and check it as [`check_module_bytes`] does, naming
/// it by `display`, escaped in every refusal, without ever holding more than one byte
/// over [`crate::MAX_FILE_SIZE`] of it (#428): a file over the cap when it is opened is
/// refused before a byte is read, with its size; one that grows past the cap while it
/// is read is read to one byte past the cap and refused. A module that is not a regular
/// file — a directory, a FIFO, a device, a socket — is refused before it is opened, as
/// `cannot read <display>: not a regular file`, the refusal `@mdscript/mds`'s WASM
/// backend makes: opening a FIFO nobody writes to blocks. [`NativeFs::read`] and
/// `mds::lint`'s re-read of its entry read a module through it.
pub(crate) fn read_module_file(path: &Path, display: &str) -> Result<String, MdsError> {
    let bytes = match read_regular_capped(path, MAX_FILE_SIZE) {
        Ok(Capped::Bytes(bytes)) => bytes,
        Ok(Capped::TooLarge(size)) => return Err(file_too_large(size, display)),
        Err(e) => {
            return Err(MdsError::io(format!(
                "cannot read {}: {e}",
                crate::lint::escape_path_for_message(display)
            )));
        }
    };
    check_module_bytes(bytes, display)
}

/// A file read under a size cap (see [`read_capped`]).
pub(crate) enum Capped {
    /// Its bytes: at most one more than the cap, and one more means it is over it.
    Bytes(Vec<u8>),
    /// Its size when it was opened, over the cap: nothing was read.
    TooLarge(u64),
}

/// Open `path` and read it, holding no more than one byte over `cap` of it: a file
/// over `cap` when it is opened is reported by its size, unread; any other is read to
/// its end or to one byte past `cap`, whichever comes first. The size is taken from
/// the opened file, so no other file can take its place between the two.
pub(crate) fn read_capped(path: &Path, cap: u64) -> std::io::Result<Capped> {
    let file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    read_opened_capped(file, size, cap)
}

/// [`read_capped`] for a regular file only: anything else fails with
/// [`not_a_regular_file`], judged on `path` before it is opened — opening a FIFO nobody
/// writes to blocks — and again on the opened file, which may have taken its place in
/// between.
fn read_regular_capped(path: &Path, cap: u64) -> std::io::Result<Capped> {
    if !std::fs::metadata(path)?.is_file() {
        return Err(not_a_regular_file());
    }
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(not_a_regular_file());
    }
    read_opened_capped(file, metadata.len(), cap)
}

/// Read the opened `file`, whose size taken from it is `size`, as [`read_capped`] reads.
fn read_opened_capped(mut file: std::fs::File, size: u64, cap: u64) -> std::io::Result<Capped> {
    if size > cap {
        return Ok(Capped::TooLarge(size));
    }
    read_at_most(&mut file, cap.saturating_add(1), size).map(Capped::Bytes)
}

/// Why a module that is not a regular file is not read — the reason
/// `@mdscript/mds`'s WASM backend gives for it too.
fn not_a_regular_file() -> std::io::Error {
    std::io::Error::other("not a regular file")
}

/// How many reads in a row may be interrupted before `read_at_most` gives up.
const MAX_INTERRUPTED_READS: u32 = 64;

/// Read `reader` to its end or to `limit` bytes, whichever comes first, into a buffer
/// whose capacity never exceeds `limit` (#428): it starts at `size_hint` plus one byte
/// (room to see the end of a source whose size is known without growing) and grows by
/// at most doubling, capped at `limit`.
///
/// Pass one byte more than a size cap as `limit` to tell a source of exactly the cap
/// from a larger one while holding no more than that one byte over it; mds-core reads
/// every module file this way, and the CLI its stdin, its `mds.json` and the head of a
/// stale source map.
///
/// Every read that returns bytes brings the buffer closer to `limit`, and at most 64
/// reads in a row may be interrupted, so the loop is bounded.
///
/// # Errors
///
/// The first error `reader` returns other than [`std::io::ErrorKind::Interrupted`], and
/// that one when more than 64 reads in a row are interrupted.
///
/// # Examples
///
/// ```
/// let bytes = mds::read_at_most(&mut &b"Hello!\n"[..], 4, 0)?;
/// assert_eq!(bytes, b"Hell");
/// assert!(bytes.capacity() <= 4);
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn read_at_most(
    reader: &mut impl std::io::Read,
    limit: u64,
    size_hint: u64,
) -> std::io::Result<Vec<u8>> {
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let first = usize::try_from(size_hint)
        .unwrap_or(usize::MAX)
        .saturating_add(1)
        .min(limit);
    let mut buf: Vec<u8> = Vec::with_capacity(first);
    let mut chunk = [0u8; 8 * 1024];
    let mut interrupted = 0;
    while buf.len() < limit {
        let want = chunk.len().min(limit - buf.len());
        let n = match reader.read(&mut chunk[..want]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                interrupted += 1;
                if interrupted > MAX_INTERRUPTED_READS {
                    return Err(e);
                }
                continue;
            }
            Err(e) => return Err(e),
        };
        interrupted = 0;
        if buf.capacity() - buf.len() < n {
            // Double, but never past `limit`: `n` fits, as `want` did.
            buf.reserve_exact(buf.capacity().max(n).min(limit - buf.len()));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(buf)
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
    /// A key an import resolves to → the key of the module in `modules` it names
    /// (see [`VirtualFs::with_aliases`]).
    aliases: HashMap<String, String>,
}

impl VirtualFs {
    /// Create a new `VirtualFs` from a map of key → content.
    pub fn new(modules: HashMap<String, String>) -> Self {
        Self {
            modules,
            aliases: HashMap::new(),
        }
    }

    /// Let an import that resolves to the key `alias` reach the module keyed
    /// `aliases[alias]` instead: that module's key is the one the import resolves to,
    /// and so the one its dependency, its source-map source and a cycle through it are
    /// named by. An entry key is taken as given.
    ///
    /// `@mdscript/mds`'s WASM backend reads the modules of a `compileFile` itself and
    /// keys each by its path on disk below the project root. An import can name a file
    /// by another spelling — a case variant on a case-insensitive volume, or a path
    /// through a symbolic link — which the backend passes as an alias, so every module
    /// is named by its on-disk path, as the native backend names it (#414).
    ///
    /// Every alias and every module key it names is checked as a key an import can
    /// resolve to — not empty, free of [`crate::is_forbidden_path_char`] codepoints, and
    /// made of at most 256 segments, none of them empty, `.` or `..` — however the
    /// spelling that led to it was checked: folding a name's case is a lossy mapping,
    /// so the key a module is reached by is validated as the key it is. Every alias must
    /// name a module, and no alias may be a module key itself.
    ///
    /// The map is bounded before any alias in it is checked: at most
    /// [`crate::MAX_MODULE_ALIASES`] aliases, whose keys and the module keys they name
    /// total at most 10 MiB — the bounds the WASM `moduleAliases` option applies as it
    /// is read.
    ///
    /// # Errors
    ///
    /// A [`ModuleAliasError`], which converts into an [`MdsError`]:
    /// [`ModuleAliasError::TooMany`] or [`ModuleAliasError::TooLarge`] for a map past a
    /// bound (`mds::resource_limit`), then [`ModuleAliasError::Refused`] for the first
    /// offending alias in key order (`mds::io`), naming it escaped with
    /// [`crate::escape_path_for_message`]: `module alias "<alias>": <reason>`.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::collections::HashMap;
    /// use mds::FileSystem;
    ///
    /// let modules = HashMap::from([("header.mds".to_string(), "Hi\n".to_string())]);
    /// let aliases = HashMap::from([("Header.mds".to_string(), "header.mds".to_string())]);
    /// let fs = mds::VirtualFs::new(modules.clone()).with_aliases(aliases)?;
    /// assert_eq!(fs.normalize_in_dir("", "./Header.mds")?, "header.mds");
    ///
    /// let missing = HashMap::from([("Header.mds".to_string(), "gone.mds".to_string())]);
    /// let err = mds::VirtualFs::new(modules).with_aliases(missing).unwrap_err();
    /// assert!(matches!(err, mds::ModuleAliasError::Refused { .. }));
    /// assert_eq!(
    ///     err.to_string(),
    ///     "module alias \"Header.mds\": its module key \"gone.mds\" names no module"
    /// );
    /// # Ok::<(), mds::MdsError>(())
    /// ```
    pub fn with_aliases(
        mut self,
        aliases: HashMap<String, String>,
    ) -> Result<Self, ModuleAliasError> {
        if aliases.len() > MAX_MODULE_ALIASES {
            return Err(ModuleAliasError::TooMany {
                count: aliases.len(),
            });
        }
        let size = aliases.iter().fold(0usize, |size, (alias, target)| {
            size.saturating_add(alias.len())
                .saturating_add(target.len())
        });
        if size > MAX_MODULE_ALIASES_SIZE {
            return Err(ModuleAliasError::TooLarge);
        }
        let mut keys: Vec<&String> = aliases.keys().collect();
        keys.sort_unstable();
        for alias in keys {
            if let Some(reason) = alias_violation(&self.modules, alias, &aliases[alias]) {
                return Err(ModuleAliasError::Refused {
                    alias: crate::lint::escape_path_for_message(alias).into_owned(),
                    reason,
                });
            }
        }
        self.aliases = aliases;
        Ok(self)
    }

    /// The source of the module keyed `key`, if there is one.
    pub(crate) fn module(&self, key: &str) -> Option<&str> {
        self.modules.get(key).map(String::as_str)
    }
}

/// Why [`VirtualFs::with_aliases`] refused an alias map (#414).
///
/// Its text is the message of the [`MdsError`] it converts into (with `?`, or
/// [`From`]): [`TooMany`](Self::TooMany) and [`TooLarge`](Self::TooLarge) become
/// `mds::resource_limit`, [`Refused`](Self::Refused) `mds::io`. A binding that names the
/// alias in its own words reads [`Refused`](Self::Refused)'s fields, which are escaped.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleAliasError {
    /// More than [`crate::MAX_MODULE_ALIASES`] aliases:
    /// `module alias count exceeds maximum of 65536 (<count> provided)`.
    #[non_exhaustive]
    TooMany {
        /// How many aliases the map holds.
        count: usize,
    },
    /// Aliases whose keys and module keys total more than 10 MiB:
    /// `module aliases aggregate size exceeds maximum of 10485760 bytes`.
    #[non_exhaustive]
    TooLarge,
    /// The first alias, in key order, that cannot join the filesystem:
    /// `module alias "<alias>": <reason>`.
    #[non_exhaustive]
    Refused {
        /// The alias, escaped with [`crate::escape_path_for_message`].
        alias: String,
        /// Why it is refused; a module key it names is escaped the same way.
        reason: String,
    },
}

// `#[inline]` on `fmt` and `from` so a binding compiles them at its own optimization
// level: mds-wasm is built for size, and compiled in mds-core (opt-level 3) these two
// measured about 5.6 KB larger in the WASM binary.
impl std::fmt::Display for ModuleAliasError {
    #[inline]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModuleAliasError::TooMany { count } => write!(
                f,
                "module alias count exceeds maximum of {MAX_MODULE_ALIASES} ({count} provided)"
            ),
            ModuleAliasError::TooLarge => write!(
                f,
                "module aliases aggregate size exceeds maximum of {MAX_MODULE_ALIASES_SIZE} bytes"
            ),
            ModuleAliasError::Refused { alias, reason } => {
                write!(f, "module alias \"{alias}\": {reason}")
            }
        }
    }
}

impl std::error::Error for ModuleAliasError {}

impl From<ModuleAliasError> for MdsError {
    #[inline]
    fn from(err: ModuleAliasError) -> Self {
        match err {
            ModuleAliasError::Refused { .. } => MdsError::io(err.to_string()),
            ModuleAliasError::TooMany { .. } | ModuleAliasError::TooLarge => {
                MdsError::resource_limit(err.to_string())
            }
        }
    }
}

/// Why `alias` → `target` cannot join a `VirtualFs` holding `modules`, if it cannot
/// (see [`VirtualFs::with_aliases`]).
fn alias_violation(modules: &HashMap<String, String>, alias: &str, target: &str) -> Option<String> {
    if let Some(reason) = module_key_violation(alias) {
        return Some(format!("the alias {reason}"));
    }
    if modules.contains_key(alias) {
        return Some("the alias is a module key itself".to_owned());
    }
    let shown = crate::lint::escape_path_for_message(target);
    if let Some(reason) = module_key_violation(target) {
        return Some(format!("its module key \"{shown}\" {reason}"));
    }
    (!modules.contains_key(target)).then(|| format!("its module key \"{shown}\" names no module"))
}

/// Why `key` is not a key an import can resolve to on a [`VirtualFs`] — the form
/// `normalize_in_dir` produces — if it is not.
fn module_key_violation(key: &str) -> Option<String> {
    if key.is_empty() {
        return Some("is empty".to_owned());
    }
    if let Some(ch) = first_forbidden_char(key) {
        return Some(format!(
            "contains forbidden character U+{:04X}",
            u32::from(ch)
        ));
    }
    let mut segments = 0usize;
    for segment in key.split('/') {
        match segment {
            ".." => return Some("has a '..' segment".to_owned()),
            "" | "." => {
                return Some("is not normalized: it has an empty or '.' segment".to_owned())
            }
            _ => segments += 1,
        }
    }
    (segments > MAX_PATH_SEGMENTS)
        .then(|| format!("exceeds maximum segment count ({MAX_PATH_SEGMENTS})"))
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

        let key = resolve_relative_segments(dir_segments, relative)?;
        // An alias resolves to the key of the module it names (`with_aliases`).
        Ok(match self.aliases.get(&key) {
            Some(target) => target.clone(),
            None => key,
        })
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

/// The refusal of a directory path, named by `shown`, that resolves to something other
/// than a directory (`mds::io`).
fn not_a_directory(shown: &str) -> MdsError {
    MdsError::io(format!("cannot resolve path {shown}: not a directory"))
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

    /// Canonicalize the directory `path`, taking a path with no final name (`.`, `..`,
    /// `sub/..`) as well as one with a final name, which is all
    /// [`check_symlink`](Self::check_symlink) takes (#413).
    ///
    /// A path with a final name (`src`, `src/`, `src/.`) is checked by `check_symlink`'s
    /// rule: its final component is judged by its own file type, so a trailing `/` or
    /// `/.` does not make it follow a link. A path with no final name is canonicalized as
    /// the operating system resolves it, and names no link: `link/..` is the directory
    /// above the link's target on Unix. Either way, a canonical form carrying a
    /// [`crate::is_forbidden_path_char`] codepoint — which a symlinked or hostile-named
    /// directory above the path can bring in — is refused with the same message, and
    /// the canonical form must be a directory. A filesystem root is accepted.
    ///
    /// Every message names `path` as passed, never its canonical form.
    ///
    /// # Errors
    ///
    /// - `MdsError::Io` — `path` carries a forbidden path character (`path contains
    ///   forbidden character U+XXXX: "<path>"`), checked before the filesystem is
    ///   touched; its canonical form carries one (`resolved path contains forbidden
    ///   character U+XXXX: "<path>"`); or it is not a directory (`cannot resolve path
    ///   <path>: not a directory`).
    /// - `MdsError::ImportError` — its final component is a symlink.
    /// - `MdsError::FileNotFound` — it does not resolve.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// let here = mds::NativeFs::check_directory(Path::new("."))?;
    /// assert_eq!(here, std::env::current_dir()?.canonicalize()?);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn check_directory(path: &Path) -> Result<PathBuf, MdsError> {
        let shown = path.display().to_string();
        reject_forbidden_path_chars("path", &shown)?;
        let canonical = if path.file_name().is_some() {
            Self::check_symlink_named(path, &shown)?
        } else {
            let canonical = path
                .canonicalize()
                .map_err(|_| MdsError::file_not_found(shown.clone()))?;
            reject_forbidden_in_path(&canonical, &shown)?;
            canonical
        };
        if !canonical.is_dir() {
            return Err(not_a_directory(&shown));
        }
        Ok(canonical)
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
                return Err(not_a_directory(shown));
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
    /// cap), refuses one whose last component is `..` as not found, joins with `dir`
    /// via `Path::join` (verbatim-path-safe on Windows; avoids PF-003 / #133), then
    /// runs `check_symlink_named` (which also refuses a forbidden character anywhere
    /// in the canonical path) and `check_path_traversal` before returning the
    /// canonical key string.
    ///
    /// Does NOT call `init_root` — only entry-point resolution
    /// ([`FileSystem::resolve_entry`]) anchors the security root.
    fn normalize_in_dir_impl(&self, dir: &Path, relative: &str) -> Result<String, MdsError> {
        validate_relative_import(relative)?;
        check_segment_count(relative)?;
        // A path whose last component is `..` (`../`, `./sub/..`, and `sub\..` on
        // Windows) names a directory, never a file: not found, on every OS (#414).
        // Decided on the path as written: on POSIX the joined path has no final name
        // either, but on Windows joining onto the verbatim (`\\?\`) canonical `dir`
        // collapses the `..` lexically and names that directory by its own name.
        if Path::new(relative).components().next_back() == Some(Component::ParentDir) {
            return Err(MdsError::file_not_found(relative));
        }
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
        // A module that is not a regular file is refused before it is opened; the size
        // is taken from the opened file, and the read itself stops one byte past the
        // cap, so neither a file swapped nor one grown after the check is held in
        // memory whole (#428).
        read_module_file(path, &display)
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

    // ── VirtualFs::with_aliases (#414) ────────────────────────────────────────

    /// A module map holding each of `keys`.
    fn modules_of(keys: &[&str]) -> HashMap<String, String> {
        keys.iter()
            .map(|k| ((*k).to_owned(), format!("{k}\n")))
            .collect()
    }

    /// An alias map of `(alias, module key)` pairs.
    fn aliases_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(a, t)| ((*a).to_owned(), (*t).to_owned()))
            .collect()
    }

    /// An import that resolves to an alias resolves to the module it names; any other
    /// import, and an entry key, resolves as without aliases.
    #[test]
    fn vfs_alias_resolves_an_import_to_the_module_it_names() {
        let modules = modules_of(&["main.mds", "sub/header.mds"]);
        let fs = VirtualFs::new(modules.clone())
            .with_aliases(aliases_of(&[("SUB/Header.mds", "sub/header.mds")]))
            .expect("a valid alias");
        assert_eq!(
            fs.normalize_in_dir("", "./SUB/Header.mds").unwrap(),
            "sub/header.mds"
        );
        assert_eq!(
            fs.normalize_in_dir("sub", "../SUB/Header.mds").unwrap(),
            "sub/header.mds"
        );
        assert_eq!(
            fs.normalize_in_dir("", "./sub/header.mds").unwrap(),
            "sub/header.mds"
        );
        assert_eq!(
            fs.resolve_entry("SUB/Header.mds").unwrap(),
            "SUB/Header.mds"
        );
        // Control: without the alias, the spelling is a key of its own.
        assert_eq!(
            VirtualFs::new(modules)
                .normalize_in_dir("", "./SUB/Header.mds")
                .unwrap(),
            "SUB/Header.mds"
        );
    }

    /// Every alias and every module key it names is checked as a key an import can
    /// resolve to — a hostile or malformed one is refused, never trusted from the
    /// spelling that led to it — a target must be a module and an alias must not be;
    /// the offending alias is named escaped (PF-013: the hostile character is shown as
    /// escape text, never raw).
    #[test]
    fn vfs_with_aliases_refuses_an_alias_that_is_not_a_module_key() {
        let esc = char::from_u32(0x1b).expect("U+001B is a char");
        let hostile = format!("a{esc}b.mds");
        let shown = format!("a{}u001Bb.mds", '\\');
        let long = ["s"; 257].join("/");
        let modules = modules_of(&["a.mds", "sub/b.mds"]);
        let cases: Vec<(String, String, String)> = vec![
            ("".into(), "a.mds".into(), "module alias \"\": the alias is empty".into()),
            (
                hostile.clone(),
                "a.mds".into(),
                format!("module alias \"{shown}\": the alias contains forbidden character U+001B"),
            ),
            ("../a.mds".into(), "a.mds".into(), "module alias \"../a.mds\": the alias has a '..' segment".into()),
            ("x/../a.mds".into(), "a.mds".into(), "module alias \"x/../a.mds\": the alias has a '..' segment".into()),
            (
                "x/./a.mds".into(),
                "a.mds".into(),
                "module alias \"x/./a.mds\": the alias is not normalized: it has an empty or '.' segment".into(),
            ),
            (
                "/A.mds".into(),
                "a.mds".into(),
                "module alias \"/A.mds\": the alias is not normalized: it has an empty or '.' segment".into(),
            ),
            (
                long.clone(),
                "a.mds".into(),
                format!("module alias \"{long}\": the alias exceeds maximum segment count (256)"),
            ),
            (
                "A.mds".into(),
                "../a.mds".into(),
                "module alias \"A.mds\": its module key \"../a.mds\" has a '..' segment".into(),
            ),
            (
                "A.mds".into(),
                hostile.clone(),
                format!("module alias \"A.mds\": its module key \"{shown}\" contains forbidden character U+001B"),
            ),
            (
                "A.mds".into(),
                "gone.mds".into(),
                "module alias \"A.mds\": its module key \"gone.mds\" names no module".into(),
            ),
            (
                "a.mds".into(),
                "sub/b.mds".into(),
                "module alias \"a.mds\": the alias is a module key itself".into(),
            ),
        ];
        for (alias, target, message) in cases {
            let err = VirtualFs::new(modules.clone())
                .with_aliases(HashMap::from([(alias.clone(), target.clone())]))
                .expect_err(&message);
            // The alias and the reason apart, as a binding words them.
            let ModuleAliasError::Refused {
                alias: ref shown_alias,
                ref reason,
            } = err
            else {
                panic!("{err:?}");
            };
            assert_eq!(format!("module alias \"{shown_alias}\": {reason}"), message);
            let err = MdsError::from(err);
            assert!(matches!(err, MdsError::Io { .. }), "{err:?}");
            assert_eq!(err.to_string(), message);
            assert!(!err.to_string().contains(esc), "{message}");
        }
        // Of several offending aliases, the first in key order is named.
        let err = VirtualFs::new(modules.clone())
            .with_aliases(aliases_of(&[("b.mds", "nope.mds"), ("A.mds", "gone.mds")]))
            .expect_err("two aliases name no module");
        assert_eq!(
            err.to_string(),
            "module alias \"A.mds\": its module key \"gone.mds\" names no module"
        );
        // No alias leads to another: one naming an alias names no module, and one
        // named by an alias is a module key, so either link of a chain is refused.
        for (chain, message) in [
            (
                aliases_of(&[("A.mds", "B.mds"), ("B.mds", "a.mds")]),
                "module alias \"A.mds\": its module key \"B.mds\" names no module",
            ),
            (
                aliases_of(&[("X.mds", "a.mds"), ("a.mds", "sub/b.mds")]),
                "module alias \"a.mds\": the alias is a module key itself",
            ),
        ] {
            let err = VirtualFs::new(modules.clone())
                .with_aliases(chain)
                .expect_err(message);
            assert_eq!(err.to_string(), message);
        }
        // Control: a clean alias to either module is accepted.
        VirtualFs::new(modules)
            .with_aliases(aliases_of(&[
                ("A.mds", "a.mds"),
                ("SUB/B.mds", "sub/b.mds"),
            ]))
            .expect("clean aliases");
    }

    /// #414: an alias map is bounded before any alias is checked, as the WASM `modules`
    /// option is: more than 65,536 aliases, or aliases and the module keys they name that
    /// together pass 10 MiB, is `mds::resource_limit`. A map at either bound is accepted
    /// (the controls), and one past a bound is refused so even when every alias in it
    /// would be refused too.
    #[test]
    fn vfs_with_aliases_bounds_the_count_and_the_size_of_the_map() {
        const COUNT: usize = 65_536;
        let size = usize::try_from(MAX_FILE_SIZE).expect("10 MiB fits a usize");
        let modules = modules_of(&["a.mds"]);
        let with = |aliases: HashMap<String, String>| {
            VirtualFs::new(modules.clone())
                .with_aliases(aliases)
                .map(drop)
                .map_err(MdsError::from)
        };
        let refused = |aliases: HashMap<String, String>, message: &str| {
            let err = with(aliases).expect_err(message);
            assert!(matches!(err, MdsError::ResourceLimit { .. }), "{err:?}");
            assert_eq!(
                err.to_string(),
                format!("resource limit exceeded: {message}")
            );
        };

        let many = |n: usize, target: &str| -> HashMap<String, String> {
            (0..n)
                .map(|i| (format!("A{i}.mds"), target.to_owned()))
                .collect()
        };
        with(many(COUNT, "a.mds")).expect("at the count bound");
        let too_many = format!(
            "module alias count exceeds maximum of {COUNT} ({} provided)",
            COUNT + 1
        );
        refused(many(COUNT + 1, "a.mds"), &too_many);
        refused(many(COUNT + 1, "gone.mds"), &too_many);

        // One alias whose key and module key total `total` bytes.
        let sized = |total: usize, target: &str| {
            let alias = format!("{}.mds", "x".repeat(total - target.len() - ".mds".len()));
            HashMap::from([(alias, target.to_owned())])
        };
        with(sized(size, "a.mds")).expect("at the size bound");
        let too_large = format!("module aliases aggregate size exceeds maximum of {size} bytes");
        refused(sized(size + 1, "a.mds"), &too_large);
        refused(sized(size + 1, "gone.mds"), &too_large);

        // The typed refusals, and the constants they name.
        assert_eq!(COUNT, MAX_MODULE_ALIASES);
        assert_eq!(size, MAX_MODULE_ALIASES_SIZE);
        let typed = |aliases| {
            VirtualFs::new(modules.clone())
                .with_aliases(aliases)
                .map(drop)
        };
        assert_eq!(
            typed(many(COUNT + 1, "a.mds")),
            Err(ModuleAliasError::TooMany { count: COUNT + 1 })
        );
        assert_eq!(
            typed(sized(size + 1, "a.mds")),
            Err(ModuleAliasError::TooLarge)
        );
    }

    /// The key rule is exactly the form `normalize_in_dir` gives a key: a key passes it
    /// if and only if resolving it from the key-space root returns it unchanged and it
    /// carries no forbidden path character.
    #[test]
    fn vfs_module_key_rule_is_the_form_an_import_resolves_to() {
        let at_cap = ["s"; 256].join("/");
        let over_cap = ["s"; 257].join("/");
        let hostile = format!("a{}b", char::from_u32(0x202e).expect("U+202E is a char"));
        let keys = [
            "a.mds",
            "sub/a.mds",
            "a b/\u{e9}.mds",
            "back\\slash.mds",
            "",
            ".",
            "..",
            "./a.mds",
            "a/./b",
            "a/../b",
            "a//b",
            "/a",
            "a/",
            "../a",
            &at_cap,
            &over_cap,
            &hostile,
        ];
        for key in keys {
            let resolves_to_itself = first_forbidden_char(key).is_none()
                && resolve_relative_segments(Vec::new(), key).ok().as_deref() == Some(key);
            assert_eq!(
                module_key_violation(key).is_none(),
                resolves_to_itself,
                "{key:?}: {:?}",
                module_key_violation(key)
            );
        }
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

    // ── check_module_bytes: the one post-read check (#414) ───────────────────

    #[test]
    fn check_module_bytes_returns_the_text_keeping_a_bom() {
        assert_eq!(
            check_module_bytes(b"Hello!\n".to_vec(), "a.mds").unwrap(),
            "Hello!\n"
        );
        let bom = [&[0xef, 0xbb, 0xbf][..], b"Hi\n"].concat();
        assert_eq!(
            check_module_bytes(bom, "a.mds").unwrap(),
            format!("{}Hi\n", char::from_u32(0xfeff).unwrap())
        );
        assert_eq!(check_module_bytes(Vec::new(), "a.mds").unwrap(), "");
    }

    #[test]
    fn check_module_bytes_refuses_one_byte_over_the_cap() {
        let at_cap = vec![b'x'; MAX_FILE_SIZE as usize];
        assert_eq!(
            check_module_bytes(at_cap, "big.mds").unwrap().len(),
            MAX_FILE_SIZE as usize
        );
        let err =
            check_module_bytes(vec![b'x'; MAX_FILE_SIZE as usize + 1], "big.mds").unwrap_err();
        assert_eq!(code_of(&err).as_deref(), Some("mds::resource_limit"));
        assert_eq!(
            err.to_string(),
            format!(
                "resource limit exceeded: file too large ({} bytes, max {MAX_FILE_SIZE} bytes): big.mds",
                MAX_FILE_SIZE + 1
            )
        );
    }

    #[test]
    fn check_module_bytes_refuses_invalid_utf8_with_the_std_reason() {
        for (bytes, reason) in [
            (
                vec![b'h', b'i', 0xff, b'\n'],
                "invalid utf-8 sequence of 1 bytes from index 2",
            ),
            // An incomplete sequence at the very end (the first two bytes of U+20AC).
            (
                vec![b'h', b'i', b'\n', 0xe2, 0x82],
                "incomplete utf-8 byte sequence from index 3",
            ),
        ] {
            let err = check_module_bytes(bytes, "sub/bad.mds").unwrap_err();
            assert_eq!(code_of(&err).as_deref(), Some("mds::io"));
            assert_eq!(
                err.to_string(),
                format!("invalid UTF-8 in sub/bad.mds: {reason}")
            );
        }
    }

    /// The display is escaped as it enters a message; a clean one is unchanged (the
    /// cases above).
    #[test]
    fn check_module_bytes_escapes_the_display() {
        let esc = char::from_u32(0x1b).unwrap();
        let hostile = format!("a{esc}b.mds");
        let err = check_module_bytes(vec![0xff], &hostile).unwrap_err();
        let message = err.to_string();
        assert!(!message.contains(esc), "raw ESC in {message:?}");
        assert!(
            message.starts_with(&format!("invalid UTF-8 in a{}u001Bb.mds: ", '\\')),
            "{message:?}"
        );
    }

    /// NativeFs::read refuses exactly what check_module_bytes refuses, naming the file
    /// by its path below the project root.
    #[test]
    fn native_read_refuses_invalid_utf8_as_check_module_bytes_does() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(".mdsroot"), "").unwrap();
        let bytes = vec![b'h', b'i', 0xff, b'\n'];
        let path = dir.path().join("bad.mds");
        std::fs::write(&path, &bytes).unwrap();

        let fs = NativeFs::new();
        let key = fs.resolve_entry(&path.display().to_string()).unwrap();
        let err = fs.read(&key).unwrap_err();
        let expected = check_module_bytes(bytes, "bad.mds").unwrap_err();
        assert_eq!(code_of(&err), code_of(&expected));
        assert_eq!(err.to_string(), expected.to_string());
        assert_eq!(
            err.to_string(),
            "invalid UTF-8 in bad.mds: invalid utf-8 sequence of 1 bytes from index 2"
        );
    }

    /// `cannot read` escapes the display it names, as the size and UTF-8 refusals do,
    /// whether the file is missing or not a regular file; a clean one is unchanged.
    #[test]
    fn read_module_file_escapes_the_display_in_cannot_read() {
        let dir = TempDir::new().unwrap();
        let esc = char::from_u32(0x1b).unwrap();
        let hostile = format!("a{esc}b.mds");
        let missing = dir.path().join("missing.mds");
        let gone = std::fs::metadata(&missing).unwrap_err().to_string();
        let mut mismatches = Vec::new();
        for (path, reason) in [
            (missing, gone),
            (dir.path().to_path_buf(), "not a regular file".to_string()),
        ] {
            for (display, named) in [
                (hostile.as_str(), format!("a{}u001Bb.mds", '\\')),
                ("clean.mds", "clean.mds".to_string()),
            ] {
                let got = read_module_file(&path, display).map_err(|e| e.to_string());
                if got != Err(format!("cannot read {named}: {reason}")) {
                    mismatches.push(format!("{display:?} at {}: {got:?}", path.display()));
                }
            }
        }
        assert!(mismatches.is_empty(), "{mismatches:#?}");
    }

    /// Resolve `entry` as the resolver resolves an entry, then read it on a thread of
    /// its own: its display name, and its text or its error's code and message — or
    /// "still blocked after 10 s" for a read that has not returned by then, which a
    /// detached thread then unblocks by opening `entry` for writing, as a FIFO's writer
    /// would. Nothing waits for either thread, so no read can hang the test.
    fn read_bounded(entry: &Path) -> (String, Result<String, String>) {
        let fs = std::sync::Arc::new(NativeFs::new());
        let key = fs
            .resolve_entry(&entry.display().to_string())
            .expect("the entry resolves");
        let display = fs.display_of(Path::new(&key));
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::sync::Arc::clone(&fs);
        std::thread::spawn(move || {
            let read = reader.read(&key).map_err(|e| {
                let code = code_of(&e).unwrap_or_default();
                format!("{code}: {e}")
            });
            let _ = tx.send(read);
        });
        let outcome = match rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(read) => read,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let fifo = entry.to_path_buf();
                std::thread::spawn(move || {
                    drop(std::fs::OpenOptions::new().write(true).open(fifo));
                });
                Err("still blocked after 10 s".to_string())
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err("the read panicked".to_string())
            }
        };
        (display, outcome)
    }

    /// A module that is not a regular file is refused before it is opened, with the
    /// refusal `@mdscript/mds`'s WASM backend makes (#428): opening a FIFO nobody writes
    /// to blocked, a device reporting no size was read to one byte past the cap, and a
    /// directory failed with the operating system's own text.
    #[test]
    fn native_read_refuses_a_module_that_is_not_a_regular_file_before_opening_it() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(".mdsroot"), "").unwrap();
        std::fs::write(dir.path().join("ok.mds"), "Hi\n").unwrap();
        std::fs::create_dir(dir.path().join("dir.mds")).unwrap();
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut refused = vec![dir.path().join("dir.mds")];
        #[cfg(unix)]
        {
            let fifo = dir.path().join("fifo.mds");
            let made = std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .expect("mkfifo runs");
            assert!(made.success(), "mkfifo {}", fifo.display());
            refused.push(fifo);
            refused.push(PathBuf::from("/dev/zero"));
        }
        let mut mismatches = Vec::new();
        for entry in &refused {
            let (display, outcome) = read_bounded(entry);
            let expected = format!("mds::io: cannot read {display}: not a regular file");
            if outcome.as_ref() != Err(&expected) {
                mismatches.push(format!("{}: {outcome:?}", entry.display()));
            }
        }
        // Control: a regular file is read, named as the others are.
        let (display, outcome) = read_bounded(&dir.path().join("ok.mds"));
        if (display.as_str(), outcome.as_deref()) != ("ok.mds", Ok("Hi\n")) {
            mismatches.push(format!("ok.mds ({display}): {outcome:?}"));
        }
        assert!(mismatches.is_empty(), "{mismatches:#?}");
    }

    // ── read_at_most: the bounded read (#428) ─────────────────────────────────

    /// The reads a [`Scripted`] reader serves before it fails with its own error: far
    /// more than any case below needs, so a loop that never gives up ends the test
    /// instead of hanging it.
    const READ_BUDGET: usize = 100_000;

    /// A fake reader: its `n`th read follows `script[n - 1]` — `None` is an interrupted
    /// read, `Some(k)` a read of up to `k` bytes — and every read after the script
    /// follows `then`. It counts its reads.
    struct Scripted {
        script: Vec<Option<usize>>,
        then: Option<usize>,
        reads: usize,
    }

    impl Scripted {
        fn new(script: Vec<Option<usize>>, then: Option<usize>) -> Self {
            Scripted {
                script,
                then,
                reads: 0,
            }
        }
    }

    impl std::io::Read for Scripted {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.reads += 1;
            if self.reads > READ_BUDGET {
                return Err(std::io::Error::other("read budget spent"));
            }
            let step = self.script.get(self.reads - 1).copied();
            match step.unwrap_or(self.then) {
                None => Err(std::io::ErrorKind::Interrupted.into()),
                Some(k) => {
                    let n = k.min(buf.len());
                    buf[..n].fill(b'x');
                    Ok(n)
                }
            }
        }
    }

    /// `n` interrupted reads in a row.
    fn interruptions(n: usize) -> Vec<Option<usize>> {
        vec![None; n]
    }

    /// Up to 64 reads in a row may be interrupted; the 65th ends the read with that
    /// error, and a read that returns bytes starts the count again. Every case runs on
    /// every OS, and each is judged before anything is asserted.
    #[test]
    fn read_at_most_gives_up_after_64_interrupted_reads_in_a_row() {
        let cases = [
            (
                "64 interrupted, then data",
                [interruptions(64), vec![Some(3), Some(0)]].concat(),
                Some(0),
            ),
            (
                "65 interrupted, then data",
                [interruptions(65), vec![Some(3)]].concat(),
                Some(0),
            ),
            ("interrupted without end", Vec::new(), None),
            (
                "64 interrupted, a byte, 64 interrupted, a byte",
                [
                    interruptions(64),
                    vec![Some(1)],
                    interruptions(64),
                    vec![Some(1)],
                ]
                .concat(),
                Some(0),
            ),
        ];
        let outcomes: Vec<(&str, Result<usize, std::io::ErrorKind>, usize)> = cases
            .into_iter()
            .map(|(case, script, then)| {
                let mut reader = Scripted::new(script, then);
                let outcome = read_at_most(&mut reader, 1024, 0)
                    .map(|bytes| bytes.len())
                    .map_err(|e| e.kind());
                (case, outcome, reader.reads)
            })
            .collect();
        let interrupted = std::io::ErrorKind::Interrupted;
        assert_eq!(
            outcomes,
            [
                ("64 interrupted, then data", Ok(3), 66),
                ("65 interrupted, then data", Err(interrupted), 65),
                ("interrupted without end", Err(interrupted), 65),
                ("64 interrupted, a byte, 64 interrupted, a byte", Ok(2), 131),
            ]
        );
    }

    /// The buffer never holds more than `limit`, whatever the chunks a source returns
    /// and however far it runs past its size hint — a file that grows after its size
    /// was taken, or reports none — and no read asks for a byte past `limit`. A source
    /// of exactly its size hint is read into the first buffer, without growing it.
    #[test]
    fn read_at_most_never_grows_its_buffer_past_limit() {
        // (case, bytes per read, size hint, limit); every source is endless.
        let cases = [
            ("1-byte reads, no size hint", 1, 0, 10_000),
            ("8 KiB reads, no size hint", 8192, 0, 100_000),
            ("8 KiB reads past a hint of 100", 8192, 100, 100_000),
            ("8 KiB reads, hint over limit", 8192, 1_000_000, 100_000),
        ];
        let mut mismatches = Vec::new();
        for (case, chunk, size_hint, limit) in cases {
            let mut reader = Scripted::new(Vec::new(), Some(chunk));
            match read_at_most(&mut reader, limit as u64, size_hint) {
                Ok(bytes) if bytes.len() == limit && bytes.capacity() <= limit => {}
                Ok(bytes) => mismatches.push(format!(
                    "{case}: len {}, capacity {} (limit {limit})",
                    bytes.len(),
                    bytes.capacity()
                )),
                Err(e) => mismatches.push(format!("{case}: {e}")),
            }
            // Every read returned `chunk` bytes but the last, which asked for the rest.
            if reader.reads != limit.div_ceil(chunk) {
                mismatches.push(format!("{case}: {} reads", reader.reads));
            }
        }
        // A source of exactly its size hint: the first buffer — the hint plus the one
        // byte that sees its end — is never grown.
        let mut reader = Scripted::new(vec![Some(5000)], Some(0));
        match read_at_most(&mut reader, 100_000, 5000) {
            Ok(bytes) if bytes.len() == 5000 && bytes.capacity() == 5001 => {}
            Ok(bytes) => mismatches.push(format!(
                "exactly its size hint: len {}, capacity {}",
                bytes.len(),
                bytes.capacity()
            )),
            Err(e) => mismatches.push(format!("exactly its size hint: {e}")),
        }
        assert!(mismatches.is_empty(), "{mismatches:#?}");
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
    fn native_normalize_in_dir_import_ending_in_dot_dot_is_not_found() {
        // An import whose last component is `..` names a directory, never a file:
        // `file not found: <import as written>` on every OS, as the WASM pre-scanner
        // reports it (#414). The directories are the canonical keys' parents the
        // resolver passes — verbatim (`\\?\`) paths on Windows, where joining collapses
        // `..` lexically and so used to name, and open, the directory it leads to.
        let dir = TempDir::new().unwrap();
        let entry = make_temp_file(&dir, "main.mds", "hello");
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let fs = NativeFs::new();
        let root = fs.parent_dir(&fs.resolve_entry(&entry.display().to_string()).unwrap());
        let sub = Path::new(&root).join("sub").display().to_string();

        let mut rows = vec![
            (sub.as_str(), "../"),
            (sub.as_str(), ".."),
            (root.as_str(), "./sub/.."),
            (root.as_str(), "sub/../"),
            (root.as_str(), "./sub/../."),
        ];
        if cfg!(windows) {
            rows.push((root.as_str(), r".\sub\.."));
        }
        let mismatches: Vec<String> = rows
            .iter()
            .filter_map(|&(from, relative)| {
                let expected = format!("file not found: {relative}");
                match fs.normalize_in_dir(from, relative) {
                    Err(err @ MdsError::FileNotFound { .. }) if err.to_string() == expected => None,
                    other => Some(format!("{relative:?} from {from:?}: {other:?}")),
                }
            })
            .collect();
        assert!(mismatches.is_empty(), "{mismatches:#?}");

        // Control: a `..` that is not the last component resolves, so the refusal
        // above is the final `..`, not any `..`.
        let key = fs.normalize_in_dir(&sub, "../main.mds").unwrap();
        assert_eq!(Path::new(&key), entry.canonicalize().unwrap());
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

    // ── NativeFs::check_directory ─────────────────────────────────────────────

    /// #413: a directory resolves to its canonical form whether the path has a final
    /// name (`d`, `d/`, `d/.`) or not (`d/sub/..`, `d/..`, `.`), and every refusal names
    /// the path as passed: missing is not found, a file is not a directory, a symlinked
    /// final component is a symlink however it is spelled, and `link/..` names no link.
    /// A canonical form carrying a forbidden character is refused with one message for
    /// a named path and one with no final name (Unix: a Windows name cannot hold TAB).
    #[test]
    fn check_directory_takes_a_path_with_or_without_a_final_name() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("d").join("sub")).unwrap();
        std::fs::write(root.join("f.mds"), "x").unwrap();
        let at = |rel: &str| root.join(rel);
        let not_found = |p: &Path| Err(format!("file not found: {}", p.display()));
        let symlink = |p: &Path| {
            Err(format!(
                "import error: symlinks are not allowed in imports: {}",
                p.display()
            ))
        };

        let mut rows: Vec<(PathBuf, Result<PathBuf, String>)> = vec![
            (at("d"), Ok(at("d"))),
            (at("d/"), Ok(at("d"))),
            (at("d/."), Ok(at("d"))),
            (at("d/sub/.."), Ok(at("d"))),
            (at("d/.."), Ok(root.clone())),
            (
                PathBuf::from("."),
                Ok(std::env::current_dir().unwrap().canonicalize().unwrap()),
            ),
            (at("gone"), not_found(&at("gone"))),
            (
                at("f.mds"),
                Err(format!(
                    "cannot resolve path {}: not a directory",
                    at("f.mds").display()
                )),
            ),
        ];
        // Windows resolves `gone\..` lexically, to the directory above it.
        #[cfg(unix)]
        rows.push((at("gone/.."), not_found(&at("gone/.."))));
        if make_symlink(&at("d"), &at("link")) {
            rows.extend([
                (at("link"), symlink(&at("link"))),
                (at("link/"), symlink(&at("link/"))),
                (at("link/."), symlink(&at("link/."))),
                (at("link/.."), Ok(root.clone())),
            ]);
        }
        #[cfg(unix)]
        {
            std::fs::create_dir_all(root.join("x\ty").join("sub")).unwrap();
            assert!(make_symlink(&at("x\ty"), &at("clink")));
            let hostile = |p: &Path| {
                Err(format!(
                    "resolved path contains forbidden character U+0009: \"{}\"",
                    p.display()
                ))
            };
            rows.extend([
                (at("clink/sub"), hostile(&at("clink/sub"))),
                (at("clink/sub/.."), hostile(&at("clink/sub/.."))),
                (at("clink"), symlink(&at("clink"))),
            ]);
            let typed = at(&format!("d{}e", '\x1b'));
            let shown = format!("{}", at("d").display()) + &format!("{}u001Be", '\\');
            rows.push((
                typed,
                Err(format!(
                    "path contains forbidden character U+001B: \"{shown}\""
                )),
            ));
        }

        let mut mismatches = Vec::new();
        for (path, want) in rows {
            let got = NativeFs::check_directory(&path).map_err(|e| e.to_string());
            if got != want {
                mismatches.push(format!("{path:?}: got {got:?}, want {want:?}"));
            }
        }
        assert!(mismatches.is_empty(), "{mismatches:#?}");
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
