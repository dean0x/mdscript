//! The one write primitive: every file `mds` writes goes through [`atomic_write_file`] —
//! `mds build` and `mds watch` outputs and `.map` sidecars, `mds fmt` and `mds lint
//! --fix` rewrites, and `mds init`'s starter (#227, #160). `tests/write_funnel.rs` keeps
//! it the only one.
//!
//! # Replace by rename
//!
//! The bytes go to a temporary file beside the target, which is then renamed over it: a
//! crash, a kill or a full disk never leaves a truncated file, and a reader sees the whole
//! old file or the whole new one. [`Durability`] says whether the bytes, and the rename,
//! are also forced to stable storage first.
//!
//! # Below the anchor (#160)
//!
//! A [`WriteTarget`] names its file below an anchor: the parent of a file argument or of
//! `-o`, `--out-dir`, a directory argument's root, or — for `mds.json`'s
//! `build.output_dir`, which the repository names and the user does not — the directory
//! `mds.json` is in, with `build.output_dir`'s own directories below it. The anchor is
//! resolved by path, in the form the run holds it — as typed, or, for a directory-mode
//! `--out-dir` and the anchors `mds watch` derives from its entry or directory argument,
//! as resolved once when the run started — so a symlinked anchor is followed (a
//! symlinked directory argument is refused before anything is written, #413); `mds
//! watch` refuses a write whose out-dir, as the user named it, now leads to another
//! directory ([`out_dir_moved`]), and names the directory its check found to the write,
//! which refuses an anchor it opens that is not that one — on unix by the descriptor it
//! opened ([`DirIdentity`]). Nothing
//! below it is: each directory is opened from the one above it without following a
//! symlink, and the file is created, checked and renamed in the last one. A symlink
//! planted below the anchor, or swapped in while the write runs, is refused (`mds::io`,
//! exit 2) by the path the user knows it by, and nothing is written through it. The walk
//! is made for every write and nothing is held open between writes, so a directory
//! replaced between two writes is the one the next write finds.
//!
//! On unix the walk is `openat(O_DIRECTORY | O_NOFOLLOW)` from the anchor's descriptor
//! (`mkdirat` first for a directory an output needs), then `fstatat(AT_SYMLINK_NOFOLLOW)` on
//! the target — never an open of it, which a FIFO would block, and a FIFO, a socket or a
//! device is refused — the temporary file `openat(O_CREAT | O_EXCL | O_NOFOLLOW)` beside it —
//! unlinked again if anything after that fails — `fchmod` to the mode of the file it
//! replaces, the durability tier's syncs, and `renameat` (`mod unix`).
//!
//! Windows has no descriptor-relative walk in std: each directory below the anchor is
//! checked with `symlink_metadata` and refused when it is a symlink or a junction — the
//! name-surrogate reparse points std's `is_symlink` reports; a cloud-sync placeholder is
//! not one and stays writable — and the write then goes by path (`mod windows`); the
//! anchor `mds watch` checked is compared by path too, just before the walk. A
//! directory swapped for a link between those checks and the write is followed: the
//! residual SECURITY.md and spec §7.2 document.
//!
//! # Contract (#226)
//!
//! This is replace-by-rename, not an in-place rewrite. The target receives a NEW inode, so
//! the write does NOT preserve hard links (other links keep the old content), ACLs,
//! extended attributes (xattrs), or owner/group of the original file; only the permission
//! bits are carried over (unix). Hard-link preservation is out of scope by construction (it
//! would require truncate-in-place and forfeit crash safety); ACL/xattr/owner-group
//! preservation is not planned — MDS only rewrites its own outputs and `.mds` sources.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use crate::output::{io_cause, safe_inline, safe_path, WriteTarget};

/// How hard [`atomic_write_file`] works to make the new bytes survive a crash.
///
/// Atomicity — a reader sees either the whole old file or the whole new one, never a
/// truncated mix — is unconditional: it comes from the rename, not from a sync. This knob
/// only chooses whether the data is forced to stable storage *before* that rename, and the
/// rename itself after it.
///
/// The split exists because the two families of file MDS writes have different recovery
/// costs, and on macOS `sync_all()` is `F_FULLFSYNC` — a full drive cache flush, ~7 ms per
/// file. Measured on a 500-template `mds watch` startup (#227): 1.44 s → 4.69 s, and the
/// `cli_watch` suite 4.2 s → 8.3 s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Durability {
    /// `sync_all()` on the temporary file before the rename and, on unix, on its directory
    /// after it — where the filesystem can sync a directory at all: one that refuses it
    /// does not fail a write that has landed. For files whose content exists nowhere else:
    /// `mds fmt` and `mds lint --fix` rewrite the user's hand-authored `.mds` source in
    /// place, so bytes lost to a power failure are lost for good.
    Fsync,
    /// Rename only. For **derived** artifacts — compiled outputs and `.map` sidecars —
    /// which are reproducible by re-running `mds build`, and `mds init`'s fixed starter. A
    /// crash can leave the previous file or an unflushed new one; either way the fix is one
    /// more run, and paying `F_FULLFSYNC` per file to avoid it costs more than it saves.
    RenameOnly,
}

/// Whether a write creates the directories its file goes in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Parents {
    /// A compiled output or a `.map` sidecar: a missing anchor is created by path, as typed,
    /// and a missing directory below it inside the anchor, the first time a write needs it.
    Create,
    /// A rewrite of a file that was just read (`mds fmt`, `mds lint --fix`), or `mds
    /// init`'s starter: the directory must already be there, and nothing is created.
    Existing,
}

/// One directory as the filesystem tells it from another at the same path: its device
/// and inode on unix, with its birth time where the filesystem keeps one, since a
/// directory made where a deleted one was can be given the freed inode; its creation time
/// alone on Windows, where std gives no file index. `mds watch` checks its out-dir by it
/// before each write below it, and the write is made only in the directory the check
/// found ([`WriteTarget::below_checked_anchor`], #160).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DirIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    created: Option<std::time::SystemTime>,
}

impl DirIdentity {
    /// The directory at `path`, through a symlink; `None` when there is none.
    pub(crate) fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok().filter(|meta| meta.is_dir())?;
        Some(Self::of_metadata(&meta))
    }

    /// The directory `meta` describes.
    fn of_metadata(meta: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt as _;

        Self {
            #[cfg(unix)]
            dev: meta.dev(),
            #[cfg(unix)]
            ino: meta.ino(),
            created: meta.created().ok(),
        }
    }
}

/// Write `content` to `target` atomically, below its anchor and through no symlink there
/// (see the module docs).
///
/// A symlink at the target itself — live or dangling — is refused rather than replaced,
/// and so, on unix, is a FIFO, a socket or a device, which the write never opens.
/// An existing file keeps its permission bits (unix); a new one is created with mode
/// `0666 & !umask`, as `std::fs::write` creates one. `parents` says whether missing
/// directories are created; `durability` whether the write is synced.
///
/// # Errors
///
/// Every failure is `mds::io` (exit 2, #157), worded `cannot write <file>: <cause>`
/// whichever step failed (#309): the file named by `target.shown`, the form the caller's
/// status line names it by, never by `target.path` (#390), and the cause naming no path
/// ([`io_cause`]). A symlink refused below the anchor is named instead, below the anchor's
/// shown form — `cannot write out/sub: refusing to follow a symlink` — and one at the
/// target says `refusing to replace a symlink`; a FIFO, a socket or a device there says
/// `not a regular file`; an anchor that is not the directory the caller checked
/// ([`WriteTarget::below_checked_anchor`]) is refused as [`out_dir_moved`] words it,
/// before anything below it is opened.
pub(crate) fn atomic_write_file(
    target: &WriteTarget,
    content: &str,
    durability: Durability,
    parents: Parents,
) -> std::result::Result<(), mds::MdsError> {
    let below = Below::of(target).map_err(|e| io_error(&target.shown, io_cause(&e)))?;
    let anchor = target.checked_anchor();
    imp::write(&below, anchor, content.as_bytes(), durability, parents).map_err(|failure| {
        match failure {
            Failure::AnchorMoved => out_dir_moved(target),
            Failure::LinkBelowAnchor { depth } => {
                io_error(&shown_directory(target, depth), FOLLOW_REFUSAL.to_owned())
            }
            Failure::LinkAtTarget => io_error(&target.shown, SYMLINK_REFUSAL.to_owned()),
            #[cfg(unix)]
            Failure::NotARegularFile => io_error(&target.shown, NOT_A_REGULAR_FILE.to_owned()),
            Failure::Io(e) => io_error(&target.shown, io_cause(&e)),
        }
    })
}

/// Why [`atomic_write_file`] refuses to replace a symlink at its target.
const SYMLINK_REFUSAL: &str = "refusing to replace a symlink";

/// Why [`atomic_write_file`] refuses to replace a FIFO, a socket or a device at its target.
#[cfg(unix)]
const NOT_A_REGULAR_FILE: &str = "not a regular file";

/// Why [`atomic_write_file`] refuses a symlink at a directory below the anchor.
const FOLLOW_REFUSAL: &str = "refusing to follow a symlink";

/// Why a `mds watch` session refuses a write below an out-dir that now leads elsewhere.
const OUT_DIR_MOVED: &str = "the output directory now resolves to a different directory; \
                             restart mds watch to follow it";

/// The `mds::io` refusal of a write below an out-dir whose path, as the user named it,
/// now leads to a different directory than the one the `mds watch` session started with
/// (#160): `cannot write <file>: …`, the file named by `target.shown`, as every failure
/// to write it is.
pub(crate) fn out_dir_moved(target: &WriteTarget) -> mds::MdsError {
    io_error(&target.shown, OUT_DIR_MOVED.to_owned())
}

/// The `mds::io` error for a write that failed: `shown` — the file, or the directory below
/// the anchor that was a symlink — escaped (#390), then the cause, escaped too.
fn io_error(shown: &Path, cause: String) -> mds::MdsError {
    mds::MdsError::Io {
        message: format!("cannot write {}: {}", safe_path(shown), safe_inline(cause)),
    }
}

/// `target`'s shown form cut after the directory `depth` levels below the anchor: the
/// directory a refused symlink is, as the user knows it. `shown` ends in the same names
/// below the anchor as `path` does, so the cut is counted from its end.
fn shown_directory(target: &WriteTarget, depth: usize) -> PathBuf {
    let components: Vec<Component<'_>> = target.shown.components().collect();
    match components.len().checked_sub(target.below_anchor()) {
        Some(above) if above + depth < components.len() => {
            components[..=above + depth].iter().collect()
        }
        _ => target.shown.clone(),
    }
}

/// What failed in a write, before it is worded.
#[derive(Debug)]
enum Failure {
    /// The anchor opened is not the directory the caller checked.
    AnchorMoved,
    /// The directory `depth` levels below the anchor is a symlink.
    LinkBelowAnchor { depth: usize },
    /// The target itself is a symlink.
    LinkAtTarget,
    /// The target itself is a FIFO, a socket or a device.
    #[cfg(unix)]
    NotARegularFile,
    /// Anything else.
    Io(std::io::Error),
}

impl From<std::io::Error> for Failure {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

#[cfg(unix)]
impl From<rustix::io::Errno> for Failure {
    fn from(e: rustix::io::Errno) -> Self {
        Self::Io(e.into())
    }
}

/// A target split at its anchor: the directory the write resolves by path, the directories
/// below it in order, and the file's name.
#[derive(Debug, PartialEq, Eq)]
struct Below<'a> {
    anchor: PathBuf,
    dirs: Vec<&'a OsStr>,
    name: &'a OsStr,
}

impl<'a> Below<'a> {
    /// `target`'s last [`WriteTarget::below_anchor`] components, each a plain name — never
    /// `..` or `.` — and the anchor above them (`.` when that is nothing).
    ///
    /// # Errors
    ///
    /// "is a directory" when the last component is not a name (`.`, `..`, a root: a
    /// directory, never a file) or the path ends in a separator, or in a separator and a
    /// `.` (`out/`, `out/.`), which `Path::components` drops; "invalid input" when one above
    /// it is not a name, or when the path has fewer components than the target says lie
    /// below its anchor.
    fn of(target: &'a WriteTarget) -> std::io::Result<Self> {
        if ends_as_a_directory(&target.path) {
            return Err(std::io::ErrorKind::IsADirectory.into());
        }
        let components: Vec<Component<'a>> = target.path.components().collect();
        let below = target.below_anchor();
        let above = components
            .len()
            .checked_sub(below)
            .filter(|_| below > 0)
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        let (anchor, under) = components.split_at(above);
        let Some((last, dirs)) = under.split_last() else {
            return Err(std::io::ErrorKind::InvalidInput.into());
        };
        let Component::Normal(name) = *last else {
            return Err(std::io::ErrorKind::IsADirectory.into());
        };
        let dirs = dirs
            .iter()
            .map(|component| match component {
                Component::Normal(dir) => Ok(*dir),
                _ => Err(std::io::Error::from(std::io::ErrorKind::InvalidInput)),
            })
            .collect::<std::io::Result<Vec<&'a OsStr>>>()?;
        let anchor = if anchor.is_empty() {
            PathBuf::from(".")
        } else {
            anchor.iter().collect()
        };
        Ok(Self { anchor, dirs, name })
    }
}

/// Whether `path` ends in a separator, or in a separator and a `.`: the spelling of a
/// directory, whatever the name before it.
fn ends_as_a_directory(path: &Path) -> bool {
    // A separator is ASCII on every platform, and no byte of a multi-byte character is.
    let separator = |byte: &u8| std::path::is_separator(char::from(*byte));
    match path.as_os_str().as_encoded_bytes() {
        [.., last] if separator(last) => true,
        [.., before, b'.'] => separator(before),
        _ => false,
    }
}

/// The name every temporary file starts with, so a crash's leftover is recognisable.
const TEMP_PREFIX: &str = ".mds-tmp-";

/// The extension every temporary file ends with: never `mds`, so no directory walk takes
/// one for a source.
const TEMP_SUFFIX: &str = ".tmp";

#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
use windows as imp;

#[cfg(unix)]
mod unix {
    use std::ffi::{OsStr, OsString};
    use std::fs::File;
    use std::io::Write as _;
    use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
    use std::path::Path;

    use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, CWD};
    use rustix::io::Errno;

    use super::{Below, DirIdentity, Durability, Failure, Parents, TEMP_PREFIX, TEMP_SUFFIX};

    /// The anchor: a directory, resolved by path — through a symlink the user named.
    const ANCHOR: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::CLOEXEC);

    /// A directory below the anchor: never through a symlink.
    const BELOW: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);

    /// The temporary file: a new file, never through a symlink.
    const TEMP: OFlags = OFlags::WRONLY
        .union(OFlags::CREATE)
        .union(OFlags::EXCL)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);

    /// The mode a new file asks for: `0666`, which the umask then narrows, as for
    /// `std::fs::write`.
    const NEW_FILE: Mode = Mode::from_raw_mode(0o666);

    /// The mode a directory a write creates asks for: `0777`, narrowed by the umask.
    const NEW_DIR: Mode = Mode::from_raw_mode(0o777);

    /// How many temporary names a write tries before it gives up: each is random, so a
    /// clash is another writer's file or a leftover, and sixteen in a row is not chance.
    pub(super) const MAX_TEMP_ATTEMPTS: usize = 16;

    /// Write `content` to `below.name`: open the anchor by path — and, when `anchor` names
    /// the directory it must be, refuse another — then each directory below it from the
    /// one above without following a symlink, and replace the file in the last.
    pub(super) fn write(
        below: &Below<'_>,
        anchor: Option<DirIdentity>,
        content: &[u8],
        durability: Durability,
        parents: Parents,
    ) -> Result<(), Failure> {
        let mut dir = open_anchor(&below.anchor, parents)?;
        if let Some(checked) = anchor {
            dir = opened_as_checked(dir, checked)?;
        }
        for (depth, name) in below.dirs.iter().enumerate() {
            let next = open_below(dir.as_fd(), name, parents)
                .map_err(|errno| below_failure(dir.as_fd(), name, depth, errno))?;
            dir = next;
        }
        replace(dir, below.name, content, durability)
    }

    /// Open the anchor by path, creating it first when it is missing and `parents` says so.
    ///
    /// A missing anchor that cannot be created because a name on its path is taken has a
    /// symlink that leads nowhere there (an existing file fails the open as not a directory
    /// instead): it is reported as the missing directory it is, not as the name creating
    /// it found.
    fn open_anchor(anchor: &Path, parents: Parents) -> Result<OwnedFd, Failure> {
        match fs::openat(CWD, anchor, ANCHOR, Mode::empty()) {
            Err(Errno::NOENT) if parents == Parents::Create => {
                std::fs::create_dir_all(anchor).map_err(|e| {
                    if e.kind() == std::io::ErrorKind::AlreadyExists {
                        Errno::NOENT.into()
                    } else {
                        e
                    }
                })?;
                Ok(fs::openat(CWD, anchor, ANCHOR, Mode::empty())?)
            }
            opened => Ok(opened?),
        }
    }

    /// `dir`, the anchor as opened, when it is the directory `checked`: one the caller
    /// checked a moment before, by path, so a link swapped onto that path since then has
    /// led the open to another directory, which is refused before anything below it is
    /// opened.
    fn opened_as_checked(dir: OwnedFd, checked: DirIdentity) -> Result<OwnedFd, Failure> {
        let dir = File::from(dir);
        if DirIdentity::of_metadata(&dir.metadata()?) == checked {
            Ok(OwnedFd::from(dir))
        } else {
            Err(Failure::AnchorMoved)
        }
    }

    /// What a failed open of the directory `name` in `dir`, `depth` levels below the
    /// anchor, comes to: a symlink refusal when the open said `ELOOP` — which an
    /// `O_NOFOLLOW` open of one name says for a symlink alone, even if the link is gone by
    /// the time it could be looked at again — or when `name` is a symlink now (Linux can
    /// say `ENOTDIR` for one opened `O_DIRECTORY`, as for a file); else the open's own
    /// failure.
    pub(super) fn below_failure(
        dir: BorrowedFd<'_>,
        name: &OsStr,
        depth: usize,
        errno: Errno,
    ) -> Failure {
        if errno == Errno::LOOP || is_symlink(dir, name) {
            Failure::LinkBelowAnchor { depth }
        } else {
            Failure::from(errno)
        }
    }

    /// Open the directory `name` in `dir` without following a symlink, creating it first
    /// when it is missing and `parents` says so.
    fn open_below(dir: BorrowedFd<'_>, name: &OsStr, parents: Parents) -> Result<OwnedFd, Errno> {
        match fs::openat(dir, name, BELOW, Mode::empty()) {
            Err(Errno::NOENT) if parents == Parents::Create => {
                match fs::mkdirat(dir, name, NEW_DIR) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(e) => return Err(e),
                }
                fs::openat(dir, name, BELOW, Mode::empty())
            }
            opened => opened,
        }
    }

    /// Whether `name` in `dir` is a symlink, looked at without following it.
    fn is_symlink(dir: BorrowedFd<'_>, name: &OsStr) -> bool {
        fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)
            .is_ok_and(|stat| FileType::from_raw_mode(stat.st_mode) == FileType::Symlink)
    }

    /// Replace `name` in `dir` with `content`, by way of a temporary file beside it.
    fn replace(
        dir: OwnedFd,
        name: &OsStr,
        content: &[u8],
        durability: Durability,
    ) -> Result<(), Failure> {
        // The target is looked at, never opened: a FIFO would block the open.
        let existing = match fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => match FileType::from_raw_mode(stat.st_mode) {
                FileType::RegularFile => Some(Mode::from_raw_mode(stat.st_mode)),
                FileType::Symlink => return Err(Failure::LinkAtTarget),
                // A directory has no mode a file should take: the rename below refuses to
                // replace it, and the temporary file is unlinked again.
                FileType::Directory => None,
                _ => return Err(Failure::NotARegularFile),
            },
            Err(Errno::NOENT) => None,
            Err(e) => return Err(e.into()),
        };
        // A file that replaces another is created owner-only and then given that file's
        // mode, so its bytes are never readable by anyone the old file did not allow.
        let create = match existing {
            Some(_) => Mode::RUSR.union(Mode::WUSR),
            None => NEW_FILE,
        };
        let (mut temp, mut file) = create_temp(dir.as_fd(), create, temp_names())?;
        if let Some(mode) = existing {
            fs::fchmod(&file, mode)?;
        }
        file.write_all(content)?;
        if durability == Durability::Fsync {
            file.sync_all()?;
        }
        drop(file);
        fs::renameat(dir.as_fd(), &temp.name, dir.as_fd(), name)?;
        temp.renamed = true;
        drop(temp);
        if durability == Durability::Fsync {
            sync_directory(dir)?;
        }
        Ok(())
    }

    /// A temporary file a write created in `dir`, unlinked when dropped unless it was
    /// renamed over the target first.
    pub(super) struct Temp<'d> {
        dir: BorrowedFd<'d>,
        pub(super) name: OsString,
        renamed: bool,
    }

    impl Drop for Temp<'_> {
        fn drop(&mut self) {
            if !self.renamed {
                // Best effort: the write has already failed, and this error is not the one
                // it reports.
                let _ = fs::unlinkat(self.dir, &self.name, AtFlags::empty());
            }
        }
    }

    /// Create a new file in `dir` with `mode` under the first of `names` that is free,
    /// trying at most [`MAX_TEMP_ATTEMPTS`] of them.
    pub(super) fn create_temp<'d>(
        dir: BorrowedFd<'d>,
        mode: Mode,
        names: impl IntoIterator<Item = OsString>,
    ) -> Result<(Temp<'d>, File), Errno> {
        for name in names.into_iter().take(MAX_TEMP_ATTEMPTS) {
            match fs::openat(dir, &name, TEMP, mode) {
                Ok(fd) => {
                    let temp = Temp {
                        dir,
                        name,
                        renamed: false,
                    };
                    return Ok((temp, File::from(fd)));
                }
                Err(Errno::EXIST) => {}
                Err(e) => return Err(e),
            }
        }
        Err(Errno::EXIST)
    }

    /// Random temporary names, `.mds-tmp-<16 hex digits>.tmp`, each drawn from a fresh
    /// `RandomState` so no two writes, in this process or another, are likely to share one.
    fn temp_names() -> impl Iterator<Item = OsString> {
        use std::hash::{BuildHasher as _, Hasher as _};

        (0..MAX_TEMP_ATTEMPTS).map(|attempt| {
            let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
            hasher.write_usize(attempt);
            OsString::from(format!(
                "{TEMP_PREFIX}{:016x}{TEMP_SUFFIX}",
                hasher.finish()
            ))
        })
    }

    /// Make the rename itself durable by syncing the directory it happened in.
    ///
    /// `File::sync_all` is `F_FULLFSYNC` on Apple platforms, which a filesystem may refuse
    /// for a directory (`ENOTSUP`, `EOPNOTSUPP`, `EINVAL`); a plain `fsync` is the fallback
    /// for those. A filesystem that refuses that too cannot sync a directory: the rename
    /// has landed, so the write stands. Any other failure is the write's.
    fn sync_directory(dir: OwnedFd) -> std::io::Result<()> {
        let dir = File::from(dir);
        settle_directory_sync(
            || dir.sync_all(),
            || fs::fsync(&dir).map_err(std::io::Error::from),
        )
    }

    /// The directory sync's outcome, from `full`, the sync asked for first, and `plain`,
    /// the fallback made only when `full` was refused for a directory: a refusal of both
    /// is no error, as there is no directory sync to be had.
    pub(super) fn settle_directory_sync(
        full: impl FnOnce() -> std::io::Result<()>,
        plain: impl FnOnce() -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        match full() {
            Err(e) if refused(&e) => match plain() {
                Err(e) if refused(&e) => Ok(()),
                synced => synced,
            },
            synced => synced,
        }
    }

    /// Whether `e` is a refusal to sync a directory the way it was asked.
    fn refused(e: &std::io::Error) -> bool {
        Errno::from_io_error(e).is_some_and(refused_for_a_directory)
    }

    /// Whether `errno` is a refusal to sync a directory the way it was asked, rather than a
    /// failure to sync it.
    fn refused_for_a_directory(errno: Errno) -> bool {
        errno == Errno::NOTSUP || errno == Errno::OPNOTSUPP || errno == Errno::INVAL
    }
}

#[cfg(windows)]
mod windows {
    use std::io::Write as _;
    use std::path::Path;

    use super::{Below, DirIdentity, Durability, Failure, Parents, TEMP_PREFIX, TEMP_SUFFIX};

    /// `ERROR_PATH_NOT_FOUND`: a directory that is not there, in the operating system's
    /// words.
    const PATH_NOT_FOUND: i32 = 3;

    /// Write `content` to `below.name`: refuse an anchor that is not the directory
    /// `anchor` names, when it names one, and a symlink or a junction at any directory
    /// below the anchor, then replace the file by path (the residual the module docs
    /// describe: each is looked at by path, before the write).
    pub(super) fn write(
        below: &Below<'_>,
        anchor: Option<DirIdentity>,
        content: &[u8],
        durability: Durability,
        parents: Parents,
    ) -> Result<(), Failure> {
        if parents == Parents::Create {
            // A name on the anchor's path taken by a link that leads nowhere — the anchor
            // then resolves to nothing — is a directory that is not there, as on unix; a
            // file there keeps the error creating the directory met.
            std::fs::create_dir_all(&below.anchor).map_err(|e| {
                let leads_nowhere = || {
                    std::fs::metadata(&below.anchor)
                        .is_err_and(|m| m.kind() == std::io::ErrorKind::NotFound)
                };
                if e.kind() == std::io::ErrorKind::AlreadyExists && leads_nowhere() {
                    std::io::Error::from_raw_os_error(PATH_NOT_FOUND)
                } else {
                    e
                }
            })?;
        }
        if anchor.is_some_and(|checked| DirIdentity::of(&below.anchor) != Some(checked)) {
            return Err(Failure::AnchorMoved);
        }
        let mut dir = below.anchor.clone();
        for (depth, name) in below.dirs.iter().enumerate() {
            dir.push(name);
            if !directory_below(&dir, parents)? {
                return Err(Failure::LinkBelowAnchor { depth });
            }
        }
        let target = dir.join(below.name);
        match std::fs::symlink_metadata(&target) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(Failure::LinkAtTarget),
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        let mut temp = tempfile::Builder::new()
            .prefix(TEMP_PREFIX)
            .suffix(TEMP_SUFFIX)
            .tempfile_in(&dir)?;
        temp.as_file_mut().write_all(content)?;
        if durability == Durability::Fsync {
            temp.as_file().sync_all()?;
        }
        temp.persist(&target).map_err(|e| e.error)?;
        Ok(())
    }

    /// Whether `dir`, a directory below the anchor, is one that is not a symlink or a
    /// junction — creating it first when it is missing and `parents` says so. `false`
    /// when it is a link; an error when it is something else or cannot be looked at.
    fn directory_below(dir: &Path, parents: Parents) -> std::io::Result<bool> {
        let meta = match std::fs::symlink_metadata(dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && parents == Parents::Create => {
                match std::fs::create_dir(dir) {
                    Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e),
                    _ => std::fs::symlink_metadata(dir)?,
                }
            }
            looked => looked?,
        };
        if meta.file_type().is_symlink() {
            return Ok(false);
        }
        if !meta.is_dir() {
            return Err(std::io::ErrorKind::NotADirectory.into());
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`atomic_write_file`] of a file named as it is written, anchored at its directory.
    fn write_as_typed(
        path: &Path,
        content: &str,
        durability: Durability,
    ) -> std::result::Result<(), mds::MdsError> {
        atomic_write_file(
            &WriteTarget::as_typed(path.to_path_buf()),
            content,
            durability,
            Parents::Existing,
        )
    }

    /// Names of leftover `.mds-tmp-*` entries directly inside `dir`.
    fn temp_residue(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(TEMP_PREFIX))
            .collect()
    }

    /// The names in `dir`, sorted.
    #[cfg(unix)]
    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Creates a symlink for a test, tolerating Windows' unprivileged restriction.
    ///
    /// Mirrors `crates/mds-core/src/lib.rs`'s crate-internal helper of the same name and
    /// contract (#147): unix needs no privilege; Windows needs Developer Mode or an
    /// elevated process (GitHub's `windows-latest` runners have Developer Mode enabled, so
    /// a failure there is a genuine regression and must panic), and only the unprivileged
    /// local case — `CI` unset plus raw OS error 1314 (`ERROR_PRIVILEGE_NOT_HELD`) — is a
    /// skip. Duplicated rather than shared because this crate has no unit-test-scope helper
    /// module.
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
                        crate::output::ewriteln!(
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

    /// The OS's own text for `errno`, as [`io_cause`] shows it.
    #[cfg(unix)]
    fn os_text(errno: rustix::io::Errno) -> String {
        std::io::Error::from(errno).to_string()
    }

    // ── Splitting a target at its anchor ─────────────────────────────────────────

    /// A target splits into its anchor, the directories below it and its name; a file
    /// typed bare is anchored at `.`. A target whose name is not one — `.`, `..`, a root —
    /// or with `..` below its anchor is refused before anything is opened (#160).
    #[test]
    fn a_target_splits_at_its_anchor_and_below_it_is_names_only() {
        let target = WriteTarget::below(Path::new("out"), Path::new("o"), Path::new("a/b/x.md"));
        assert_eq!(
            Below::of(&target).unwrap(),
            Below {
                anchor: PathBuf::from("out"),
                dirs: vec![OsStr::new("a"), OsStr::new("b")],
                name: OsStr::new("x.md"),
            }
        );
        let bare = WriteTarget::as_typed(PathBuf::from("x.md"));
        assert_eq!(
            Below::of(&bare).unwrap(),
            Below {
                anchor: PathBuf::from("."),
                dirs: vec![],
                name: OsStr::new("x.md"),
            }
        );

        for typed in [".", "out/..", "/"] {
            let target = WriteTarget::as_typed(PathBuf::from(typed));
            let kind = Below::of(&target).map(|_| ()).unwrap_err().kind();
            assert_eq!(
                kind,
                std::io::ErrorKind::IsADirectory,
                "{typed:?} names a directory"
            );
        }
        let climbing =
            WriteTarget::below(Path::new("out"), Path::new("out"), Path::new("a/../x.md"));
        assert_eq!(
            Below::of(&climbing).map(|_| ()).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput,
            "a `..` below the anchor is refused"
        );
    }

    /// A target that ends in a separator, or in a separator and a `.`, names a directory,
    /// though `Path::components` drops that ending and leaves the name before it: `out/` and
    /// `out/.` are refused as a directory, never written as the file `out`. Controls: the
    /// same names without the ending, and a name that merely ends in a dot, split.
    #[test]
    fn a_target_ending_in_a_separator_names_a_directory() {
        let sep = std::path::MAIN_SEPARATOR_STR;
        for typed in [
            "out/".to_owned(),
            "out/.".to_owned(),
            "out/./".to_owned(),
            "sub/out/".to_owned(),
            format!("out{sep}"),
            format!("out{sep}."),
        ] {
            let target = WriteTarget::as_typed(PathBuf::from(&typed));
            let kind = Below::of(&target).map(|_| ()).unwrap_err().kind();
            assert_eq!(
                kind,
                std::io::ErrorKind::IsADirectory,
                "{typed:?} names a directory"
            );
        }
        for (typed, name) in [("out", "out"), ("sub/out", "out"), ("out.", "out.")] {
            let target = WriteTarget::as_typed(PathBuf::from(typed));
            assert_eq!(
                Below::of(&target).unwrap().name,
                OsStr::new(name),
                "{typed:?}"
            );
        }
    }

    /// A refused directory is named below the anchor's shown form, cut after the level the
    /// symlink was found at.
    #[test]
    fn a_refused_directory_is_named_as_shown_down_to_the_link() {
        let target = WriteTarget::below(
            Path::new("/canonical/out"),
            Path::new("out"),
            Path::new("a/b/x.md"),
        );
        assert_eq!(shown_directory(&target, 0), Path::new("out").join("a"));
        assert_eq!(
            shown_directory(&target, 1),
            Path::new("out").join("a").join("b")
        );
    }

    // ── A symlink below the anchor (#160) ────────────────────────────────────────

    /// A symlink planted at a directory below the anchor is refused, named as shown, and
    /// nothing reaches the directory it points at; the link is left as it was. Controls:
    /// the same target writes once the link is a directory again, and a file written
    /// through the link by hand does reach the directory it points at.
    #[cfg(unix)]
    #[test]
    fn a_symlink_planted_below_the_anchor_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let anchor = dir.path().join("anchor");
        let victim = dir.path().join("victim");
        std::fs::create_dir_all(anchor.join("a")).unwrap();
        std::fs::create_dir(&victim).unwrap();
        std::os::unix::fs::symlink(&victim, anchor.join("a").join("b")).unwrap();
        let target = WriteTarget::below(&anchor, Path::new("out"), Path::new("a/b/x.md"));

        for parents in [Parents::Create, Parents::Existing] {
            let err = atomic_write_file(&target, "X", Durability::Fsync, parents)
                .expect_err("a symlink below the anchor is refused")
                .to_string();
            assert_eq!(err, "cannot write out/a/b: refusing to follow a symlink");
        }
        assert_eq!(entries(&victim), Vec::<String>::new());
        assert!(std::fs::symlink_metadata(anchor.join("a/b"))
            .unwrap()
            .file_type()
            .is_symlink());

        std::fs::remove_file(anchor.join("a/b")).unwrap();
        atomic_write_file(&target, "X", Durability::Fsync, Parents::Create).unwrap();
        assert_eq!(
            std::fs::read_to_string(anchor.join("a/b/x.md")).unwrap(),
            "X"
        );
        std::fs::remove_dir_all(anchor.join("a/b")).unwrap();
        std::os::unix::fs::symlink(&victim, anchor.join("a").join("b")).unwrap();
        std::fs::write(anchor.join("a/b/by-hand"), "x").unwrap();
        assert_eq!(entries(&victim), vec!["by-hand".to_owned()]);
    }

    /// A failed open of a directory below the anchor is a symlink refusal when the open
    /// said `ELOOP` — a link swapped back for a directory before it could be looked at
    /// again included — or when the name is a symlink, whatever the open said; any other
    /// failure of a directory is the write's own.
    #[cfg(unix)]
    #[test]
    fn a_failed_open_below_the_anchor_is_refused_when_it_met_a_symlink() {
        use std::os::fd::AsFd as _;

        use rustix::io::Errno;

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("real")).unwrap();
        std::os::unix::fs::symlink("real", dir.path().join("link")).unwrap();
        let fd = rustix::fs::open(
            dir.path(),
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY,
            rustix::fs::Mode::empty(),
        )
        .unwrap();
        let failure =
            |name: &str, errno| super::unix::below_failure(fd.as_fd(), OsStr::new(name), 3, errno);

        assert!(
            matches!(
                failure("real", Errno::LOOP),
                Failure::LinkBelowAnchor { depth: 3 }
            ),
            "ELOOP is a symlink, though a directory is there now"
        );
        assert!(
            matches!(
                failure("link", Errno::NOTDIR),
                Failure::LinkBelowAnchor { depth: 3 }
            ),
            "a symlink there now is refused, whatever the open said"
        );
        for errno in [Errno::NOTDIR, Errno::ACCESS] {
            assert!(
                matches!(failure("real", errno), Failure::Io(_)),
                "{errno:?} of a directory is the write's own failure"
            );
        }
    }

    /// The Fsync tier's directory sync, after the rename has landed: a sync refused for a
    /// directory (`ENOTSUP`, `EOPNOTSUPP`, `EINVAL`) falls back to a plain one, and a
    /// filesystem that refuses that too cannot sync a directory at all — the write stands,
    /// with no error. Any other failure, of either sync, is the write's; the fallback is
    /// made only after a refusal.
    #[cfg(unix)]
    #[test]
    fn a_directory_sync_the_filesystem_refuses_twice_does_not_fail_the_write() {
        use rustix::io::Errno;

        let fail = |errno: Errno| -> std::io::Result<()> { Err(errno.into()) };
        let settle = |full: std::io::Result<()>, plain: Option<std::io::Result<()>>| {
            let mut fell_back = false;
            let settled = super::unix::settle_directory_sync(
                || full,
                || {
                    fell_back = true;
                    plain.expect("no fallback after this sync")
                },
            );
            (settled.map_err(|e| Errno::from_io_error(&e)), fell_back)
        };

        assert_eq!(settle(Ok(()), None), (Ok(()), false), "synced");
        for refusal in [Errno::NOTSUP, Errno::OPNOTSUPP, Errno::INVAL] {
            assert_eq!(
                settle(fail(refusal), Some(Ok(()))),
                (Ok(()), true),
                "{refusal:?}, then a plain sync"
            );
            assert_eq!(
                settle(fail(refusal), Some(fail(Errno::INVAL))),
                (Ok(()), true),
                "{refusal:?}, then a plain sync refused too: no directory sync to be had"
            );
            assert_eq!(
                settle(fail(refusal), Some(fail(Errno::IO))),
                (Err(Some(Errno::IO)), true),
                "{refusal:?}, then a plain sync that fails: the write's error"
            );
        }
        assert_eq!(
            settle(fail(Errno::IO), None),
            (Err(Some(Errno::IO)), false),
            "a failed sync is the write's error, with no fallback"
        );
    }

    /// A symlinked anchor is followed: the anchor is resolved by path, as the user typed it
    /// (#160). Control for the refusal above: the same link, as the anchor itself, writes.
    #[test]
    fn a_symlinked_anchor_is_followed() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        if !make_symlink(&real, &link) {
            return;
        }
        let target = WriteTarget::below(&link, Path::new("link"), Path::new("sub/x.md"));
        atomic_write_file(&target, "X", Durability::RenameOnly, Parents::Create).unwrap();
        assert_eq!(
            std::fs::read_to_string(real.join("sub").join("x.md")).unwrap(),
            "X"
        );
    }

    /// A write `mds watch` makes below its out-dir names the directory its check found
    /// there, and an anchor the write opens that is another directory — the out-dir
    /// swapped between the check and the write, here a sibling's identity named instead
    /// — is refused with the out-dir refusal, the file named as shown, and nothing is
    /// written or created below it (#160). Control: the identity of the directory opened
    /// writes.
    ///
    /// `#[cfg(unix)]`: two directories made a moment apart are told apart by their inode
    /// there; Windows has their creation times alone, which can be equal.
    #[cfg(unix)]
    #[test]
    fn a_write_whose_anchor_is_not_the_directory_checked_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let other = dir.path().join("other");
        for made in [&out, &other] {
            std::fs::create_dir(made).unwrap();
        }
        let identity = |of: &Path| DirIdentity::of(of).expect("a directory");
        let target = WriteTarget::below(&out, Path::new("o"), Path::new("sub/x.md"));

        let swapped = target.below_checked_anchor(&out, 0, identity(&other));
        let err = atomic_write_file(&swapped, "X", Durability::RenameOnly, Parents::Create)
            .expect_err("the anchor opened is not the directory checked")
            .to_string();
        assert_eq!(
            err,
            format!("cannot write {}: {OUT_DIR_MOVED}", safe_path(&target.shown))
        );
        assert_eq!(
            entries(&out),
            Vec::<String>::new(),
            "nothing is written or created below it"
        );

        let checked = target.below_checked_anchor(&out, 0, identity(&out));
        atomic_write_file(&checked, "X", Durability::RenameOnly, Parents::Create).unwrap();
        assert_eq!(
            std::fs::read_to_string(out.join("sub").join("x.md")).unwrap(),
            "X"
        );
    }

    /// An out-dir the check found missing is written below the directory above it, the
    /// one checked, and created there without following a symlink (#160): a write that
    /// opens another directory as that anchor is refused and creates nothing, and a
    /// symlink put where the out-dir is to be made is refused, its target left empty.
    /// Control: the directory checked, opened, gets the out-dir and the file.
    #[cfg(unix)]
    #[test]
    fn a_missing_out_dir_is_made_below_the_directory_checked_above_it() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let other = dir.path().join("other");
        let victim = dir.path().join("victim");
        for made in [&base, &other, &victim] {
            std::fs::create_dir(made).unwrap();
        }
        let identity = |of: &Path| DirIdentity::of(of).expect("a directory");
        let out = base.join("out");
        let target = WriteTarget::below(&out, Path::new("o"), Path::new("x.md"));
        let write = |anchor: &Path| {
            let checked = target.below_checked_anchor(&out, 1, identity(anchor));
            atomic_write_file(&checked, "X", Durability::RenameOnly, Parents::Create)
        };

        let err = write(&other)
            .expect_err("not the directory checked")
            .to_string();
        assert_eq!(
            err,
            format!("cannot write {}: {OUT_DIR_MOVED}", safe_path(&target.shown))
        );
        assert!(!out.exists(), "nothing is created");

        std::os::unix::fs::symlink(&victim, &out).unwrap();
        let err = write(&base)
            .expect_err("a link where the out-dir goes")
            .to_string();
        assert_eq!(err, format!("cannot write o: {FOLLOW_REFUSAL}"));
        assert_eq!(
            entries(&victim),
            Vec::<String>::new(),
            "nothing lands there"
        );
        std::fs::remove_file(&out).unwrap();

        write(&base).unwrap();
        assert_eq!(std::fs::read_to_string(out.join("x.md")).unwrap(), "X");
    }

    /// A bounded swap loop (#160): while a thread swaps a directory below the anchor
    /// between a directory and a symlink, at most 2,000 writes in at most 10 s land
    /// nothing in the link's target. Controls: the loop met both states — some writes
    /// landed, some were refused — and a file written through the link by hand does reach
    /// its target.
    #[cfg(unix)]
    #[test]
    fn a_swap_loop_below_the_anchor_lands_nothing_in_the_link_s_target() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        /// How long each state is held: about as long as a write takes.
        const HOLD: Duration = Duration::from_micros(200);

        let dir = tempfile::tempdir().unwrap();
        let anchor = dir.path().join("anchor");
        let victim = dir.path().join("victim");
        std::fs::create_dir_all(anchor.join("sub")).unwrap();
        std::fs::create_dir(&victim).unwrap();
        let target = WriteTarget::below(&anchor, Path::new("out"), Path::new("sub/x.md"));

        let stop = Arc::new(AtomicBool::new(false));
        let swapper = {
            let stop = Arc::clone(&stop);
            let (anchor, victim) = (anchor.clone(), victim.clone());
            std::thread::spawn(move || {
                let sub = anchor.join("sub");
                // Bounded: at most 100,000 swaps, and the loop ends with the writes.
                for i in 0..100_000u32 {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let _ = std::fs::rename(&sub, anchor.join(format!("away-{i}")));
                    let _ = std::os::unix::fs::symlink(&victim, &sub);
                    std::thread::sleep(HOLD);
                    if std::fs::remove_file(&sub).is_err() {
                        let _ = std::fs::rename(&sub, anchor.join(format!("away-{i}-b")));
                    }
                    let _ = std::fs::create_dir(&sub);
                    std::thread::sleep(HOLD);
                }
            })
        };

        let started = Instant::now();
        let (mut written, mut refused) = (0u32, 0u32);
        // Bounded: 2,000 writes or 10 s, whichever comes first.
        for _ in 0..2_000 {
            if started.elapsed() > Duration::from_secs(10) {
                break;
            }
            match atomic_write_file(&target, "X", Durability::RenameOnly, Parents::Create) {
                Ok(()) => written += 1,
                Err(e) if e.to_string().contains(FOLLOW_REFUSAL) => refused += 1,
                Err(_) => {}
            }
        }
        stop.store(true, Ordering::Relaxed);
        swapper.join().unwrap();

        assert_eq!(
            entries(&victim),
            Vec::<String>::new(),
            "nothing landed in the link's target ({written} written, {refused} refused)"
        );
        assert!(
            written > 0 && refused > 0,
            "the loop met both states: {written} written, {refused} refused"
        );
        let _ = std::fs::remove_file(anchor.join("sub"));
        let _ = std::fs::remove_dir_all(anchor.join("sub"));
        std::os::unix::fs::symlink(&victim, anchor.join("sub")).unwrap();
        std::fs::write(anchor.join("sub/by-hand"), "x").unwrap();
        assert_eq!(entries(&victim), vec!["by-hand".to_owned()]);
    }

    // ── Directories a write creates ─────────────────────────────────────────────

    /// `Parents::Create` creates a missing anchor by path and the directories below it;
    /// `Parents::Existing` creates nothing and fails. Control: the first writes.
    #[test]
    fn only_an_output_creates_its_directories() {
        let dir = tempfile::tempdir().unwrap();
        let anchor = dir.path().join("new").join("anchor");
        let target = WriteTarget::below(&anchor, Path::new("out"), Path::new("a/b/x.md"));

        let err = atomic_write_file(&target, "X", Durability::RenameOnly, Parents::Existing)
            .expect_err("nothing is created for a rewrite")
            .to_string();
        let named = format!("cannot write {}: ", safe_path(&target.shown));
        assert!(err.starts_with(&named), "{err}");
        assert!(!dir.path().join("new").exists(), "nothing was created");

        atomic_write_file(&target, "X", Durability::RenameOnly, Parents::Create).unwrap();
        assert_eq!(
            std::fs::read_to_string(anchor.join("a").join("b").join("x.md")).unwrap(),
            "X"
        );
    }

    /// An anchor that is a symlink leading nowhere is a directory that is not there: the
    /// write says so, rather than the `File exists` creating it fails with, and creates
    /// nothing where the link points. Control: once the link leads to a directory, the
    /// write lands there.
    #[test]
    fn a_dangling_symlink_anchor_is_reported_as_missing() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link");
        let nowhere = dir.path().join("nowhere");
        // A directory link (Windows tells the kinds apart), then left leading nowhere.
        std::fs::create_dir(&nowhere).unwrap();
        if !make_symlink(&nowhere, &link) {
            return;
        }
        std::fs::remove_dir(&nowhere).unwrap();
        let target = WriteTarget::below(&link, Path::new("o"), Path::new("x.md"));

        let err = atomic_write_file(&target, "X", Durability::RenameOnly, Parents::Create)
            .expect_err("a dangling anchor cannot be written below")
            .to_string();
        #[cfg(unix)]
        let missing = os_text(rustix::io::Errno::NOENT);
        // `ERROR_PATH_NOT_FOUND`.
        #[cfg(windows)]
        let missing = std::io::Error::from_raw_os_error(3).to_string();
        assert_eq!(
            err,
            format!("cannot write {}: {missing}", safe_path(&target.shown))
        );
        assert!(
            !nowhere.exists(),
            "nothing was created where the link points"
        );

        std::fs::create_dir(&nowhere).unwrap();
        atomic_write_file(&target, "X", Durability::RenameOnly, Parents::Create).unwrap();
        assert_eq!(std::fs::read_to_string(nowhere.join("x.md")).unwrap(), "X");
    }

    // ── The temporary file ───────────────────────────────────────────────────────

    /// A temporary name is tried at most [`MAX_TEMP_ATTEMPTS`] times: a write whose every
    /// name is taken fails after that many, and the file under the taken name is left as
    /// it was. Control: a free name is taken at once.
    #[cfg(unix)]
    #[test]
    fn temporary_names_are_tried_a_bounded_number_of_times() {
        use std::os::fd::AsFd as _;

        use super::unix::{create_temp, MAX_TEMP_ATTEMPTS};

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".taken"), "T").unwrap();
        let fd = rustix::fs::open(
            dir.path(),
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY,
            rustix::fs::Mode::empty(),
        )
        .unwrap();
        let tried = std::cell::Cell::new(0usize);
        let taken = std::iter::repeat_with(|| {
            tried.set(tried.get() + 1);
            std::ffi::OsString::from(".taken")
        });
        let result = create_temp(fd.as_fd(), rustix::fs::Mode::RUSR, taken);
        assert_eq!(result.map(|_| ()).unwrap_err(), rustix::io::Errno::EXIST);
        assert_eq!(tried.get(), MAX_TEMP_ATTEMPTS);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".taken")).unwrap(),
            "T"
        );

        let free = [std::ffi::OsString::from(".free")];
        let (temp, _file) = create_temp(fd.as_fd(), rustix::fs::Mode::RUSR, free).unwrap();
        assert_eq!(temp.name, ".free");
        drop(temp);
        assert!(
            !dir.path().join(".free").exists(),
            "dropped, the file is unlinked"
        );
    }

    // ── Moved from output.rs: the replace-by-rename contract ─────────────────────

    /// An error writing a file names it by `shown`, never by `path`, and its cause is the
    /// operating system's, naming no path (#390). Control: the whole message is the one
    /// expected, so the target is named.
    ///
    /// `#[cfg(unix)]`: a read-only directory is what makes the temporary file fail, and
    /// Windows' read-only attribute does not stop a file being created in one (#147).
    #[cfg(unix)]
    #[test]
    fn an_error_writing_names_the_file_as_shown_and_no_path_of_its_own() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let target = WriteTarget::new(sub.join("locked.mds"), Path::new("out").join("locked.mds"));
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();
        let probe = std::fs::write(sub.join("probe"), "");
        let result = atomic_write_file(&target, "NEW", Durability::Fsync, Parents::Existing);
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
        if probe.is_ok() {
            crate::output::ewriteln!(
                "running as root; a read-only directory does not stop the write"
            );
            return;
        }

        let err = result
            .expect_err("a read-only directory must fail the write")
            .to_string();
        assert_eq!(
            err,
            format!(
                "cannot write out/locked.mds: {}",
                os_text(rustix::io::Errno::ACCESS)
            )
        );
        let tmp = dir.path().display().to_string();
        assert!(!err.contains(&tmp), "no path of the write's own: {err}");
        assert!(!err.contains(TEMP_PREFIX), "no temporary file: {err}");
    }

    /// T-U1: `mds build` writes artifacts that do not exist yet (#227). The primitive must
    /// create the target instead of failing the existence probe.
    #[test]
    fn atomic_write_file_creates_missing_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("fresh.md");
        assert!(!target.exists(), "precondition: target must be absent");

        write_as_typed(&target, "CREATED", Durability::Fsync)
            .expect("writing an absent target must succeed");

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "CREATED");
        let residue = temp_residue(dir.path());
        assert!(
            residue.is_empty(),
            "no .mds-tmp- residue may survive a successful write; got {residue:?}"
        );
    }

    /// T-U9: `Durability::RenameOnly` changes ONLY whether the write is synced. Everything
    /// the callers rely on — the content, the mode of a freshly created artifact, the
    /// symlink refusal, and leaving no temp residue — must be identical to `Fsync` (#227).
    /// The syncs themselves are not observable from a passing process; what this pins is
    /// that skipping them did not quietly relax anything else, and that the file and
    /// directory syncs of `Fsync` succeed on this filesystem.
    #[test]
    fn atomic_write_file_rename_only_matches_fsync_contract() {
        let dir = tempfile::tempdir().unwrap();

        // Fresh target: created, with the same content and mode as the Fsync sibling.
        let quick = dir.path().join("quick.md");
        let synced = dir.path().join("synced.md");
        write_as_typed(&quick, "DERIVED", Durability::RenameOnly).unwrap();
        write_as_typed(&synced, "DERIVED", Durability::Fsync).unwrap();
        assert_eq!(std::fs::read_to_string(&quick).unwrap(), "DERIVED");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&quick).unwrap().permissions().mode() & 0o777,
                std::fs::metadata(&synced).unwrap().permissions().mode() & 0o777,
                "RenameOnly must not change the mode a fresh artifact is created with"
            );
        }

        // Existing target: replaced, previous content gone.
        write_as_typed(&quick, "REBUILT", Durability::RenameOnly).unwrap();
        assert_eq!(std::fs::read_to_string(&quick).unwrap(), "REBUILT");
        write_as_typed(&synced, "RESYNCED", Durability::Fsync).unwrap();
        assert_eq!(std::fs::read_to_string(&synced).unwrap(), "RESYNCED");

        // Symlink target: still refused (the sync is not what enforces this).
        {
            let real = dir.path().join("real.md");
            std::fs::write(&real, "REAL").unwrap();
            let link = dir.path().join("link.md");
            if !make_symlink(&real, &link) {
                return;
            }
            let err = write_as_typed(&link, "NEW", Durability::RenameOnly)
                .expect_err("RenameOnly must still refuse a symlink target");
            assert!(
                err.to_string().contains("symlink"),
                "the refusal must say why; got: {err}"
            );
            assert_eq!(std::fs::read_to_string(&real).unwrap(), "REAL");
        }

        let residue = temp_residue(dir.path());
        assert!(
            residue.is_empty(),
            "RenameOnly must leave no .mds-tmp- residue; got {residue:?}"
        );
    }

    /// T-U2: a freshly created artifact must carry the same mode `std::fs::write` would
    /// have produced (`0666 & !umask`), not an owner-only 0600. The sibling control makes
    /// the assertion umask-independent.
    ///
    /// `#[cfg(unix)]`: unix permission mode bits (`PermissionsExt::mode`) have no Windows
    /// equivalent — the permission model differs (#147).
    #[cfg(unix)]
    #[test]
    fn atomic_write_file_new_file_mode_matches_std_fs_write() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.md");
        let ctl = dir.path().join("ctl.md");

        write_as_typed(&out, "X", Durability::Fsync).unwrap();
        std::fs::write(&ctl, "X").unwrap();

        let mode_out = std::fs::metadata(&out).unwrap().permissions().mode() & 0o777;
        let mode_ctl = std::fs::metadata(&ctl).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode_out, mode_ctl,
            "new-file mode must match std::fs::write; got 0{mode_out:o} vs control 0{mode_ctl:o}"
        );
    }

    /// T-U3: an existing file keeps its mode across the replace-by-rename cycle — owner-only
    /// `0600`, which the umask could never produce for a new file, and `0640`.
    ///
    /// `#[cfg(unix)]`: unix permission mode bits have no Windows equivalent (#147).
    #[cfg(unix)]
    #[test]
    fn atomic_write_file_existing_mode_preserved() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        for kept in [0o600, 0o640] {
            let target = dir.path().join(format!("src-{kept:o}.mds"));
            std::fs::write(&target, "OLD").unwrap();
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(kept)).unwrap();

            write_as_typed(&target, "NEW", Durability::Fsync).unwrap();

            let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o7777;
            assert_eq!(mode, kept, "existing mode must be preserved; got 0{mode:o}");
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "NEW");
        }
    }

    /// T-U4: a symlink at the target is refused, never written through. The control writes
    /// the symlink's own target directly and must succeed, so the refusal is not passing on
    /// an unrelated failure.
    #[test]
    fn atomic_write_file_refuses_live_symlink_target() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.md");
        let link = dir.path().join("link.md");
        std::fs::write(&real, "REAL").unwrap();
        if !make_symlink(&real, &link) {
            return;
        }

        let err = write_as_typed(&link, "NEW", Durability::Fsync)
            .expect_err("writing through a symlink must be refused");
        assert!(
            matches!(err, mds::MdsError::Io { .. }),
            "a refused write is mds::io, exit 2 (#157); got {err:?}"
        );
        let err = err.to_string();
        assert!(
            err.ends_with(&format!(": {SYMLINK_REFUSAL}")),
            "expected a symlink refusal; got {err}"
        );
        assert_eq!(
            std::fs::read_to_string(&real).unwrap(),
            "REAL",
            "the symlink's target must not be written through"
        );
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the symlink itself must survive the refusal"
        );

        // CONTROL: the same directory and content, addressed at the real file.
        write_as_typed(&real, "NEW", Durability::Fsync)
            .expect("writing the real file must succeed");
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "NEW");
    }

    /// T-U5: a dangling symlink is still a symlink — refuse it rather than materialising
    /// the missing file it points at.
    #[test]
    fn atomic_write_file_refuses_dangling_symlink_target() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.md");
        let link = dir.path().join("link.md");
        if !make_symlink(&missing, &link) {
            return;
        }

        let err = write_as_typed(&link, "NEW", Durability::Fsync)
            .expect_err("writing through a dangling symlink must be refused")
            .to_string();
        assert!(
            err.ends_with(&format!(": {SYMLINK_REFUSAL}")),
            "expected a symlink refusal; got {err}"
        );
        assert!(
            !missing.exists(),
            "the dangling link's target must not be created"
        );
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the symlink itself must survive the refusal"
        );
    }

    /// T-U6: a failed write leaves the original inode, bytes and mtime untouched and no
    /// temp file. The control proves the same call succeeds once the directory is writable
    /// again, and that success DOES replace the inode.
    ///
    /// `#[cfg(unix)]`: provokes the failure via chmod and asserts on
    /// `MetadataExt::ino()`, neither of which exists on Windows (#147).
    #[cfg(unix)]
    #[test]
    fn atomic_write_file_failure_preserves_original_and_leaves_no_temp() {
        use std::os::unix::fs::MetadataExt as _;
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let target = sub.join("locked.mds");
        std::fs::write(&target, "OLD").unwrap();

        let before = std::fs::metadata(&target).unwrap();
        let (ino, mtime) = (before.ino(), before.modified().unwrap());

        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = write_as_typed(&target, "NEW", Durability::Fsync);
        // Restore before asserting so a failed assertion cannot leave an undeletable
        // tempdir behind.
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let err = result
            .expect_err("a read-only parent directory must fail the write")
            .to_string();
        assert!(
            err.contains("locked.mds"),
            "error must name the target; got {err}"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "OLD");
        let after = std::fs::metadata(&target).unwrap();
        assert_eq!(
            after.ino(),
            ino,
            "a failed write must not replace the inode"
        );
        assert_eq!(
            after.modified().unwrap(),
            mtime,
            "a failed write must not touch the mtime"
        );
        let residue = temp_residue(&sub);
        assert!(
            residue.is_empty(),
            "failed write left temp residue: {residue:?}"
        );

        // CONTROL: writable again — the same call succeeds and swaps the inode.
        write_as_typed(&target, "NEW", Durability::Fsync)
            .expect("write must succeed once the dir is writable");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "NEW");
        assert_ne!(
            std::fs::metadata(&target).unwrap().ino(),
            ino,
            "replace-by-rename must produce a new inode"
        );
    }

    /// T-U7: a directory at the target fails the write at its last step — the rename, after
    /// the temporary file was created and written — and leaves no temp file behind: the
    /// injected failure the unlink guard is for. Named as shown (#390): the error names
    /// neither the path written nor the temporary file.
    #[test]
    fn atomic_write_file_directory_target_refused_without_residue() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("adir");
        std::fs::create_dir(&target).unwrap();

        let shown = Path::new("out").join("adir");
        let err = atomic_write_file(
            &WriteTarget::new(target.clone(), shown.clone()),
            "X",
            Durability::Fsync,
            Parents::Existing,
        )
        .expect_err("a directory target must not be written")
        .to_string();
        let named = format!("cannot write {}: ", safe_path(&shown));
        assert!(
            err.starts_with(&named),
            "error must name the target as shown; got {err}"
        );
        let tmp = dir.path().display().to_string();
        assert!(!err.contains(&tmp), "no path of the write's own: {err}");
        assert!(!err.contains(TEMP_PREFIX), "no temporary file: {err}");
        assert!(target.is_dir(), "the directory must survive the refusal");
        let residue = temp_residue(dir.path());
        assert!(
            residue.is_empty(),
            "refused write left temp residue: {residue:?}"
        );
    }

    /// T-U8: a directory the write cannot open is a hard error — never a write with a
    /// guessed mode (#225) — naming the target and the operating system's cause.
    ///
    /// `#[cfg(unix)]`: provokes the failure with a `0o000`-mode parent directory; Windows'
    /// permission model does not block traversal the same way (#147).
    #[cfg(unix)]
    #[test]
    fn atomic_write_file_unreadable_parent_is_hard_error() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nosearch");
        std::fs::create_dir(&p).unwrap();
        // Planted before the chmod so the root probe below has something to stat.
        let probe = p.join("probe");
        std::fs::write(&probe, "").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();

        if std::fs::metadata(&probe).is_ok() {
            // Root bypasses the mode bits; EACCES cannot be provoked here.
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            crate::output::ewriteln!("running as root; cannot exercise EACCES");
            return;
        }

        let target = WriteTarget::new(p.join("x.md"), PathBuf::from("x.md"));
        let result = atomic_write_file(&target, "X", Durability::Fsync, Parents::Existing);
        // Restore before asserting so tempdir cleanup always succeeds.
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();

        let err = result
            .expect_err("an unopenable directory must be a hard error")
            .to_string();
        assert_eq!(
            err,
            format!("cannot write x.md: {}", os_text(rustix::io::Errno::ACCESS))
        );
        assert!(!p.join("x.md").exists());
    }
}
