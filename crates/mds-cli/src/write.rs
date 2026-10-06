//! The one write primitive: every file `mds` writes goes through it —
//! [`write_compiled`] for `mds build` and `mds watch` outputs and `.map` sidecars (#425),
//! [`write_compiled_and_look`] for a directory build's outputs below an out-dir (#160),
//! [`atomic_write_file`] for `mds init --force`'s starter, [`create_new`] for `mds init`'s
//! starter without `--force`, [`replace_if_unchanged`] for `mds fmt` and `mds lint --fix`
//! rewrites, [`write_over_own`] for an `mds watch` output after its source's change of kind
//! (#227, #160) — and every file it removes, through [`remove_proven`].
//! `tests/write_funnel.rs` keeps it the only one.
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
//! opened ([`DirIdentity`]) — and one that is gone, without making it again. Nothing
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
//! unlinked again if anything after that fails — `fchmod` to the permission bits of the
//! file it replaces when one user owns both, the durability tier's syncs, and `renameat`
//! (`mod unix`).
//!
//! Windows has no descriptor-relative walk in std: each directory below the anchor is
//! checked with `symlink_metadata` and refused when it is a symlink or a junction — the
//! name-surrogate reparse points std's `is_symlink` reports; a cloud-sync placeholder is
//! not one and stays writable — and the write then goes by path (`mod windows`); the
//! anchor `mds watch` checked is compared by path too, just before the walk. A
//! directory swapped for a link between those checks and the write is followed: the
//! residual SECURITY.md and spec §7.2 document.
//!
//! # Rewrites over the bytes read (#160)
//!
//! `mds fmt` and `mds lint --fix` rewrite a file they have read. [`read_stamped`] reads it
//! again below its anchor and holds the directory it is in — on unix its descriptor — with
//! a stamp of the file; [`replace_if_unchanged`] renames the rewrite into that directory
//! only while the file there is still the one read, so an edit made after the read is not
//! overwritten and a directory swapped after it never receives the rewrite.
//!
//! # A new file, never over another (#160)
//!
//! [`create_new`] gives the temporary file the target's name only where nothing has that
//! name, at the moment it is given — not at an earlier look — so a file that appears in
//! between is left as it is and the write refused. On unix the commit is a rename that
//! never replaces (`renameat2(RENAME_NOREPLACE)` on Linux and Android,
//! `renameatx_np(RENAME_EXCL)` on Apple platforms); where the platform, the kernel or the
//! filesystem has none, a hard link of the temporary file to the target, which never
//! replaces either, and the temporary name removed; and on a filesystem without hard
//! links, the content written in place into a target created exclusively (`O_CREAT |
//! O_EXCL | O_NOFOLLOW`) — still never over another file, but no longer all or nothing: a
//! failure while it is written leaves the file partly written. Windows moves the
//! temporary file into place without `MOVEFILE_REPLACE_EXISTING`.
//!
//! # Never over an MDS module or a file the run reads (#425)
//!
//! [`write_compiled`] renames an output over whatever file is at its target, except an MDS
//! module — a `.mds` file, or a `.md` file whose frontmatter declares `type: mds`, as
//! mds-core judges one ([`mds::check_module_type`]), which a template may import — and a
//! file the run reads ([`Inputs`]): its entry, the modules its compile imported, the
//! `--vars` file and the `mds.json` in force. An output whose own frontmatter declares
//! `type: mds` — a module a template generates — is one too, and replaces a module as it
//! rewrites itself, whoever wrote the file there; it is judged the same way, by its text,
//! and still never replaces a file the run reads. Just before the rename the file at the
//! target is looked at in the directory the write is in, without following a symlink. A
//! regular `.mds` file there is a module by its name alone and is never read. A regular
//! `.md` file there — each name's extension taken in any case, which on a case-insensitive
//! volume names a `.md` or `.mds` file — is read as opened in that directory — on unix `openat(O_NOFOLLOW | O_NONBLOCK)` from the
//! walk's descriptor, never by path — past its first line only when that is a frontmatter
//! fence, and then no further than the end of the frontmatter, all mds-core's check looks
//! at, nor than [`mds::MAX_FILE_SIZE`] bytes; one that cannot be read is refused, since
//! nothing tells it is no module. Any regular file there is compared, on unix by its device and inode,
//! with each file the run reads, looked up by its path at that moment, so one an editor has
//! replaced since it was read is the one compared, and a hard link to one is that file. A
//! module or an input is refused, the write leaves no temporary file, and the file is left
//! as it is. A symlink put there by then is replaced, never written through, as for every
//! write; a module or an input put there in the instant between that look and the rename
//! is replaced too. Windows looks and reads by path, as its write goes, judges the target
//! by the name of the file it opens — less the dots and spaces it ends in, which Windows
//! drops, so `lib.md.` and `lib.md ` are `lib.md` — and compares the canonical paths of
//! the target and of each input, since std gives no file index there: a hard link to an
//! input is another path, and is written over by the rename; a target whose canonical
//! path cannot be had, but that is not gone, is refused, as a look that fails is on unix.
//!
//! # Over the caller's own file, or as a new one (#160)
//!
//! [`write_over_own`] replaces a file only while it holds exactly what its caller last
//! wrote there — read again as a rewrite reads its file, and replaced as a rewrite
//! replaces it, only while it is still that file — and otherwise writes as [`create_new`]
//! does: a file the caller did not write, or one changed since, is never replaced. A file
//! there that cannot be read again is neither, as nothing tells which: the write fails
//! with the read's cause, and the file is left as it is.
//!
//! # A removal proves its file first (#160)
//!
//! [`remove_proven`] removes a stale output, a stale `.map` sidecar or a deleted source's
//! output only once the file is shown to be the one to remove. The directory it is in is
//! walked to as a write's is — never created, a symlink below the anchor refused — and
//! the file is looked at without following a symlink: a symlink there is refused, never
//! removed, and anything else that is not a regular file is left unopened. On unix the
//! file is then opened in that directory (`openat(O_NOFOLLOW | O_NONBLOCK)`), `fstat` must
//! find a regular file, the caller's proof reads it, and `fstatat(AT_SYMLINK_NOFOLLOW)`
//! must find at the name the stamp `fstat` gave — the same device and inode, size, and
//! modification and status-change times, as a rewrite compares — before `unlinkat`
//! removes it from the directory the walk opened: a file put in its place after it was
//! opened is left, and so is the same file written over after the proof read it. Only one
//! put there, or an edit made, in the instant between that look and the removal is
//! removed instead, as is an edit that keeps the file's size and times on a filesystem
//! whose clock is coarser than the time it takes. Windows checks each directory below the
//! anchor as a write does, and removes by path: the file is closed after its proof,
//! looked at again by path — the same size, and modification and creation times — and
//! then removed by its name, so one put there in between is removed instead.
//!
//! A directory build learns whether anything has a stale output's name from the write of
//! the output beside it, which looks there just after its rename, without following a
//! symlink ([`write_compiled_and_look`]) — on unix with `fstatat(AT_SYMLINK_NOFOLLOW)` on
//! the descriptor its walk opened, before it closes it; on Windows by path, as its write
//! goes: a name nothing has needs no removal, and no walk is made for one; anything there,
//! and a look that fails, is dealt with as before — removed, if at all, only through
//! [`remove_proven`].
//!
//! # Contract (#226)
//!
//! This is replace-by-rename, not an in-place rewrite. The target receives a NEW inode, so
//! the write does NOT preserve hard links (other links keep the old content), ACLs,
//! extended attributes (xattrs), or owner/group of the original file; only the permission
//! bits are carried over (unix) — never a setuid, setgid or sticky bit, and only from a
//! file the user who writes owns: one another user owns lends none, and the new file has
//! the mode a new file is created with. Hard-link preservation is out of scope by
//! construction (it would require truncate-in-place and forfeit crash safety);
//! ACL/xattr/owner-group preservation is not planned — MDS only rewrites its own outputs
//! and `.mds` sources.

use std::borrow::Cow;
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
/// An existing file the user owns keeps its permission bits, never a setuid, setgid or
/// sticky bit (unix); a new one — and one replacing a file another user owns — is
/// created with mode `0666 & !umask`, as `std::fs::write` creates one. `parents` says
/// whether missing directories are created; `durability` whether the write is synced.
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
/// ([`WriteTarget::below_checked_anchor`]), or that is gone — it is not made again — is
/// refused as [`out_dir_moved`] words it, before anything below it is opened.
pub(crate) fn atomic_write_file(
    target: &WriteTarget,
    content: &str,
    durability: Durability,
    parents: Parents,
) -> std::result::Result<(), mds::MdsError> {
    write_below_anchor(target, content, durability, parents, Commit::Replace(None))
        .map(drop)
        .map_err(|failure| worded(target, failure))
}

/// Write a compiled output, or its `.map` sidecar, to `target` as [`atomic_write_file`]
/// writes one — [`Durability::RenameOnly`], since a rebuild reproduces it, creating the
/// directories it goes in — but never over one of `inputs`, the files the run reads, nor,
/// unless `content` is itself an MDS module, over one (#425; see the module docs): a `.mds`
/// file, or a `.md` file whose frontmatter declares `type: mds`, is a source a template
/// imports, and the
/// entry, an imported module, the `--vars` file or the `mds.json` in force is what the
/// output was made from — none is an output to replace. An output that declares
/// `type: mds` itself is a module a template generates, and replaces one as it rewrites
/// itself.
///
/// # Errors
///
/// As [`atomic_write_file`]; and, for an output that is no module, a module at the target
/// or a regular `.md` file there that cannot be read to tell, and a file the run reads,
/// are refused before the rename —
/// `cannot write <file>: refusing to replace an MDS module`, `cannot write <file>: cannot
/// tell whether it is an MDS module: <cause>` with the read's cause, or `cannot write
/// <file>: refusing to replace a file this run reads` — and left as they are, with no
/// temporary file left behind.
pub(crate) fn write_compiled(
    target: &WriteTarget,
    content: &str,
    inputs: &Inputs,
) -> std::result::Result<(), mds::MdsError> {
    write_compiled_in(target, content, inputs).map(drop)
}

/// [`write_compiled`], giving back the directory the output went in — on unix still open.
fn write_compiled_in(
    target: &WriteTarget,
    content: &str,
    inputs: &Inputs,
) -> std::result::Result<imp::Dir, mds::MdsError> {
    write_below_anchor(
        target,
        content,
        Durability::RenameOnly,
        Parents::Create,
        Commit::Output {
            inputs,
            module: declares_a_module(content),
        },
    )
    .map_err(|failure| worded(target, failure))
}

/// Write a compiled output as [`write_compiled`] does, then look at `beside`, a file in the
/// same directory — a directory build's stale output, the other kind's of the same name —
/// without following a symlink, and never reading it (#160): on unix in that directory as
/// the write's walk opened it, before it is closed; on Windows by path, as the write goes
/// (the residual the module docs describe). Where nothing has its name there is nothing to
/// prove or remove, and the second walk a removal makes is not needed; where something
/// has it, or the look cannot tell, the caller deals with it as before — removing it, if
/// at all, through [`remove_proven`], whose own walk and checks are made as for any
/// removal. A `beside` that is not in the directory the output went in is not looked at,
/// and is [`Beside::Something`].
///
/// # Errors
///
/// As [`write_compiled`]; nothing is looked at when the write fails.
pub(crate) fn write_compiled_and_look(
    target: &WriteTarget,
    content: &str,
    inputs: &Inputs,
    beside: &WriteTarget,
) -> std::result::Result<Beside, mds::MdsError> {
    let dir = write_compiled_in(target, content, inputs)?;
    Ok(match (Below::of(target), Below::of(beside)) {
        (Ok(written), Ok(beside))
            if written.anchor == beside.anchor && written.dirs == beside.dirs =>
        {
            imp::look(&dir, beside.name)
        }
        _ => Beside::Something,
    })
}

/// What [`write_compiled_and_look`] found at the name it looked at beside its output.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Beside {
    /// Nothing had the name.
    Nothing,
    /// Something had it — a file, a symlink, a directory, anything — or the look could not
    /// tell: the caller deals with it.
    Something,
}

/// The files a run reads — its entry, the modules its compile imported, the `--vars` file
/// and the `mds.json` in force — which [`write_compiled`] never writes an output over
/// (#425). Each is held by its path and looked up again by every write, so the file there
/// then is the one compared: an editor that saves by replacing the file leaves no stale
/// identity behind. One can be held as a search instead ([`Inputs::and_found_from`]), made
/// only by a write that meets a file at its target.
#[derive(Debug, Clone, Default)]
pub(crate) struct Inputs {
    files: Vec<PathBuf>,
    found: Option<Found>,
}

/// An input found from a path rather than named — the `mds.json` nearest a directory run's
/// source (#425): the file `find` finds from `from`, if any.
#[derive(Debug, Clone)]
struct Found {
    from: PathBuf,
    find: fn(&Path) -> Option<PathBuf>,
}

impl Inputs {
    /// The files at `files`, each as the run reads it — through a symlink.
    pub(crate) fn new(files: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            files: files.into_iter().collect(),
            found: None,
        }
    }

    /// These inputs and the file `find` finds from `from`. The search is made only by a
    /// write that meets a file at its target, and then at that moment, after the files
    /// named: where nothing is, nothing is replaced, so nothing needs to be found.
    pub(crate) fn and_found_from(self, from: &Path, find: fn(&Path) -> Option<PathBuf>) -> Self {
        Self {
            found: Some(Found {
                from: from.to_path_buf(),
                find,
            }),
            ..self
        }
    }

    /// Each input's path: the files named, then the one found, searched for only once
    /// every file named has been taken.
    fn paths(&self) -> impl Iterator<Item = Cow<'_, Path>> {
        let named = self.files.iter().map(|file| Cow::Borrowed(file.as_path()));
        let found = self
            .found
            .iter()
            .filter_map(|found| (found.find)(&found.from))
            .map(Cow::Owned);
        named.chain(found)
    }
}

/// Write `content` to `target` as [`atomic_write_file`] does, but only as a new file
/// (#160): the temporary file is given the target's name only where nothing has it at
/// that moment (see the module docs), so a file that appears after any earlier look at
/// the target — `mds init`'s own check included — is never replaced.
///
/// # Errors
///
/// [`NotCreated::Exists`] for a target that is there — before the write or appearing
/// while it runs: the file is left as it is and no temporary file is left behind, and the
/// caller words the refusal, as it words its own earlier look's. Any other failure is
/// [`NotCreated::Failed`], worded as [`atomic_write_file`] words it — a symlink at the
/// target is refused as it refuses one.
pub(crate) fn create_new(
    target: &WriteTarget,
    content: &str,
    durability: Durability,
    parents: Parents,
) -> std::result::Result<(), NotCreated> {
    write_below_anchor(target, content, durability, parents, Commit::New)
        .map(drop)
        .map_err(|failure| match failure {
            Failure::Exists => NotCreated::Exists,
            failure => NotCreated::Failed(worded(target, failure)),
        })
}

/// Why [`create_new`], or [`write_over_own`], did not write its file.
#[derive(Debug)]
pub(crate) enum NotCreated {
    /// Something has the target's name: it was there, or it appeared while the write ran
    /// — for [`write_over_own`], anything but the caller's own file, unchanged.
    Exists,
    /// The write failed, as [`atomic_write_file`] words a failure.
    Failed(mds::MdsError),
}

/// Write `content` to `target` over the file its caller wrote there — while it still holds
/// exactly `own`, what was last written, and only until the rename, as
/// [`replace_if_unchanged`] replaces the file it read — or else only where nothing has the
/// target's name, as [`create_new`] writes (#160). `own` is `None` when the caller wrote
/// nothing there. `mds watch` writes the output of a source's new kind so: a file the
/// session did not write, or one changed since it wrote it, is never replaced.
///
/// # Errors
///
/// [`NotCreated::Exists`] for anything else at the target — a file with other bytes, or
/// one changed before the rename, a symlink, a directory, a FIFO, a socket or a device —
/// left as it is, with no temporary file left behind. Any other failure is
/// [`NotCreated::Failed`], worded as [`atomic_write_file`] words it — a file at the
/// target that cannot be read again included, with the read's cause: whether it is still
/// the caller's cannot be told, and it is left as it is.
pub(crate) fn write_over_own(
    target: &WriteTarget,
    own: Option<&str>,
    content: &str,
    durability: Durability,
    parents: Parents,
) -> std::result::Result<(), NotCreated> {
    let held = match own {
        Some(own) => own_file(target, own),
        None => Ok(None),
    };
    let written = match held {
        Ok(Some(held)) => {
            pause_before_replace();
            imp::replace_held(held, content.as_bytes(), durability)
        }
        // Only a new file may take the name, and the commit, never the read, finds whether
        // something has it.
        Ok(None) => write_below_anchor(target, content, durability, parents, Commit::New).map(drop),
        Err(failure) => Err(failure),
    };
    written.map_err(|failure| match failure {
        Failure::Exists | Failure::Changed | Failure::LinkAtTarget | Failure::NotARegularFile => {
            NotCreated::Exists
        }
        failure => NotCreated::Failed(worded(target, failure)),
    })
}

/// [`write_over_own`]'s read of `target`, as a rewrite reads its file: held when it holds
/// exactly `own`; `None` when it is not the caller's — other bytes, gone, a symlink,
/// anything but a regular file — or when nothing is on the way to it, a directory below
/// the anchor gone since; and a read that fails otherwise — the file may not be read,
/// say — is the write's failure, never taken for a file that is not the caller's.
fn own_file(target: &WriteTarget, own: &str) -> std::result::Result<Option<imp::Held>, Failure> {
    let below = Below::of(target)?;
    match imp::read_stamped(&below, target.checked_anchor(), own.as_bytes()) {
        Ok(held) => Ok(Some(held)),
        Err(Failure::Changed | Failure::LinkAtTarget | Failure::NotARegularFile) => Ok(None),
        Err(Failure::Io(e)) if imp::nothing_below(&e) => Ok(None),
        Err(failure) => Err(failure),
    }
}

/// [`atomic_write_file`], [`write_compiled`] and [`create_new`] share this: resolve
/// `target` below its anchor, then write through it as `commit` says; the directory the
/// file went in is given back — on unix still open, for its caller to close.
fn write_below_anchor(
    target: &WriteTarget,
    content: &str,
    durability: Durability,
    parents: Parents,
    commit: Commit<'_, &imp::Stamp>,
) -> std::result::Result<imp::Dir, Failure> {
    let below = Below::of(target)?;
    let anchor = target.checked_anchor();
    imp::write(
        &below,
        anchor,
        content.as_bytes(),
        durability,
        parents,
        commit,
    )
}

/// How the temporary file a write has filled takes the target's name.
#[derive(Debug, Clone, Copy)]
enum Commit<'i, S> {
    /// Renamed over whatever file is there — for a rewrite, only while that file still
    /// holds the stamp `S` it was read with.
    Replace(Option<S>),
    /// Renamed over whatever file is there, unless it is one of the files the run reads, or
    /// an MDS module while the output itself is none (`module`) ([`write_compiled`], #425).
    Output { inputs: &'i Inputs, module: bool },
    /// Given the name only where nothing has it ([`create_new`]).
    New,
}

/// A file `mds fmt` or `mds lint --fix` is about to rewrite, read again by
/// [`read_stamped`] and held for [`replace_if_unchanged`] (#160): the directory it was
/// read in, still open on unix, its name there, and a stamp of the file as it was read.
pub(crate) struct ReadForRewrite {
    target: WriteTarget,
    held: imp::Held,
}

/// Read `target` again for its rewrite, below its anchor and through no symlink there or
/// at the file, and hold it if its bytes are `read` — the text the rewrite was made from,
/// which the caller read by path, with every check a source read makes (#160).
///
/// What is held is the directory the file was read in — its descriptor, on unix — and a
/// stamp of the file: on unix its device, inode, size and modification and status-change
/// times; on Windows, which has no descriptor-relative walk in std, its size and its
/// modification and creation times, by path. [`replace_if_unchanged`] writes into that
/// directory, whatever its path leads to by then, and only over that file.
///
/// # Errors
///
/// `mds::io`, worded as [`atomic_write_file`] words a failure — a symlink below the
/// anchor or at the file is refused as it refuses one — and, for a file whose bytes are
/// not `read` any more, or that is gone, `"<file>" changed since it was read; not
/// written`, the file named by `target.shown`.
pub(crate) fn read_stamped(
    target: &WriteTarget,
    read: &str,
) -> std::result::Result<ReadForRewrite, mds::MdsError> {
    let below = Below::of(target).map_err(|e| io_error(&target.shown, io_cause(&e)))?;
    let held = imp::read_stamped(&below, target.checked_anchor(), read.as_bytes())
        .map_err(|f| worded(target, f))?;
    Ok(ReadForRewrite {
        target: target.clone(),
        held,
    })
}

/// Replace the file `read` holds with `content`, by way of a temporary file in the
/// directory it was read in, unless it has changed since it was read (#160): just before
/// the rename the file is looked at again in that directory, and a stamp that differs
/// refuses the rewrite, leaving the file — the edit made to it — as it is, and no
/// temporary file.
///
/// # Errors
///
/// `mds::io`: `"<file>" changed since it was read; not written` for a file that changed,
/// else as [`atomic_write_file`] words a failure.
pub(crate) fn replace_if_unchanged(
    read: ReadForRewrite,
    content: &str,
    durability: Durability,
) -> std::result::Result<(), mds::MdsError> {
    pause_before_replace();
    let ReadForRewrite { target, held } = read;
    imp::replace_held(held, content.as_bytes(), durability).map_err(|f| worded(&target, f))
}

/// Remove `target` once it is shown to be a regular file `proof` accepts, below its anchor
/// and through no symlink there or at the file (#160; see the module docs): `proof` reads
/// the file as opened in the directory the walk reached, and the name is removed from that
/// directory only while it is still that file. A target that names the directory its
/// caller checked ([`WriteTarget::below_checked_anchor`]) is removed only below that one,
/// as a write is made only there. Nothing is created, and a file that is not there —
/// nor, below the anchor, a directory on its way — is nothing to remove.
///
/// # Errors
///
/// [`NotRemoved`], the file left as it is, with a cause that names no path but the
/// shown form of a refused directory, for the caller to word after the file's name.
pub(crate) fn remove_proven(
    target: &WriteTarget,
    proof: impl FnOnce(&mut std::fs::File) -> std::io::Result<bool>,
) -> std::result::Result<Removal, NotRemoved> {
    let below = Below::of(target).map_err(|e| NotRemoved::Failed(safe_inline(io_cause(&e))))?;
    imp::remove(&below, target.checked_anchor(), proof).map_err(|f| not_removed(target, f))
}

/// What [`remove_proven`] found at its file, and did.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Removal {
    /// The file was proven, and is removed.
    Removed,
    /// Nothing has its name, nor — below the anchor — a directory on its way.
    Missing,
    /// A regular file its proof does not accept: left as it is.
    Kept,
}

/// Why [`remove_proven`] did not remove a file that is, or may be, there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NotRemoved {
    /// Not a regular file — a directory, a FIFO, a socket or a device: never opened.
    NotAFile,
    /// A symlink, live or dangling: refused, never followed — mds writes none.
    Link,
    /// It could not be looked at or read, so nothing is known of it: the cause, escaped.
    Unreadable(String),
    /// Refused — a symlink below the anchor, an anchor that is not the directory checked,
    /// another file at its name by the time it was to go — or the removal failed: the
    /// cause, escaped.
    Failed(String),
}

impl NotRemoved {
    /// The refusal of a removal below an out-dir that now leads to a different directory
    /// than the one the `mds watch` session started with, as [`out_dir_moved`] refuses a
    /// write there (#160).
    pub(crate) fn out_dir_moved() -> Self {
        Self::Failed(OUT_DIR_MOVED.to_owned())
    }

    /// Why the file was not removed, as its caller's message gives it after the file's
    /// name.
    pub(crate) fn cause(&self) -> &str {
        match self {
            Self::NotAFile => NOT_A_REGULAR_FILE,
            Self::Link => SYMLINK_REMOVAL_REFUSAL,
            Self::Unreadable(cause) | Self::Failed(cause) => cause,
        }
    }
}

/// `failure`, a removal of `target` that did not happen, as [`NotRemoved`] gives it.
fn not_removed(target: &WriteTarget, failure: Failure) -> NotRemoved {
    let cause = |e: &std::io::Error| safe_inline(io_cause(e));
    match failure {
        Failure::NotARegularFile => NotRemoved::NotAFile,
        Failure::Unreadable(e) => NotRemoved::Unreadable(cause(&e)),
        Failure::AnchorMoved => NotRemoved::out_dir_moved(),
        Failure::Changed => NotRemoved::Failed(CHANGED_WHILE_CHECKED.to_owned()),
        Failure::LinkBelowAnchor { depth } => NotRemoved::Failed(format!(
            "{FOLLOW_REFUSAL} at {}",
            safe_path(&shown_directory(target, depth))
        )),
        Failure::LinkAtTarget => NotRemoved::Link,
        // Only a new file's commit meets a file at its name, and only an output's refuses a
        // module or an input; a removal never does either.
        Failure::Exists => NotRemoved::Failed(cause(&std::io::ErrorKind::AlreadyExists.into())),
        Failure::Module => NotRemoved::Failed(MODULE_REFUSAL.to_owned()),
        Failure::Input => NotRemoved::Failed(INPUT_REFUSAL.to_owned()),
        Failure::UnreadMarkdown(e) | Failure::Io(e) => NotRemoved::Failed(cause(&e)),
    }
}

/// `failure`, a write of `target` that did not land, as the `mds::io` error it reports.
fn worded(target: &WriteTarget, failure: Failure) -> mds::MdsError {
    match failure {
        Failure::AnchorMoved => out_dir_moved(target),
        Failure::Changed => mds::MdsError::Io {
            message: format!(
                "\"{}\" changed since it was read; not written",
                safe_path(&target.shown)
            ),
        },
        // Only a new file's commit meets a file at its target, and [`create_new`] hands that
        // to its caller to word; any other write that did would have failed like any other.
        Failure::Exists => io_error(
            &target.shown,
            io_cause(&std::io::ErrorKind::AlreadyExists.into()),
        ),
        Failure::LinkBelowAnchor { depth } => {
            io_error(&shown_directory(target, depth), FOLLOW_REFUSAL.to_owned())
        }
        Failure::LinkAtTarget => io_error(&target.shown, SYMLINK_REFUSAL.to_owned()),
        Failure::Module => io_error(&target.shown, MODULE_REFUSAL.to_owned()),
        Failure::Input => io_error(&target.shown, INPUT_REFUSAL.to_owned()),
        Failure::UnreadMarkdown(e) => mds::MdsError::Io {
            message: format!(
                "cannot write {}: {MODULE_UNKNOWN}: {}",
                safe_path(&target.shown),
                safe_inline(io_cause(&e))
            ),
        },
        Failure::NotARegularFile => io_error(&target.shown, NOT_A_REGULAR_FILE.to_owned()),
        // Only a removal's look at its file, or its proof's read, fails as unreadable.
        Failure::Unreadable(e) | Failure::Io(e) => io_error(&target.shown, io_cause(&e)),
    }
}

/// Why `mds init` refuses a target that is there — found by its own look, or met by
/// [`create_new`]'s commit ([`NotCreated::Exists`]) — after the file's name.
pub(crate) const ALREADY_EXISTS: &str = "already exists (use --force to overwrite)";

/// Why [`atomic_write_file`] refuses to replace a symlink at its target.
const SYMLINK_REFUSAL: &str = "refusing to replace a symlink";

/// Why [`remove_proven`] refuses to remove a symlink at its file: mds writes none.
const SYMLINK_REMOVAL_REFUSAL: &str = "refusing to remove a symlink";

/// Why [`write_compiled`] refuses to replace an MDS module (#425).
const MODULE_REFUSAL: &str = "refusing to replace an MDS module";

/// Why [`write_compiled`] refuses to replace one of the files the run reads (#425).
const INPUT_REFUSAL: &str = "refusing to replace a file this run reads";

/// Why [`write_compiled`] refuses to replace a `.md` file it cannot read, before the
/// read's cause: nothing tells it is no MDS module (#425).
const MODULE_UNKNOWN: &str = "cannot tell whether it is an MDS module";

/// Why [`remove_proven`] leaves a file whose name, by the time it was to be removed, was
/// another file's than the one its proof read.
const CHANGED_WHILE_CHECKED: &str = "changed while it was checked";

/// Why [`atomic_write_file`] refuses to replace, and [`remove_proven`] to remove, a FIFO, a
/// socket, a device or a directory.
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
    /// The file a rewrite read is not as it was read: its bytes, or its stamp, differ, or
    /// it is gone; or another file has the name of the one a removal proved.
    Changed,
    /// A new file's target is there, or appeared before the commit ([`create_new`]).
    Exists,
    /// The directory `depth` levels below the anchor is a symlink.
    LinkBelowAnchor { depth: usize },
    /// The target itself is a symlink.
    LinkAtTarget,
    /// An output's target is an MDS module ([`write_compiled`], #425).
    Module,
    /// An output's target is one of the files the run reads ([`write_compiled`], #425).
    Input,
    /// An output's target, a `.md` file, could not be opened or read to tell whether it
    /// is an MDS module ([`write_compiled`], #425).
    UnreadMarkdown(std::io::Error),
    /// The target itself is a FIFO, a socket or a device — or, for a removal, a
    /// directory.
    NotARegularFile,
    /// A file to be removed could not be looked at, or its proof could not read it.
    Unreadable(std::io::Error),
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

/// Whether `name` can be a `.md` file's: an MDS module when its frontmatter declares
/// `type: mds` (#425). The extension is taken in any case, since on a case-insensitive
/// volume `LIB.MD` is the file `lib.md`, which mds-core judges by its name on disk.
fn names_markdown(name: &OsStr) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
}

/// Whether `name` can be a `.mds` file's: an MDS source, which mds-core takes for a module
/// by its name alone, whatever it holds (#425). The extension is taken in any case, as
/// [`names_markdown`] takes it.
fn names_source(name: &OsStr) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mds"))
}

/// `name`, the last component of a path, as code units — UTF-16 on Windows — less the
/// dots and spaces it ends in, which Windows drops from the last component of a path it
/// opens: there `lib.md.` and `lib.md ` are the file `lib.md`, so the name a write is
/// judged by is this one (#425). A name of nothing but dots and spaces is kept whole.
#[cfg(any(windows, test))]
fn as_windows_opens<T: Copy + Into<u32>>(name: &[T]) -> &[T] {
    match name
        .iter()
        .rposition(|&unit| !matches!(unit.into(), 0x2E | 0x20))
    {
        Some(last) => &name[..=last],
        None => name,
    }
}

/// Whether `file`, opened at a `.md` file's name ([`names_markdown`]), is an MDS module an
/// output must never replace (#425): its frontmatter declares `type: mds`, as mds-core's
/// own check takes a module ([`mds::check_module_type`]). That check looks no further
/// than the frontmatter — from the fence on the first line to the first `\n---` after it —
/// so no more is read: nothing past a first line that is no fence, and past a fence one
/// read at a time until the close, at most one read beyond it, and no further than
/// [`mds::MAX_FILE_SIZE`] bytes in all — a fence that does not close by then is no module,
/// as mds-core reads no more of any module. Bytes that are not UTF-8 are judged with each
/// replaced.
fn is_mds_module(file: &mut std::fs::File) -> std::io::Result<bool> {
    /// The longest frontmatter fence: `---` and a CRLF.
    const FENCE: &[u8] = b"---\r\n";
    /// What closes the frontmatter for mds-core: the first `---` that begins a line.
    const CLOSE: &[u8] = b"\n---";
    /// How much one read past the fence takes.
    const ONE_READ: u64 = 8 * 1024;
    let fence = u64::try_from(FENCE.len()).unwrap_or(u64::MAX);
    let mut head = mds::read_at_most(file, fence, 0)?;
    let after_fence = if head.starts_with(b"---\n") {
        FENCE.len() - 1
    } else if head.starts_with(FENCE) {
        FENCE.len()
    } else {
        return Ok(false);
    };
    // Where the close is looked for: from the end of the fence, then from just before the
    // bytes the last read added, so a close split between two reads is found.
    let mut from = after_fence;
    for _ in 0..=mds::MAX_FILE_SIZE / ONE_READ {
        if let Some(at) = head[from..]
            .windows(CLOSE.len())
            .position(|bytes| bytes == CLOSE)
        {
            head.truncate(from + at + CLOSE.len());
            break;
        }
        let held = u64::try_from(head.len()).unwrap_or(u64::MAX);
        let more = mds::read_at_most(
            file,
            ONE_READ.min(mds::MAX_FILE_SIZE.saturating_sub(held)),
            0,
        )?;
        if more.is_empty() {
            break;
        }
        from = head.len().saturating_sub(CLOSE.len() - 1).max(after_fence);
        head.extend(more);
    }
    Ok(declares_a_module(&String::from_utf8_lossy(&head)))
}

/// Whether `text`, a `.md` file's, declares `type: mds` in its frontmatter: an MDS module,
/// as mds-core's own check takes one ([`mds::check_module_type`], #425). The check is given
/// a constant `.md` key — mds-core judges a `.md` key by the text alone — so no path
/// becomes text.
fn declares_a_module(text: &str) -> bool {
    const MARKDOWN_KEY: &str = "module.md";
    let key = mds::ModuleRef::keyed(MARKDOWN_KEY).typed(MARKDOWN_KEY);
    mds::check_module_type(key, text).is_ok()
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

    use super::{
        Below, Beside, Commit, DirIdentity, Durability, Failure, Inputs, Parents, Removal,
        TEMP_PREFIX, TEMP_SUFFIX,
    };

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

    /// The file a new file's commit writes in place, on a filesystem without hard links: a
    /// new file, never one already there, and never through a symlink.
    const IN_PLACE: OFlags = OFlags::WRONLY
        .union(OFlags::CREATE)
        .union(OFlags::EXCL)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);

    /// A file a rewrite reads again, a removal's proof reads, or an output's target read to
    /// tell an MDS module (#425): never through a symlink, and never waiting on a FIFO put
    /// in its place, which the read then refuses as no longer the file read, the removal as
    /// not a regular file, and the output's look as no module.
    const READ: OFlags = OFlags::RDONLY
        .union(OFlags::NOFOLLOW)
        .union(OFlags::NONBLOCK)
        .union(OFlags::CLOEXEC);

    /// The mode a new file asks for: `0666`, which the umask then narrows, as for
    /// `std::fs::write`.
    const NEW_FILE: Mode = Mode::from_raw_mode(0o666);

    /// The mode a file that replaces another is created with: owner-only, so its bytes are
    /// never readable by anyone the file it replaces did not allow.
    const OWNER_ONLY: Mode = Mode::RUSR.union(Mode::WUSR);

    /// The bits of its mode a replaced file lends the file that replaces it: read, write
    /// and execute for its owner, its group and others — never setuid, setgid or sticky.
    const PERMISSION_BITS: fs::RawMode = 0o777;

    /// The mode a directory a write creates asks for: `0777`, narrowed by the umask.
    const NEW_DIR: Mode = Mode::from_raw_mode(0o777);

    /// How many temporary names a write tries before it gives up: each is random, so a
    /// clash is another writer's file or a leftover, and sixteen in a row is not chance.
    pub(super) const MAX_TEMP_ATTEMPTS: usize = 16;

    /// The directory a write's file went in: the descriptor its walk opened, closed when it
    /// drops.
    pub(super) type Dir = OwnedFd;

    /// Write `content` to `below.name` in the directory [`walk`] opens, as `commit` says,
    /// and give that directory back.
    pub(super) fn write(
        below: &Below<'_>,
        anchor: Option<DirIdentity>,
        content: &[u8],
        durability: Durability,
        parents: Parents,
        commit: Commit<'_, &Stamp>,
    ) -> Result<Dir, Failure> {
        let dir = walk(below, anchor, parents)?;
        replace(dir, below.name, content, durability, commit)
    }

    /// Whether anything has `name` in `dir`, looked at without following a symlink: only a
    /// name nothing has is [`Beside::Nothing`]; a failed look is [`Beside::Something`].
    pub(super) fn look(dir: &Dir, name: &OsStr) -> Beside {
        match fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
            Err(Errno::NOENT) => Beside::Nothing,
            _ => Beside::Something,
        }
    }

    /// Open the directory `below.name` is in: the anchor by path — and, when `anchor`
    /// names the directory it must be, refuse another, and one gone, which is never made
    /// again by path — then each directory below it from the one above without following
    /// a symlink.
    fn walk(
        below: &Below<'_>,
        anchor: Option<DirIdentity>,
        parents: Parents,
    ) -> Result<OwnedFd, Failure> {
        let mut dir = match anchor {
            Some(checked) => match open_anchor(&below.anchor, Parents::Existing) {
                Ok(dir) => opened_as_checked(dir, checked)?,
                Err(Failure::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(Failure::AnchorMoved)
                }
                Err(failure) => return Err(failure),
            },
            None => open_anchor(&below.anchor, parents)?,
        };
        for (depth, name) in below.dirs.iter().enumerate() {
            let next = open_below(dir.as_fd(), name, parents)
                .map_err(|errno| below_failure(dir.as_fd(), name, depth, errno))?;
            dir = next;
        }
        Ok(dir)
    }

    /// A file as a rewrite read it (#160): its device and inode, its size, and the times
    /// it was last modified and last changed, each to the nanosecond where the filesystem
    /// keeps one. Writing to the file, renaming another over it, or changing its mode
    /// changes at least one of them — within the resolution of the filesystem's clock.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) struct Stamp {
        device: i128,
        inode: i128,
        size: i128,
        modified: (i128, i128),
        changed: (i128, i128),
    }

    impl Stamp {
        /// The stamp `stat` gives. Every field widens without loss, whatever integer type
        /// the platform gives it.
        fn of(stat: &fs::Stat) -> Self {
            Self {
                device: i128::from(stat.st_dev),
                inode: i128::from(stat.st_ino),
                size: i128::from(stat.st_size),
                modified: (i128::from(stat.st_mtime), i128::from(stat.st_mtime_nsec)),
                changed: (i128::from(stat.st_ctime), i128::from(stat.st_ctime_nsec)),
            }
        }

        /// Whether `name` in `dir`, looked at without following a symlink, is still the
        /// file this stamp was taken of, unchanged.
        fn holds(&self, dir: BorrowedFd<'_>, name: &OsStr) -> bool {
            fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)
                .is_ok_and(|stat| Self::of(&stat) == *self)
        }
    }

    /// A file a rewrite read, held for its replace: the directory it was read in, still
    /// open, its name there, and its stamp.
    pub(super) struct Held {
        dir: OwnedFd,
        name: OsString,
        stamp: Stamp,
    }

    /// Read `below.name` again in the directory [`walk`] opens — below `anchor`, the
    /// directory checked, when there is one — without following a symlink, and hold it if
    /// its bytes are `read`: the stamp is taken of the file opened, before its bytes are
    /// read, so a change made while they are read changes it too.
    pub(super) fn read_stamped(
        below: &Below<'_>,
        anchor: Option<DirIdentity>,
        read: &[u8],
    ) -> Result<Held, Failure> {
        let dir = walk(below, anchor, Parents::Existing)?;
        let mut file = match open_to_read(dir.as_fd(), below.name) {
            Ok(file) => file,
            Err(Errno::LOOP) => return Err(Failure::LinkAtTarget),
            Err(Errno::NOENT) => return Err(Failure::Changed),
            // A socket refuses the open, and so can a device: no longer the file read, as
            // nothing but a regular file is (below) — not a file that cannot be read.
            Err(_) if is_no_regular_file(dir.as_fd(), below.name) => return Err(Failure::Changed),
            Err(e) => return Err(e.into()),
        };
        let stat = fs::fstat(&file)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Err(Failure::Changed);
        }
        // One byte more than was read tells a file that grew from one that did not.
        let expected = u64::try_from(read.len()).unwrap_or(u64::MAX);
        if mds::read_at_most(&mut file, expected.saturating_add(1), expected)? != read {
            return Err(Failure::Changed);
        }
        Ok(Held {
            dir,
            name: below.name.to_owned(),
            stamp: Stamp::of(&stat),
        })
    }

    /// Replace the file `held` was read from with `content`, in the directory it was read
    /// in, unless its stamp has changed by the time of the rename.
    pub(super) fn replace_held(
        held: Held,
        content: &[u8],
        durability: Durability,
    ) -> Result<(), Failure> {
        let commit = Commit::Replace(Some(&held.stamp));
        replace(held.dir, &held.name, content, durability, commit).map(drop)
    }

    /// Remove `below.name` from the directory [`walk`] opens — creating none — once it is
    /// a regular file `proof` accepts, and only while the name is still that file (#160).
    /// It is looked at without following a symlink first, so anything but a regular file
    /// is never opened; then opened without following one, and never waiting on a FIFO
    /// put in its place; its [`Stamp`], taken of the file opened before `proof` reads it,
    /// must be the name's again, looked at once `proof` has read it, before it is
    /// unlinked.
    pub(super) fn remove(
        below: &Below<'_>,
        anchor: Option<DirIdentity>,
        proof: impl FnOnce(&mut File) -> std::io::Result<bool>,
    ) -> Result<Removal, Failure> {
        let dir = match walk(below, anchor, Parents::Existing) {
            Ok(dir) => dir,
            Err(Failure::Io(e)) if nothing_below(&e) => return Ok(Removal::Missing),
            Err(failure) => return Err(failure),
        };
        let name = below.name;
        match fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => match FileType::from_raw_mode(stat.st_mode) {
                FileType::RegularFile => {}
                FileType::Symlink => return Err(Failure::LinkAtTarget),
                _ => return Err(Failure::NotARegularFile),
            },
            // A name too long for the file system names no file: none can be there.
            Err(Errno::NOENT | Errno::NAMETOOLONG) => return Ok(Removal::Missing),
            Err(e) => return Err(Failure::Unreadable(e.into())),
        }
        let mut file = match open_to_read(dir.as_fd(), name) {
            Ok(file) => file,
            Err(Errno::LOOP) => return Err(Failure::LinkAtTarget),
            Err(Errno::NOENT) => return Ok(Removal::Missing),
            Err(e) => return Err(Failure::Unreadable(e.into())),
        };
        let opened = fs::fstat(&file).map_err(|e| Failure::Unreadable(e.into()))?;
        if FileType::from_raw_mode(opened.st_mode) != FileType::RegularFile {
            return Err(Failure::NotARegularFile);
        }
        // Taken of the file opened before `proof` reads it, so an edit made while or after
        // it is read changes it too.
        let stamp = Stamp::of(&opened);
        if !proof(&mut file).map_err(Failure::Unreadable)? {
            return Ok(Removal::Kept);
        }
        drop(file);
        // The file proven must still be the one at the name, unchanged: another put there
        // since, or the same file written over after it was read, is left as it is.
        match fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(now) if Stamp::of(&now) == stamp => {}
            Ok(_) => return Err(Failure::Changed),
            Err(Errno::NOENT) => return Ok(Removal::Missing),
            Err(e) => return Err(e.into()),
        }
        match fs::unlinkat(&dir, name, AtFlags::empty()) {
            Ok(()) => Ok(Removal::Removed),
            Err(Errno::NOENT) => Ok(Removal::Missing),
            Err(e) => Err(e.into()),
        }
    }

    /// Whether `e`, from the walk to a file to be removed or read again, says a directory
    /// on its way is not there, or is no directory: then no file is there.
    pub(super) fn nothing_below(e: &std::io::Error) -> bool {
        matches!(Errno::from_io_error(e), Some(Errno::NOENT | Errno::NOTDIR))
    }

    /// Open `name` in `dir` to read it: a rewrite's second read, or a removal's proof.
    fn open_to_read(dir: BorrowedFd<'_>, name: &OsStr) -> Result<File, Errno> {
        fs::openat(dir, name, READ, Mode::empty()).map(File::from)
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

    /// Whether `name` in `dir`, looked at without following a symlink, is something other
    /// than a regular file; a look that fails tells nothing, and is not.
    fn is_no_regular_file(dir: BorrowedFd<'_>, name: &OsStr) -> bool {
        fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)
            .is_ok_and(|stat| FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile)
    }

    /// Put `content` at `name` in `dir`, by way of a temporary file beside it, as `commit`
    /// says: renamed over the file there — when it stamps the file a rewrite read, only if
    /// the file there still holds it, and for an output only if it is no MDS module, each
    /// looked at just before the rename ([`ready_to_replace`]) — or given the name only
    /// where nothing has it ([`commit_new`]); and give `dir` back.
    fn replace(
        dir: OwnedFd,
        name: &OsStr,
        content: &[u8],
        durability: Durability,
        commit: Commit<'_, &Stamp>,
    ) -> Result<Dir, Failure> {
        // The target is looked at, never opened: a FIFO would block the open.
        let replaced = match fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => match (FileType::from_raw_mode(stat.st_mode), commit) {
                (FileType::Symlink, _) => return Err(Failure::LinkAtTarget),
                // A new file is never put where anything is.
                (_, Commit::New) => return Err(Failure::Exists),
                (FileType::RegularFile, _) => Some(stat),
                // A directory has no mode a file should take: the rename below refuses to
                // replace it, and the temporary file is unlinked again.
                (FileType::Directory, _) => None,
                _ => return Err(Failure::NotARegularFile),
            },
            Err(Errno::NOENT) => None,
            Err(e) => return Err(e.into()),
        };
        let (mut temp, file) = match &replaced {
            Some(replaced) => create_replacement(dir.as_fd(), replaced)?,
            None => create_temp(dir.as_fd(), NEW_FILE, temp_names())?,
        };
        fill(file, content, durability)?;
        match commit {
            Commit::Replace(_) | Commit::Output { .. } => {
                // The temporary file is unlinked again as `temp` drops.
                ready_to_replace(dir.as_fd(), name, commit)?;
                fs::renameat(dir.as_fd(), &temp.name, dir.as_fd(), name)?;
                temp.renamed = true;
                drop(temp);
            }
            Commit::New => {
                // A file that appears while the run pauses here is met by the commit
                // itself, not by the look above.
                super::pause_before_replace();
                commit_new(temp, name, content, durability, &NO_CLOBBER)?;
            }
        }
        if durability == Durability::Fsync {
            return Ok(sync_directory(dir)?);
        }
        Ok(dir)
    }

    /// The temporary file that is to replace the regular file `replaced` describes, in
    /// `dir` (#160): created owner-only, then given that file's permission bits when the
    /// user who owns that file owns this one too ([`kept_mode`]). Beside a file another
    /// user owns it is made again as a new file is, its mode the umask's: that user chose
    /// that file's mode, and lends it to no one else's file.
    fn create_replacement<'d>(
        dir: BorrowedFd<'d>,
        replaced: &fs::Stat,
    ) -> Result<(Temp<'d>, File), Failure> {
        let (temp, file) = create_temp(dir, OWNER_ONLY, temp_names())?;
        let same_owner = fs::fstat(&file)?.st_uid == replaced.st_uid;
        match kept_mode(replaced.st_mode, same_owner) {
            Some(mode) => {
                fs::fchmod(&file, mode)?;
                Ok((temp, file))
            }
            None => {
                // Unlinked again as it drops, before the new file is created.
                drop((temp, file));
                Ok(create_temp(dir, NEW_FILE, temp_names())?)
            }
        }
    }

    /// The mode a file with mode `st_mode` lends the file that replaces it: its permission
    /// bits alone ([`PERMISSION_BITS`]), and only when one user owns both (`same_owner`) —
    /// else none.
    pub(super) fn kept_mode(st_mode: fs::RawMode, same_owner: bool) -> Option<Mode> {
        same_owner.then(|| Mode::from_raw_mode(st_mode & PERMISSION_BITS))
    }

    /// Whether the rename over `name` in `dir` may go on, as `commit` says, looked at just
    /// before it: a rewrite's file must still hold the stamp it was read with, and an
    /// output's target must be no file the run reads, nor an MDS module unless the output
    /// is one (#425).
    fn ready_to_replace(
        dir: BorrowedFd<'_>,
        name: &OsStr,
        commit: Commit<'_, &Stamp>,
    ) -> Result<(), Failure> {
        match commit {
            Commit::Replace(Some(stamp)) if !stamp.holds(dir, name) => Err(Failure::Changed),
            Commit::Output { inputs, module } => never_over_an_input(dir, name, inputs, module),
            _ => Ok(()),
        }
    }

    /// Refuse the rename over `name` in `dir` when the file there is one of `inputs`, or an
    /// MDS module while the output is none (`module`) (#425). It is looked at without
    /// following a symlink: nothing there, a symlink — which the rename replaces, never
    /// writing through it — and anything else that is no regular file are neither. For an
    /// output that is no module, a regular file there is judged by its name and, for a
    /// `.md` file's, read as opened in `dir` ([`is_a_module`]); any regular file is compared
    /// with each of `inputs` by its device and inode ([`is_an_input`]).
    fn never_over_an_input(
        dir: BorrowedFd<'_>,
        name: &OsStr,
        inputs: &Inputs,
        module: bool,
    ) -> Result<(), Failure> {
        let stat = match fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile => stat,
            Ok(_) | Err(Errno::NOENT) => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        if !module && is_a_module(dir, name)? {
            return Err(Failure::Module);
        }
        if is_an_input(inputs, &stat) {
            return Err(Failure::Input);
        }
        Ok(())
    }

    /// Whether the file `stat` describes is one of `inputs`: the same device and inode as
    /// the file each names now, looked up through a symlink as the run read it.
    fn is_an_input(inputs: &Inputs, stat: &fs::Stat) -> bool {
        inputs.paths().any(|file| {
            fs::stat(&*file)
                .is_ok_and(|input| input.st_dev == stat.st_dev && input.st_ino == stat.st_ino)
        })
    }

    /// Whether `name` in `dir`, a regular file a moment ago, is an MDS module (#425): a
    /// `.mds` file by its name alone ([`super::names_source`]), never read; a `.md` file
    /// ([`super::names_markdown`]) opened in `dir` — never by path — without following a
    /// symlink, and read there, that [`super::is_mds_module`] takes for one. Any other name
    /// is none, and so is a `.md` file gone since, a symlink or anything else that is no
    /// regular file by then; a `.md` file that cannot be opened or read is
    /// [`Failure::UnreadMarkdown`].
    fn is_a_module(dir: BorrowedFd<'_>, name: &OsStr) -> Result<bool, Failure> {
        if super::names_source(name) {
            return Ok(true);
        }
        if !super::names_markdown(name) {
            return Ok(false);
        }
        let judge = || -> std::io::Result<bool> {
            let mut file = match open_to_read(dir, name) {
                Ok(file) => file,
                // Gone since the look, or a symlink put there since: no module to replace.
                Err(Errno::NOENT | Errno::LOOP) => return Ok(false),
                Err(e) => return Err(e.into()),
            };
            let opened = fs::fstat(&file)?;
            if FileType::from_raw_mode(opened.st_mode) != FileType::RegularFile {
                return Ok(false);
            }
            super::is_mds_module(&mut file)
        };
        judge().map_err(Failure::UnreadMarkdown)
    }

    /// Write `content` to `file` and, in the [`Durability::Fsync`] tier, sync it; then
    /// close it.
    fn fill(mut file: File, content: &[u8], durability: Durability) -> std::io::Result<()> {
        file.write_all(content)?;
        if durability == Durability::Fsync {
            file.sync_all()?;
        }
        Ok(())
    }

    /// One step of a new file's commit: give the file `from` in `dir` the name `to` there,
    /// never over a file that has it.
    pub(super) type Step<'s> = &'s dyn Fn(BorrowedFd<'_>, &OsStr, &OsStr) -> Result<(), Errno>;

    /// The steps a new file's commit takes, in order (#160): a rename that never replaces,
    /// then a hard link, which never replaces either; each is passed over when it is not
    /// to be had (see [`commit_new`]).
    pub(super) struct NewSteps<'s> {
        pub(super) rename: Step<'s>,
        pub(super) link: Step<'s>,
    }

    /// The steps a new file's commit takes.
    pub(super) const NO_CLOBBER: NewSteps<'static> = NewSteps {
        rename: &rename_noreplace,
        link: &link_new,
    };

    /// Give `temp`'s file the name `name` in the directory it is in, only where nothing has
    /// that name: by `steps.rename`; where that is not to be had — `EINVAL`, `ENOSYS`,
    /// `ENOTSUP` or `EOPNOTSUPP`: no such rename on this platform, kernel or filesystem — by
    /// `steps.link`, the temporary name then removed as `temp` drops; and where that is
    /// not to be had either — `EPERM`, `ENOSYS`, `ENOTSUP`, `EOPNOTSUPP` or `EMLINK`: a
    /// filesystem without hard links — by writing `content` in place, into a file created
    /// exclusively ([`write_in_place`]). `EEXIST` from any step, a file that appeared since
    /// the target was looked at, refuses the write and leaves that file as it is.
    pub(super) fn commit_new(
        mut temp: Temp<'_>,
        name: &OsStr,
        content: &[u8],
        durability: Durability,
        steps: &NewSteps<'_>,
    ) -> Result<(), Failure> {
        let dir = temp.dir;
        match (steps.rename)(dir, &temp.name, name) {
            Ok(()) => {
                temp.renamed = true;
                Ok(())
            }
            Err(errno) if no_such_rename(errno) => match (steps.link)(dir, &temp.name, name) {
                Ok(()) => Ok(()),
                Err(errno) if no_hard_links(errno) => {
                    write_in_place(dir, name, content, durability)
                }
                Err(errno) => Err(refused_if_there(errno)),
            },
            Err(errno) => Err(refused_if_there(errno)),
        }
    }

    /// Rename `from` to `to` in `dir`, never over a file at `to`:
    /// `renameat2(RENAME_NOREPLACE)` on Linux and Android, `renameatx_np(RENAME_EXCL)` on
    /// Apple platforms.
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "visionos",
        target_os = "watchos"
    ))]
    fn rename_noreplace(dir: BorrowedFd<'_>, from: &OsStr, to: &OsStr) -> Result<(), Errno> {
        fs::renameat_with(dir, from, dir, to, fs::RenameFlags::NOREPLACE)
    }

    /// No rename that never replaces on this platform: not to be had, so the commit goes on
    /// to a hard link.
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "visionos",
        target_os = "watchos"
    )))]
    fn rename_noreplace(_: BorrowedFd<'_>, _: &OsStr, _: &OsStr) -> Result<(), Errno> {
        Err(Errno::NOSYS)
    }

    /// Link `to` in `dir` to the file `from` is, which fails on a name already taken.
    fn link_new(dir: BorrowedFd<'_>, from: &OsStr, to: &OsStr) -> Result<(), Errno> {
        fs::linkat(dir, from, dir, to, AtFlags::empty())
    }

    /// Write `content` to `name` in `dir`, created exclusively and without following a
    /// symlink: a new file's commit on a filesystem without hard links. No file there is
    /// replaced, but the write is not all or nothing — one that fails part-way leaves the
    /// file partly written.
    fn write_in_place(
        dir: BorrowedFd<'_>,
        name: &OsStr,
        content: &[u8],
        durability: Durability,
    ) -> Result<(), Failure> {
        let file = fs::openat(dir, name, IN_PLACE, NEW_FILE).map_err(refused_if_there)?;
        fill(File::from(file), content, durability)?;
        Ok(())
    }

    /// Whether `errno`, from a rename that never replaces, says there is no such rename.
    fn no_such_rename(errno: Errno) -> bool {
        [Errno::INVAL, Errno::NOSYS, Errno::NOTSUP, Errno::OPNOTSUPP].contains(&errno)
    }

    /// Whether `errno`, from a hard link, says the filesystem makes none.
    fn no_hard_links(errno: Errno) -> bool {
        [
            Errno::PERM,
            Errno::NOSYS,
            Errno::NOTSUP,
            Errno::OPNOTSUPP,
            Errno::MLINK,
        ]
        .contains(&errno)
    }

    /// `errno` from a step of a new file's commit, as the write's failure: `EEXIST`, a
    /// file at the name, is the refusal.
    fn refused_if_there(errno: Errno) -> Failure {
        if errno == Errno::EXIST {
            Failure::Exists
        } else {
            Failure::from(errno)
        }
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
    /// has landed, so the write stands. Any other failure is the write's. The directory is
    /// given back.
    fn sync_directory(dir: OwnedFd) -> std::io::Result<OwnedFd> {
        let dir = File::from(dir);
        settle_directory_sync(
            || dir.sync_all(),
            || fs::fsync(&dir).map_err(std::io::Error::from),
        )?;
        Ok(OwnedFd::from(dir))
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
    use std::ffi::{OsStr, OsString};
    use std::io::Write as _;
    use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    use super::{
        Below, Beside, Commit, DirIdentity, Durability, Failure, Inputs, Parents, Removal,
        TEMP_PREFIX, TEMP_SUFFIX,
    };

    /// `ERROR_PATH_NOT_FOUND`: a directory that is not there, in the operating system's
    /// words.
    const PATH_NOT_FOUND: i32 = 3;

    /// The directory a write's file went in, by path.
    pub(super) type Dir = PathBuf;

    /// Write `content` to `below.name`, in the directory [`walk`] checks, by path (the
    /// residual the module docs describe), as `commit` says, refusing a symlink at the
    /// target; and give that directory back.
    pub(super) fn write(
        below: &Below<'_>,
        anchor: Option<DirIdentity>,
        content: &[u8],
        durability: Durability,
        parents: Parents,
        commit: Commit<'_, &Stamp>,
    ) -> Result<Dir, Failure> {
        let dir = walk(below, anchor, parents)?;
        let target = dir.join(below.name);
        match (std::fs::symlink_metadata(&target), commit) {
            (Ok(meta), _) if meta.file_type().is_symlink() => return Err(Failure::LinkAtTarget),
            // A new file is never put where anything is.
            (Ok(_), Commit::New) => return Err(Failure::Exists),
            (Err(e), _) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        replace(&dir, &target, content, durability, commit)?;
        Ok(dir)
    }

    /// Whether anything has `name` in `dir`, looked at by path without following a
    /// symlink: only a name nothing has is [`Beside::Nothing`]; a failed look is
    /// [`Beside::Something`].
    pub(super) fn look(dir: &Dir, name: &OsStr) -> Beside {
        match std::fs::symlink_metadata(dir.join(name)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Beside::Nothing,
            _ => Beside::Something,
        }
    }

    /// A file as a rewrite read it, by path (#160): its size and the times it was last
    /// modified and created.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) struct Stamp {
        size: u64,
        modified: Option<SystemTime>,
        created: Option<SystemTime>,
    }

    impl Stamp {
        /// The stamp `meta` gives.
        fn of(meta: &std::fs::Metadata) -> Self {
            Self {
                size: meta.len(),
                modified: meta.modified().ok(),
                created: meta.created().ok(),
            }
        }

        /// Whether `file`, looked at by path without following a symlink, is still the
        /// regular file this stamp was taken of, unchanged.
        fn holds(&self, file: &Path) -> bool {
            std::fs::symlink_metadata(file)
                .is_ok_and(|meta| meta.is_file() && Self::of(&meta) == *self)
        }
    }

    /// A file a rewrite read, held for its replace, by path: the directory it was read in,
    /// the file, and its stamp.
    pub(super) struct Held {
        dir: PathBuf,
        file: PathBuf,
        stamp: Stamp,
    }

    /// Read `below.name` again in the directory [`walk`] checks — below `anchor`, the
    /// directory checked, when there is one — by path, refusing a symlink there, and hold
    /// it if its bytes are `read`.
    pub(super) fn read_stamped(
        below: &Below<'_>,
        anchor: Option<DirIdentity>,
        read: &[u8],
    ) -> Result<Held, Failure> {
        let dir = walk(below, anchor, Parents::Existing)?;
        let target = dir.join(below.name);
        match std::fs::symlink_metadata(&target) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(Failure::LinkAtTarget),
            // A directory, which an open as a file refuses: no longer the file read, as
            // nothing but a regular file is (below) — not a file that cannot be read.
            Ok(meta) if !meta.is_file() => return Err(Failure::Changed),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Failure::Changed),
            Err(e) => return Err(e.into()),
        }
        let mut opened = std::fs::File::open(&target)?;
        let meta = opened.metadata()?;
        if !meta.is_file() {
            return Err(Failure::Changed);
        }
        // One byte more than was read tells a file that grew from one that did not.
        let expected = u64::try_from(read.len()).unwrap_or(u64::MAX);
        if mds::read_at_most(&mut opened, expected.saturating_add(1), expected)? != read {
            return Err(Failure::Changed);
        }
        Ok(Held {
            dir,
            file: target,
            stamp: Stamp::of(&meta),
        })
    }

    /// Replace the file `held` was read from with `content`, unless its stamp has changed
    /// by the time it is replaced.
    pub(super) fn replace_held(
        held: Held,
        content: &[u8],
        durability: Durability,
    ) -> Result<(), Failure> {
        let commit = Commit::Replace(Some(&held.stamp));
        replace(&held.dir, &held.file, content, durability, commit)
    }

    /// Remove `below.name`, in the directory [`walk`] checks — creating none — once it is
    /// a regular file `proof` accepts, refusing a symlink or a junction there; the file is
    /// closed again, looked at by path for the [`Stamp`] it was opened with, and removed
    /// by path (the residual the module docs describe).
    pub(super) fn remove(
        below: &Below<'_>,
        anchor: Option<DirIdentity>,
        proof: impl FnOnce(&mut std::fs::File) -> std::io::Result<bool>,
    ) -> Result<Removal, Failure> {
        let dir = match walk(below, anchor, Parents::Existing) {
            Ok(dir) => dir,
            Err(Failure::Io(e)) if nothing_below(&e) => return Ok(Removal::Missing),
            Err(failure) => return Err(failure),
        };
        let target = dir.join(below.name);
        match std::fs::symlink_metadata(&target) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(Failure::LinkAtTarget),
            Ok(meta) if !meta.is_file() => return Err(Failure::NotARegularFile),
            Ok(_) => {}
            // A name the file system cannot hold — too long (`ERROR_FILENAME_EXCED_RANGE`)
            // or not a name it accepts (`ERROR_INVALID_NAME`) — names no file.
            Err(e) if no_such_name(&e) => return Ok(Removal::Missing),
            Err(e) => return Err(Failure::Unreadable(e)),
        }
        let mut file = match std::fs::File::open(&target) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Removal::Missing),
            Err(e) => return Err(Failure::Unreadable(e)),
        };
        // Taken of the file opened before `proof` reads it, so an edit made while or after
        // it is read changes it too.
        let stamp = Stamp::of(&file.metadata().map_err(Failure::Unreadable)?);
        if !proof(&mut file).map_err(Failure::Unreadable)? {
            return Ok(Removal::Kept);
        }
        drop(file);
        // The file proven must still be at the name, unchanged: one written over after it
        // was read, or another put there with another size or other times, is left.
        match std::fs::symlink_metadata(&target) {
            Ok(now) if now.is_file() && Stamp::of(&now) == stamp => {}
            Ok(_) => return Err(Failure::Changed),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Removal::Missing),
            Err(e) => return Err(e.into()),
        }
        match std::fs::remove_file(&target) {
            Ok(()) => Ok(Removal::Removed),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Removal::Missing),
            Err(e) => Err(e.into()),
        }
    }

    /// Whether `e`, from the walk to a file to be removed or read again, says a directory
    /// on its way is not there, or is no directory: then no file is there.
    pub(super) fn nothing_below(e: &std::io::Error) -> bool {
        matches!(
            e.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
        ) || e.raw_os_error() == Some(PATH_NOT_FOUND)
    }

    /// Whether `e`, from the look at a file to be removed, says no file has its name: none
    /// is there, or the file system cannot hold the name, which std reports as
    /// `InvalidFilename`.
    fn no_such_name(e: &std::io::Error) -> bool {
        matches!(
            e.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidFilename
        )
    }

    /// Put `content` at `target` in `dir`, by way of a temporary file beside it, as
    /// `commit` says: persisted over the file there — when it stamps the file a rewrite
    /// read, only if `target` still holds it, and for an output only if it is no MDS
    /// module, each looked at just before ([`ready_to_replace`]) — or moved into place
    /// without `MOVEFILE_REPLACE_EXISTING`, which fails on a file that is there.
    fn replace(
        dir: &Path,
        target: &Path,
        content: &[u8],
        durability: Durability,
        commit: Commit<'_, &Stamp>,
    ) -> Result<(), Failure> {
        let mut temp = tempfile::Builder::new()
            .prefix(TEMP_PREFIX)
            .suffix(TEMP_SUFFIX)
            .tempfile_in(dir)?;
        temp.as_file_mut().write_all(content)?;
        if durability == Durability::Fsync {
            temp.as_file().sync_all()?;
        }
        match commit {
            Commit::Replace(_) | Commit::Output { .. } => {
                // The temporary file is deleted again as `temp` drops.
                ready_to_replace(target, commit)?;
                temp.persist(target).map_err(|e| e.error)?;
            }
            Commit::New => {
                // A file that appears while the run pauses here is met by the move itself.
                super::pause_before_replace();
                // The temporary file is deleted again as the error drops.
                temp.persist_noclobber(target).map_err(|e| {
                    if e.error.kind() == std::io::ErrorKind::AlreadyExists {
                        Failure::Exists
                    } else {
                        Failure::from(e.error)
                    }
                })?;
            }
        }
        Ok(())
    }

    /// Whether the move over `target` may go on, as `commit` says, looked at by path just
    /// before it: a rewrite's file must still hold the stamp it was read with, and an
    /// output's target must be no file the run reads, nor an MDS module unless the output
    /// is one (#425).
    fn ready_to_replace(target: &Path, commit: Commit<'_, &Stamp>) -> Result<(), Failure> {
        match commit {
            Commit::Replace(Some(stamp)) if !stamp.holds(target) => Err(Failure::Changed),
            Commit::Output { inputs, module } => never_over_an_input(target, inputs, module),
            _ => Ok(()),
        }
    }

    /// Refuse the move over `target` when the file there is one of `inputs`, or an MDS
    /// module while the output is none (`module`) (#425), by path. It is looked at without
    /// following a symlink: nothing there, a symlink — which the move replaces, never
    /// writing through it — and anything else that is no regular file are neither. For an
    /// output that is no module, a regular file there is judged by its name and, for a
    /// `.md` file's, read ([`is_a_module`]); any regular file is compared with each of
    /// `inputs` ([`is_an_input`]).
    fn never_over_an_input(target: &Path, inputs: &Inputs, module: bool) -> Result<(), Failure> {
        match std::fs::symlink_metadata(target) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        }
        if !module && is_a_module(target)? {
            return Err(Failure::Module);
        }
        if is_an_input(inputs, target)? {
            return Err(Failure::Input);
        }
        Ok(())
    }

    /// Whether `target` is one of `inputs`, by canonical path: std gives no file index on
    /// Windows, so a hard link to an input — another path — is not one (the residual the
    /// module docs describe). A target gone since the look is none — nothing to replace —
    /// and one whose canonical path cannot be had otherwise is an error, which refuses the
    /// write, as a look that fails does on unix.
    fn is_an_input(inputs: &Inputs, target: &Path) -> Result<bool, Failure> {
        let target = match std::fs::canonicalize(target) {
            Ok(target) => target,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        Ok(inputs
            .paths()
            .any(|file| std::fs::canonicalize(file).is_ok_and(|file| file == target)))
    }

    /// The name of the file `target` opens: its last component less the dots and spaces
    /// it ends in, which Windows drops ([`super::as_windows_opens`]) — so `-o "lib.md."`
    /// and `-o "lib.md "`, which write `lib.md`, are judged as `lib.md` (#425).
    fn opened_name(target: &Path) -> Option<OsString> {
        let name: Vec<u16> = target.file_name()?.encode_wide().collect();
        Some(OsString::from_wide(super::as_windows_opens(&name)))
    }

    /// Whether `target`, a regular file a moment ago, is an MDS module (#425), by the name
    /// of the file it opens ([`opened_name`]): a `.mds` file by its name alone
    /// ([`super::names_source`]), never read; a `.md` file ([`super::names_markdown`])
    /// opened and read by path, that [`super::is_mds_module`] takes for one. Any other name
    /// is none, and so is a `.md` file gone since; a `.md` file that cannot be opened or
    /// read is [`Failure::UnreadMarkdown`].
    fn is_a_module(target: &Path) -> Result<bool, Failure> {
        let Some(name) = opened_name(target) else {
            return Ok(false);
        };
        if super::names_source(&name) {
            return Ok(true);
        }
        if !super::names_markdown(&name) {
            return Ok(false);
        }
        let judge = || -> std::io::Result<bool> {
            let mut file = match std::fs::File::open(target) {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(e) => return Err(e),
            };
            super::is_mds_module(&mut file)
        };
        judge().map_err(Failure::UnreadMarkdown)
    }

    /// The directory `below.name` is in: the anchor, created first when `parents` says
    /// so — refused when `anchor` names another directory than the one there, or one
    /// gone, which is never made again — then each directory below it, refused when it is
    /// a symlink or a junction; all by path.
    fn walk(
        below: &Below<'_>,
        anchor: Option<DirIdentity>,
        parents: Parents,
    ) -> Result<PathBuf, Failure> {
        if parents == Parents::Create && anchor.is_none() {
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
        Ok(dir)
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

// ── Test-only pause before a rewrite's replace or a new file's commit (#160) ─

/// `MDS_TEST_PAUSE_BEFORE_REPLACE`: how a debug build is made to stop between a rewrite's
/// read and its replace, so that a test can change the file, or swap its directory, in
/// that window, and between [`create_new`]'s look at its target and its commit, so that a
/// test can put a file there (`tests/anchored_writes.rs`) — [`write_over_own`] stops at
/// the same two places. A release build has none of it.
#[cfg(debug_assertions)]
mod pause_trigger {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::{atomic_write_file, Durability, Parents};
    use crate::output::WriteTarget;

    /// The variable naming the file that ends the pause. The run writes the same name with
    /// `.paused` appended once it has stopped, for the test to wait for.
    const VARIABLE: &str = "MDS_TEST_PAUSE_BEFORE_REPLACE";

    /// How long the pause waits between two looks for the file that ends it.
    const POLL: Duration = Duration::from_millis(5);

    /// How many looks the pause makes before the replace goes on regardless: ten seconds.
    const MAX_POLLS: u32 = 2_000;

    /// Stop here when `MDS_TEST_PAUSE_BEFORE_REPLACE` names a file: say so by writing
    /// `<file>.paused`, then wait until `<file>` exists, or until [`MAX_POLLS`] looks have
    /// found none.
    pub(crate) fn pause_before_replace() {
        let Some(go) = std::env::var_os(VARIABLE).map(PathBuf::from) else {
            return;
        };
        let mut paused = go.clone().into_os_string();
        paused.push(".paused");
        // A marker that cannot be written leaves the test waiting for it, which the test
        // reports as a run that never paused.
        let _ = atomic_write_file(
            &WriteTarget::as_typed(PathBuf::from(paused)),
            "",
            Durability::RenameOnly,
            Parents::Existing,
        );
        for _ in 0..MAX_POLLS {
            if go.exists() {
                return;
            }
            std::thread::sleep(POLL);
        }
    }
}

#[cfg(debug_assertions)]
pub(crate) use pause_trigger::pause_before_replace;

/// A release build's pause before a rewrite's replace or a new file's commit: none.
#[cfg(not(debug_assertions))]
pub(crate) fn pause_before_replace() {}

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

    /// An anchor a check found is never made again by path (#160): a write that finds it
    /// gone is refused with the out-dir refusal and creates nothing, so no directory is
    /// made wherever that path leads by then; the next check finds the directory above it.
    /// Control: the same target, named with no directory to expect, is created by path,
    /// as any output's anchor is.
    #[test]
    fn a_checked_anchor_that_is_gone_is_not_made_again() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let identity = DirIdentity::of(&out).expect("a directory");
        std::fs::remove_dir(&out).unwrap();
        let target = WriteTarget::below(&out, Path::new("o"), Path::new("x.md"));

        let checked = target.below_checked_anchor(&out, 0, identity);
        let err = atomic_write_file(&checked, "X", Durability::RenameOnly, Parents::Create)
            .expect_err("the anchor checked is gone")
            .to_string();
        assert_eq!(
            err,
            format!("cannot write {}: {OUT_DIR_MOVED}", safe_path(&target.shown))
        );
        assert!(!out.exists(), "nothing is made by path");

        atomic_write_file(&target, "X", Durability::RenameOnly, Parents::Create).unwrap();
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

    // ── A rewrite over the bytes it read ─────────────────────────────────────────

    /// A rewrite's second read holds the file only as it was read (#160): other bytes,
    /// more bytes, a file gone and a symlink in its place are each refused; a file edited
    /// after the read is not replaced — the edit is left and no temporary file — and one
    /// as it was read is (control).
    ///
    /// Each edit changes the file's size: a filesystem whose clock is coarser than the
    /// time between two writes can give an edit of the same size the same times.
    #[cfg(unix)]
    #[test]
    fn a_rewrite_replaces_only_the_file_as_it_was_read() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.mds");
        let target = WriteTarget::new(file.clone(), PathBuf::from("a.mds"));
        let changed = format!(
            "\"{}\" changed since it was read; not written",
            safe_path(&target.shown)
        );
        let refused = |read: &str| {
            read_stamped(&target, read)
                .map(|_| ())
                .expect_err("not the file as read")
                .to_string()
        };
        std::fs::write(&file, "one").unwrap();
        assert_eq!(refused("two"), changed, "other bytes");
        assert_eq!(refused("on"), changed, "more bytes than were read");

        let read = read_stamped(&target, "one").unwrap();
        std::fs::write(&file, "edited").unwrap();
        let err = replace_if_unchanged(read, "ONE", Durability::Fsync)
            .expect_err("edited after the read")
            .to_string();
        assert_eq!(err, changed);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "edited");
        assert_eq!(temp_residue(dir.path()), Vec::<String>::new());

        let read = read_stamped(&target, "edited").unwrap();
        replace_if_unchanged(read, "EDITED", Durability::Fsync).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "EDITED");

        std::fs::remove_file(&file).unwrap();
        assert_eq!(refused("EDITED"), changed, "gone");
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), &file).unwrap();
        assert_eq!(
            refused("EDITED"),
            format!(
                "cannot write {}: {SYMLINK_REFUSAL}",
                safe_path(&target.shown)
            ),
            "a symlink in its place"
        );
    }

    // ── A new file, never over another ───────────────────────────────────────────

    /// [`create_new`] puts a file where nothing is, and refuses a target that is there
    /// ([`NotCreated::Exists`]), the file left as it is and no temporary file (#160). A
    /// symlink there is refused as a symlink, and nothing is written through it. Control:
    /// [`atomic_write_file`] replaces the same file.
    #[test]
    fn a_new_file_is_never_put_where_a_file_is() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.mds");
        let target = WriteTarget::new(file.clone(), PathBuf::from("a.mds"));
        let new = |content: &str| {
            create_new(&target, content, Durability::RenameOnly, Parents::Existing).map_err(
                |not_created| match not_created {
                    NotCreated::Exists => "exists".to_owned(),
                    NotCreated::Failed(e) => e.to_string(),
                },
            )
        };

        new("first").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "first");
        assert_eq!(new("second"), Err("exists".to_owned()));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "first");
        assert_eq!(temp_residue(dir.path()), Vec::<String>::new());

        atomic_write_file(
            &target,
            "replaced",
            Durability::RenameOnly,
            Parents::Existing,
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "replaced");

        std::fs::remove_file(&file).unwrap();
        let elsewhere = dir.path().join("elsewhere");
        if make_symlink(&elsewhere, &file) {
            assert_eq!(
                new("third"),
                Err(format!(
                    "cannot write {}: {SYMLINK_REFUSAL}",
                    safe_path(&target.shown)
                ))
            );
            assert!(!elsewhere.exists(), "nothing is written through the link");
        }
    }

    /// [`write_over_own`] of `target`, its directories already there, with what it did
    /// not write as text: `kept` for [`NotCreated::Exists`], else the error's.
    fn over_own(target: &WriteTarget, own: Option<&str>, content: &str) -> Result<(), String> {
        write_over_own(
            target,
            own,
            content,
            Durability::RenameOnly,
            Parents::Existing,
        )
        .map_err(|not_written| match not_written {
            NotCreated::Exists => "kept".to_owned(),
            NotCreated::Failed(e) => e.to_string(),
        })
    }

    /// [`write_over_own`] replaces only the caller's own file, unchanged, and otherwise
    /// writes only where nothing is (#160): nothing there — whether or not the caller wrote
    /// there before — and the file is created; the file holding exactly what the caller
    /// wrote is replaced; a file the caller did not write, one changed since, one with
    /// more bytes, a directory and a symlink — even to a file holding the caller's bytes —
    /// are kept ([`NotCreated::Exists`]), each as it was, nothing written through the
    /// link, and no temporary file left.
    #[test]
    fn a_write_over_its_own_file_replaces_no_other() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.md");
        let target = WriteTarget::new(file.clone(), PathBuf::from("a.md"));
        let text = || std::fs::read_to_string(&file).unwrap();
        let kept = Err("kept".to_owned());

        assert_eq!(over_own(&target, None, "first"), Ok(()), "nothing there");
        assert_eq!(text(), "first");
        assert_eq!(over_own(&target, None, "second"), kept, "not the caller's");
        assert_eq!(text(), "first");
        assert_eq!(
            over_own(&target, Some("first"), "second"),
            Ok(()),
            "the caller's own, unchanged"
        );
        assert_eq!(text(), "second");
        assert_eq!(
            over_own(&target, Some("first"), "third"),
            kept,
            "changed since the caller wrote it"
        );
        assert_eq!(
            over_own(&target, Some("secon"), "third"),
            kept,
            "more bytes"
        );
        assert_eq!(text(), "second");
        assert_eq!(temp_residue(dir.path()), Vec::<String>::new());

        std::fs::remove_file(&file).unwrap();
        assert_eq!(over_own(&target, Some("second"), "third"), Ok(()), "gone");
        assert_eq!(text(), "third");

        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        assert_eq!(
            over_own(&target, Some("third"), "fourth"),
            kept,
            "a directory"
        );
        assert!(file.is_dir(), "the directory is left");
        std::fs::remove_dir(&file).unwrap();

        let elsewhere = dir.path().join("elsewhere");
        std::fs::write(&elsewhere, "third").unwrap();
        if make_symlink(&elsewhere, &file) {
            for own in [Some("third"), None] {
                assert_eq!(over_own(&target, own, "fourth"), kept, "a symlink, {own:?}");
            }
            assert_eq!(
                std::fs::read_to_string(&elsewhere).unwrap(),
                "third",
                "nothing is written through the link"
            );
            assert!(std::fs::symlink_metadata(&file)
                .unwrap()
                .file_type()
                .is_symlink());
        }
        assert_eq!(temp_residue(dir.path()), Vec::<String>::new());
    }

    /// A FIFO at the target of [`write_over_own`] is kept, never opened — the write does
    /// not wait on it — whether or not the caller wrote there (#160).
    #[cfg(unix)]
    #[test]
    fn a_write_over_its_own_file_keeps_a_fifo() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.md");
        let made = std::process::Command::new("mkfifo")
            .arg(&file)
            .status()
            .expect("mkfifo runs");
        assert!(made.success(), "mkfifo a.md");
        let target = WriteTarget::new(file.clone(), PathBuf::from("a.md"));
        for own in [Some("first"), None] {
            assert_eq!(
                over_own(&target, own, "second"),
                Err("kept".to_owned()),
                "{own:?}"
            );
        }
        assert_eq!(
            entries(dir.path()),
            ["a.md"],
            "the FIFO is left, and no temporary file"
        );
    }

    /// A socket at the target of [`write_over_own`] is kept, whether or not the caller wrote
    /// there (#160): a socket refuses the read's open, and is no file of the caller's, as a
    /// FIFO is none.
    #[cfg(unix)]
    #[test]
    fn a_write_over_its_own_file_keeps_a_socket() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.md");
        let _socket = std::os::unix::net::UnixListener::bind(&file).unwrap();
        let target = WriteTarget::new(file.clone(), PathBuf::from("a.md"));
        for own in [Some("first"), None] {
            assert_eq!(
                over_own(&target, own, "second"),
                Err("kept".to_owned()),
                "{own:?}"
            );
        }
        assert_eq!(
            entries(dir.path()),
            ["a.md"],
            "the socket is left, and no temporary file"
        );
    }

    /// A file the caller wrote at the target of [`write_over_own`] that cannot be read is
    /// reported with the read's cause (#160), never kept as a file changed since — the
    /// caller cannot tell — and left as it is, with no temporary file. Control: the same
    /// file, readable again, is replaced. Skipped, with a reason, where mode 0o200 does not
    /// stop a read (running as root).
    #[cfg(unix)]
    #[test]
    fn a_write_over_its_own_file_reports_a_file_it_cannot_read() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.md");
        let target = WriteTarget::new(file.clone(), PathBuf::from("a.md"));
        let set_mode = |mode: u32| {
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        std::fs::write(&file, "first").unwrap();
        set_mode(0o200);
        let Err(denied) = std::fs::File::open(&file) else {
            crate::output::ewriteln!("running as root; mode 0o200 does not stop a read");
            return;
        };

        let failed = over_own(&target, Some("first"), "second");
        set_mode(0o644);
        assert_eq!(
            failed,
            Err(format!(
                "cannot write {}: {}",
                safe_path(&target.shown),
                io_cause(&denied)
            ))
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "first", "left");
        assert_eq!(temp_residue(dir.path()), Vec::<String>::new());

        assert_eq!(over_own(&target, Some("first"), "second"), Ok(()));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "second");
    }

    /// A directory gone since the caller wrote below it is no failure of
    /// [`write_over_own`] (#160): nothing is there, so the file is written as a new one,
    /// the directory made again where the caller makes its directories.
    #[test]
    fn a_write_over_its_own_file_in_a_directory_gone_since_makes_it_again() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sub").join("a.md");
        let target = WriteTarget::below(dir.path(), Path::new("o"), Path::new("sub/a.md"));
        let written = |own: Option<&str>| {
            write_over_own(
                &target,
                own,
                "second",
                Durability::RenameOnly,
                Parents::Create,
            )
            .map_err(|not_written| match not_written {
                NotCreated::Exists => "kept".to_owned(),
                NotCreated::Failed(e) => e.to_string(),
            })
        };
        assert_eq!(written(None), Ok(()), "control: made where nothing is");
        std::fs::remove_dir_all(dir.path().join("sub")).unwrap();

        assert_eq!(written(Some("second")), Ok(()));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "second");
    }

    // ── Never over an MDS module (#425) ─────────────────────────────────────────

    /// An output is never written over an MDS module (#425): a `.md` file that declares
    /// `type: mds` — bare, quoted, or with CRLF lines, and named with its extension in
    /// another case, which on a case-insensitive volume is a `.md` file's name — and any
    /// `.mds` file, whatever it holds and in either case, are refused, named as shown, and
    /// left as they are, with no temporary file. Controls: a `.md` file with no
    /// frontmatter, one that declares another `type`, one with `type: mds` below another
    /// key only, another kind of file, and names nothing has are each written.
    #[test]
    fn an_output_is_never_written_over_an_mds_module() {
        let dir = tempfile::tempdir().unwrap();
        let shown = |name: &str| PathBuf::from("out").join(name);
        let write = |name: &str| {
            write_compiled(
                &WriteTarget::new(dir.path().join(name), shown(name)),
                "X",
                &Inputs::default(),
            )
            .map_err(|e| e.to_string())
        };
        let text = |name: &str| std::fs::read_to_string(dir.path().join(name)).unwrap();
        for (name, module) in [
            ("m.md", "---\ntype: mds\n---\nM\n"),
            ("q.md", "---\ntype: \"mds\"\n---\nQ\n"),
            ("crlf.md", "---\r\ntype: 'mds'\r\n---\r\nC\r\n"),
            ("UP.MD", "---\ntype: mds\n---\nU\n"),
            ("mixed.Md", "---\ntype: mds\n---\nX\n"),
            ("source.mds", "---\ntype: mds\n---\nS\n"),
            ("plain.mds", "Hello\n"),
            ("UP.MDS", ""),
        ] {
            std::fs::write(dir.path().join(name), module).unwrap();
            assert_eq!(
                write(name),
                Err(format!(
                    "cannot write {}: {MODULE_REFUSAL}",
                    shown(name).display()
                )),
                "{name}"
            );
            assert_eq!(text(name), module, "{name} is left as it is");
        }
        assert_eq!(
            temp_residue(dir.path()),
            Vec::<String>::new(),
            "no temporary file"
        );
        for (name, other) in [
            ("plain.md", Some("plain\n")),
            ("other.md", Some("---\ntype: other\n---\nO\n")),
            ("nested.md", Some("---\nconfig:\n  type: mds\n---\nN\n")),
            ("notes.txt", Some("---\ntype: mds\n---\nT\n")),
            ("new.md", None),
            ("new.mds", None),
        ] {
            if let Some(other) = other {
                std::fs::write(dir.path().join(name), other).unwrap();
            }
            assert_eq!(write(name), Ok(()), "control: {name} is written");
            assert_eq!(text(name), "X", "control: {name}");
        }
    }

    /// An output that is itself an MDS module — its frontmatter declares `type: mds`, as
    /// mds-core judges a `.md` file's — may replace one (#425): a template that generates
    /// a module rewrites it, and a module written by hand at its output is replaced too.
    /// It is still never written over a file the run reads, module or not. Controls: an
    /// output that is no module is refused over the same module, and one that declares
    /// `type: mds` only below another key is no module either.
    #[test]
    fn a_module_is_replaced_only_by_output_that_is_itself_a_module() {
        const GENERATED: &str = "---\ntype: mds\n---\nGenerated\n";
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str| dir.path().join(name);
        let write = |name: &str, content: &str, inputs: &Inputs| {
            write_compiled(
                &WriteTarget::new(path(name), PathBuf::from(name)),
                content,
                inputs,
            )
            .map_err(|e| e.to_string())
        };
        let text = |name: &str| std::fs::read_to_string(path(name)).unwrap();
        let module = "---\ntype: mds\n---\nM\n";
        std::fs::write(path("m.md"), module).unwrap();

        assert_eq!(
            write("m.md", "plain\n", &Inputs::default()),
            Err(format!("cannot write m.md: {MODULE_REFUSAL}")),
            "control: an output that is no module"
        );
        assert_eq!(
            write(
                "m.md",
                "---\nconfig:\n  type: mds\n---\nN\n",
                &Inputs::default()
            ),
            Err(format!("cannot write m.md: {MODULE_REFUSAL}")),
            "control: type: mds below another key is no module"
        );
        assert_eq!(text("m.md"), module, "left as it is");

        assert_eq!(write("m.md", GENERATED, &Inputs::default()), Ok(()));
        assert_eq!(text("m.md"), GENERATED, "a module replaces a module");
        std::fs::write(path("lib.mds"), "Hello\n").unwrap();
        assert_eq!(
            write("lib.mds", "plain\n", &Inputs::default()),
            Err(format!("cannot write lib.mds: {MODULE_REFUSAL}")),
            "control: an output that is no module, over a .mds file"
        );
        assert_eq!(write("lib.mds", GENERATED, &Inputs::default()), Ok(()));
        assert_eq!(text("lib.mds"), GENERATED, "a module replaces a .mds file");
        let crlf = "---\r\ntype: 'mds'\r\n---\r\nAgain\r\n";
        assert_eq!(write("m.md", crlf, &Inputs::default()), Ok(()));
        assert_eq!(text("m.md"), crlf, "and its own output again");

        assert_eq!(
            write("m.md", GENERATED, &Inputs::new([path("m.md")])),
            Err(format!("cannot write m.md: {INPUT_REFUSAL}")),
            "never over a file the run reads"
        );
        assert_eq!(text("m.md"), crlf, "the input is left as it is");
        assert_eq!(
            temp_residue(dir.path()),
            Vec::<String>::new(),
            "no temporary file"
        );
    }

    /// The module check reads the file in the directory the write opened, never by path
    /// (#425): while a thread points the symlinked anchor `cur` at a directory holding a
    /// module, then at one holding a plain file, and back, at most 2,000 writes in at most
    /// 10 s never replace the module — each lands in the plain file's directory or is
    /// refused. Controls: the loop met both — some writes landed, some were refused.
    #[cfg(unix)]
    #[test]
    fn a_module_is_judged_in_the_directory_the_write_opened() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        /// How long each state is held: about as long as a write takes.
        const HOLD: Duration = Duration::from_micros(200);
        const MODULE: &str = "---\ntype: mds\n---\nM\n";

        let dir = tempfile::tempdir().unwrap();
        let (with_module, plain) = (dir.path().join("with-module"), dir.path().join("plain"));
        std::fs::create_dir(&with_module).unwrap();
        std::fs::create_dir(&plain).unwrap();
        std::fs::write(with_module.join("a.md"), MODULE).unwrap();
        std::fs::write(plain.join("a.md"), "plain\n").unwrap();
        let cur = dir.path().join("cur");
        std::os::unix::fs::symlink(&with_module, &cur).unwrap();
        let target = WriteTarget::below(&cur, Path::new("cur"), Path::new("a.md"));

        let stop = Arc::new(AtomicBool::new(false));
        let swapper = {
            let stop = Arc::clone(&stop);
            let (base, with_module, plain) =
                (dir.path().to_path_buf(), with_module.clone(), plain.clone());
            std::thread::spawn(move || {
                // Bounded: at most 100,000 swaps, and the loop ends with the writes. Each
                // swap renames a new link over `cur`, so `cur` always leads somewhere.
                for i in 0..100_000u32 {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let next = if i % 2 == 0 { &plain } else { &with_module };
                    let link = base.join(format!("link-{i}"));
                    let _ = std::os::unix::fs::symlink(next, &link);
                    let _ = std::fs::rename(&link, base.join("cur"));
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
            match write_compiled(&target, "X", &Inputs::default()) {
                Ok(()) => written += 1,
                Err(e) if e.to_string().contains(MODULE_REFUSAL) => refused += 1,
                Err(_) => {}
            }
        }
        stop.store(true, Ordering::Relaxed);
        swapper.join().unwrap();

        assert_eq!(
            std::fs::read_to_string(with_module.join("a.md")).unwrap(),
            MODULE,
            "the module is never replaced ({written} written, {refused} refused)"
        );
        assert!(
            written > 0 && refused > 0,
            "the loop met both directories: {written} written, {refused} refused"
        );
    }

    /// An output is never written over a file the run reads (#425): `vars.json`, one of
    /// the inputs, is refused, named as shown, and left as it is, with no temporary file.
    /// Controls: the same write with no inputs writes it, and a file that is no input is
    /// written.
    #[test]
    fn an_output_is_never_written_over_a_file_the_run_reads() {
        let dir = tempfile::tempdir().unwrap();
        let (vars, other) = (dir.path().join("vars.json"), dir.path().join("other.json"));
        std::fs::write(&vars, "{}\n").unwrap();
        std::fs::write(&other, "{}\n").unwrap();
        let inputs = Inputs::new([vars.clone()]);
        let write = |path: &Path, inputs: &Inputs| {
            write_compiled(
                &WriteTarget::new(path.to_path_buf(), PathBuf::from("out/x.json")),
                "X",
                inputs,
            )
            .map_err(|e| e.to_string())
        };

        assert_eq!(
            write(&vars, &inputs),
            Err(format!(
                "cannot write {}: {INPUT_REFUSAL}",
                Path::new("out/x.json").display()
            ))
        );
        assert_eq!(
            std::fs::read_to_string(&vars).unwrap(),
            "{}\n",
            "left as it is"
        );
        assert_eq!(
            temp_residue(dir.path()),
            Vec::<String>::new(),
            "no temporary file"
        );
        assert_eq!(
            write(&other, &inputs),
            Ok(()),
            "control: no input is written"
        );
        assert_eq!(
            write(&vars, &Inputs::default()),
            Ok(()),
            "control: no inputs"
        );
        assert_eq!(std::fs::read_to_string(&vars).unwrap(), "X");
    }

    /// An input found from a path rather than named — the `mds.json` nearest a directory
    /// run's source (#425) — is searched for only by a write that meets a file at its
    /// target, and a file found there is refused as any input is, and left as it is. A new
    /// file is written without the search: nothing is there to replace. Control: a file at
    /// the target that is no input is searched for once, and written over.
    #[test]
    fn an_input_found_from_a_path_is_searched_for_only_when_a_file_is_at_the_target() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static SEARCHES: AtomicUsize = AtomicUsize::new(0);
        fn beside(from: &Path) -> Option<PathBuf> {
            SEARCHES.fetch_add(1, Ordering::SeqCst);
            Some(from.with_file_name("mds.json"))
        }
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str| dir.path().join(name);
        let inputs = Inputs::default().and_found_from(&path("page.mds"), beside);
        let write = |name: &str| {
            write_compiled(
                &WriteTarget::new(path(name), PathBuf::from(name)),
                "X",
                &inputs,
            )
            .map_err(|e| e.to_string())
        };
        let searches = || SEARCHES.load(Ordering::SeqCst);

        assert_eq!(write("page.md"), Ok(()));
        assert_eq!(searches(), 0, "a new file is written without the search");
        assert_eq!(
            write("page.md"),
            Ok(()),
            "control: no input is written over"
        );
        assert_eq!(
            searches(),
            1,
            "control: a file at the target is searched for"
        );
        std::fs::write(path("mds.json"), "{}\n").unwrap();
        assert_eq!(
            write("mds.json"),
            Err(format!("cannot write mds.json: {INPUT_REFUSAL}"))
        );
        assert_eq!(searches(), 2);
        assert_eq!(
            std::fs::read_to_string(path("mds.json")).unwrap(),
            "{}\n",
            "left as it is"
        );
    }

    /// A compiled output's write looks at a file beside it, in the directory it went in
    /// (#160): nothing there is [`Beside::Nothing`]; a file, a directory and, on unix, a
    /// dangling symlink there are [`Beside::Something`], each left as it is. A file named
    /// in another directory is not looked at, and is something. The output is written in
    /// every case.
    #[test]
    fn a_compiled_output_s_write_looks_at_the_file_beside_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str| dir.path().join(name);
        let target = WriteTarget::new(path("a.md"), PathBuf::from("a.md"));
        let look = |beside: &WriteTarget| {
            write_compiled_and_look(&target, "X", &Inputs::default(), beside)
                .map_err(|e| e.to_string())
        };
        let named = |name: &str| target.sibling(|file| file.with_file_name(name));

        assert_eq!(look(&named("a.json")), Ok(Beside::Nothing), "nothing there");
        std::fs::write(path("a.json"), "[]\n").unwrap();
        assert_eq!(look(&named("a.json")), Ok(Beside::Something), "a file");
        assert_eq!(
            std::fs::read_to_string(path("a.json")).unwrap(),
            "[]\n",
            "left as it is"
        );
        std::fs::create_dir(path("d.json")).unwrap();
        assert_eq!(look(&named("d.json")), Ok(Beside::Something), "a directory");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(path("gone"), path("l.json")).unwrap();
            assert_eq!(
                look(&named("l.json")),
                Ok(Beside::Something),
                "a dangling symlink"
            );
        }
        std::fs::create_dir(path("sub")).unwrap();
        std::fs::write(path("sub").join("e.json"), "[]\n").unwrap();
        let elsewhere = WriteTarget::new(path("sub").join("e.json"), PathBuf::from("e.json"));
        assert_eq!(
            look(&elsewhere),
            Ok(Beside::Something),
            "another directory is not looked at"
        );
        assert_eq!(std::fs::read_to_string(path("a.md")).unwrap(), "X");
    }

    /// On unix an input is told by its device and inode (#425): a hard link to it is that
    /// file and is refused, and so is the file an input named through a symlink leads to.
    /// Control: a copy of it, another file with the same bytes, is written.
    #[cfg(unix)]
    #[test]
    fn an_input_is_the_file_its_path_leads_to_now() {
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str| dir.path().join(name);
        std::fs::write(path("vars.json"), "{}\n").unwrap();
        std::fs::hard_link(path("vars.json"), path("linked.json")).unwrap();
        std::os::unix::fs::symlink(path("vars.json"), path("via.json")).unwrap();
        std::fs::write(path("copy.json"), "{}\n").unwrap();
        let write = |name: &str, inputs: &Inputs| {
            write_compiled(
                &WriteTarget::new(path(name), PathBuf::from(name)),
                "X",
                inputs,
            )
            .map_err(|e| e.to_string())
        };

        let refused = |name: &str| Err(format!("cannot write {name}: {INPUT_REFUSAL}"));
        assert_eq!(
            write("linked.json", &Inputs::new([path("vars.json")])),
            refused("linked.json")
        );
        assert_eq!(
            write("vars.json", &Inputs::new([path("via.json")])),
            refused("vars.json")
        );
        assert_eq!(std::fs::read_to_string(path("vars.json")).unwrap(), "{}\n");
        assert_eq!(
            write("copy.json", &Inputs::new([path("vars.json")])),
            Ok(()),
            "control: a copy is another file"
        );
    }

    /// A `.md` file at an output's target that cannot be read is refused (#425): nothing
    /// tells it is no MDS module. `locked.md`, at mode 0o000, is refused, named as shown,
    /// with the read's cause, and left as it is, with no temporary file. Controls: the same
    /// file, readable again, is written; and `locked.json` at mode 0o000 — a name no
    /// module has, so the write never opens it — is written. `locked.mds` at mode 0o000 is
    /// refused as a module, by its name, without a read. Skipped, with a reason, where mode
    /// 0o000 does not stop a read (running as root).
    #[cfg(unix)]
    #[test]
    fn an_output_is_never_written_over_a_markdown_file_it_cannot_read() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str| dir.path().join(name);
        let shown = |name: &str| PathBuf::from("out").join(name);
        let write = |name: &str| {
            write_compiled(
                &WriteTarget::new(path(name), shown(name)),
                "X",
                &Inputs::default(),
            )
            .map_err(|e| e.to_string())
        };
        let set_mode = |name: &str, mode: u32| {
            std::fs::set_permissions(path(name), std::fs::Permissions::from_mode(mode)).unwrap();
        };
        std::fs::write(path("locked.md"), "notes\n").unwrap();
        std::fs::write(path("locked.json"), "{}\n").unwrap();
        set_mode("locked.md", 0o000);
        set_mode("locked.json", 0o000);
        let Err(denied) = std::fs::File::open(path("locked.md")) else {
            crate::output::ewriteln!("running as root; mode 0o000 does not stop a read");
            return;
        };

        let refused = write("locked.md");
        set_mode("locked.md", 0o644);
        assert_eq!(
            refused,
            Err(format!(
                "cannot write {}: {MODULE_UNKNOWN}: {}",
                shown("locked.md").display(),
                io_cause(&denied)
            ))
        );
        assert_eq!(
            std::fs::read_to_string(path("locked.md")).unwrap(),
            "notes\n",
            "left as it is"
        );
        assert_eq!(
            temp_residue(dir.path()),
            Vec::<String>::new(),
            "no temporary file"
        );

        assert_eq!(
            write("locked.md"),
            Ok(()),
            "control: readable, it is written"
        );
        assert_eq!(std::fs::read_to_string(path("locked.md")).unwrap(), "X");
        assert_eq!(
            write("locked.json"),
            Ok(()),
            "control: a .json file is never opened"
        );
        set_mode("locked.json", 0o644);
        assert_eq!(std::fs::read_to_string(path("locked.json")).unwrap(), "X");

        // A `.mds` file is a module by its name alone: refused as one, never read.
        std::fs::write(path("locked.mds"), "Hello\n").unwrap();
        set_mode("locked.mds", 0o000);
        let refused = write("locked.mds");
        set_mode("locked.mds", 0o644);
        assert_eq!(
            refused,
            Err(format!(
                "cannot write {}: {MODULE_REFUSAL}",
                shown("locked.mds").display()
            )),
            "a .mds file is never read to tell"
        );
        assert_eq!(
            std::fs::read_to_string(path("locked.mds")).unwrap(),
            "Hello\n"
        );
    }

    /// A name is judged as Windows opens it (#425): less the dots and spaces it ends in,
    /// which Windows drops from a path's last component, so `lib.md.` and `lib.md ` are
    /// `lib.md` — in bytes and in UTF-16 units alike. Controls: a name that ends in neither
    /// is kept as it is, a dot or space inside it is kept, and a name of nothing but dots
    /// and spaces is kept whole.
    #[test]
    fn a_name_is_judged_without_the_dots_and_spaces_windows_drops() {
        let trimmed = |name: &str| as_windows_opens(name.as_bytes()).to_vec();
        for (name, opens) in [
            ("lib.md.", "lib.md"),
            ("lib.md ", "lib.md"),
            ("lib.md. .", "lib.md"),
            ("lib.mds...", "lib.mds"),
            ("lib.md", "lib.md"),
            ("a. b.md", "a. b.md"),
            ("...", "..."),
            (". ", ". "),
            ("", ""),
        ] {
            assert_eq!(trimmed(name), opens.as_bytes(), "{name:?}");
        }
        let wide: Vec<u16> = "lib.md .".encode_utf16().collect();
        let expected: Vec<u16> = "lib.md".encode_utf16().collect();
        assert_eq!(as_windows_opens(&wide), expected.as_slice(), "UTF-16");
    }

    /// The module check reads an existing output no further than the end of its
    /// frontmatter, at most one read past it (#425): a `.md` file with frontmatter and a
    /// 1 MiB body — whether it declares `type: mds` or not, with the close met inside a
    /// read, across two reads, or after a frontmatter longer than one read — is judged
    /// as mds-core judges it, and the body is left unread. A fence that never closes is
    /// read no further than the cap and is no module. Control: a file with no fence is
    /// read for the fence alone.
    #[test]
    fn the_module_check_reads_no_further_than_the_frontmatter() {
        use std::io::Seek as _;
        const ONE_READ: usize = 8 * 1024;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.md");
        let check = |text: &str| -> (bool, usize) {
            std::fs::write(&path, text).unwrap();
            let mut file = std::fs::File::open(&path).unwrap();
            let module = is_mds_module(&mut file).unwrap();
            let read = usize::try_from(file.stream_position().unwrap()).unwrap();
            (module, read)
        };
        let body = "b\n".repeat(512 * 1024);
        let straddle = format!("---\ntype: mds\n{}", "a".repeat(8181));
        for (label, frontmatter, module) in [
            ("no type", "---\ntitle: t\n---\n".to_owned(), false),
            ("a module", "---\ntype: mds\n---\n".to_owned(), true),
            ("CRLF", "---\r\ntype: mds\r\n---\r\n".to_owned(), true),
            ("across two reads", format!("{straddle}\n---\n"), true),
            (
                "longer than one read",
                format!("---\n{}type: mds\n---\n", "k: v\n".repeat(5000)),
                true,
            ),
        ] {
            let (judged, read) = check(&format!("{frontmatter}{body}"));
            assert_eq!(judged, module, "{label}");
            assert!(
                read <= frontmatter.len() + ONE_READ,
                "{label}: read {read} bytes of a {}-byte frontmatter",
                frontmatter.len()
            );
        }

        let cap = usize::try_from(mds::MAX_FILE_SIZE).unwrap();
        let (judged, read) = check(&format!("---\ntype: mds\n{}", "u".repeat(cap + 1024)));
        assert!(!judged, "a fence that never closes is no module");
        assert!(read <= cap, "read {read} bytes, past the cap");

        let (judged, read) = check(&format!("plain\n{body}"));
        assert!(!judged, "control");
        assert!(read <= 5, "control: read {read} bytes");
    }

    /// On Windows an output's target is judged as the file it opens (#425): `m.md.` and
    /// `m.md ` write `m.md`, a module, and are refused, named as typed, and `lib.mds.` is
    /// the source `lib.mds`, refused too; each is left as it is. Control: `plain.md.` writes
    /// `plain.md`, which is no module.
    #[cfg(windows)]
    #[test]
    fn on_windows_a_name_ending_in_dots_or_spaces_is_judged_as_the_file_it_opens() {
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str| dir.path().join(name);
        let write = |name: &str| {
            write_compiled(
                &WriteTarget::new(path(name), PathBuf::from(name)),
                "X",
                &Inputs::default(),
            )
            .map_err(|e| e.to_string())
        };
        let module = "---\ntype: mds\n---\nM\n";
        std::fs::write(path("m.md"), module).unwrap();
        std::fs::write(path("lib.mds"), "Hello\n").unwrap();
        std::fs::write(path("plain.md"), "notes\n").unwrap();
        for name in ["m.md.", "m.md ", "lib.mds."] {
            assert_eq!(
                write(name),
                Err(format!("cannot write {name}: {MODULE_REFUSAL}")),
                "{name:?}"
            );
        }
        assert_eq!(std::fs::read_to_string(path("m.md")).unwrap(), module);
        assert_eq!(std::fs::read_to_string(path("lib.mds")).unwrap(), "Hello\n");
        assert_eq!(write("plain.md."), Ok(()), "control");
        assert_eq!(std::fs::read_to_string(path("plain.md")).unwrap(), "X");
    }

    /// Each step of a new file's commit puts the file where nothing is, and never over a
    /// file that is there (#160): the rename that never replaces; a hard link, once each
    /// answer saying that rename is not to be had is given; and the content written in
    /// place into a file created exclusively, once each answer saying hard links are not
    /// to be had either is given too. With the name taken, each step refuses, the file
    /// there keeps its bytes, and no temporary file is left. A step that fails otherwise
    /// fails the write, and no later step is taken.
    #[cfg(unix)]
    #[test]
    fn each_step_of_a_new_file_s_commit_never_replaces_a_file() {
        use std::cell::RefCell;
        use std::ffi::OsString;
        use std::io::Write as _;
        use std::os::fd::{AsFd as _, BorrowedFd};

        use rustix::fs::{Mode, OFlags};
        use rustix::io::Errno;

        use super::unix::{commit_new, create_temp, NewSteps, NO_CLOBBER};

        /// What a commit of `new` to `a.mds` came to: its outcome, the steps it took, the
        /// bytes of `a.mds` — `there` when it was taken first — and the names left.
        #[derive(Debug, PartialEq, Eq)]
        struct Outcome {
            result: Result<(), String>,
            steps: Vec<&'static str>,
            file: Option<String>,
            names: Vec<String>,
        }

        // `rename` and `link`: the answer each step gives in place of its own, if any.
        let commit = |rename: Option<Errno>, link: Option<Errno>, taken: bool| {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("a.mds");
            if taken {
                std::fs::write(&file, "there").unwrap();
            }
            let fd = rustix::fs::open(
                dir.path(),
                OFlags::RDONLY | OFlags::DIRECTORY,
                Mode::empty(),
            )
            .unwrap();
            let taken_steps = RefCell::new(Vec::new());
            let forced_rename = |d: BorrowedFd<'_>, from: &OsStr, to: &OsStr| {
                taken_steps.borrow_mut().push("rename");
                rename.map_or_else(|| (NO_CLOBBER.rename)(d, from, to), Err)
            };
            let forced_link = |d: BorrowedFd<'_>, from: &OsStr, to: &OsStr| {
                taken_steps.borrow_mut().push("link");
                link.map_or_else(|| (NO_CLOBBER.link)(d, from, to), Err)
            };
            let steps = NewSteps {
                rename: &forced_rename,
                link: &forced_link,
            };
            let temp_name = OsString::from(format!("{TEMP_PREFIX}step{TEMP_SUFFIX}"));
            let (temp, mut written) =
                create_temp(fd.as_fd(), Mode::from_raw_mode(0o644), [temp_name]).unwrap();
            written.write_all(b"new").unwrap();
            drop(written);
            let result = commit_new(
                temp,
                OsStr::new("a.mds"),
                b"new",
                Durability::RenameOnly,
                &steps,
            )
            .map_err(|failure| match failure {
                Failure::Exists => "exists".to_owned(),
                Failure::Io(e) => format!("{:?}", Errno::from_io_error(&e)),
                other => format!("{other:?}"),
            });
            Outcome {
                result,
                steps: taken_steps.into_inner(),
                file: std::fs::read_to_string(&file).ok(),
                names: entries(dir.path()),
            }
        };
        let landed = |steps: &[&'static str]| Outcome {
            result: Ok(()),
            steps: steps.to_vec(),
            file: Some("new".to_owned()),
            names: vec!["a.mds".to_owned()],
        };
        let refused = |steps: &[&'static str]| Outcome {
            result: Err("exists".to_owned()),
            steps: steps.to_vec(),
            file: Some("there".to_owned()),
            names: vec!["a.mds".to_owned()],
        };

        assert_eq!(commit(None, None, false), landed(&["rename"]), "renamed");
        assert_eq!(
            commit(None, None, true),
            refused(&["rename"]),
            "rename refused"
        );
        for no_rename in [Errno::INVAL, Errno::NOSYS, Errno::NOTSUP, Errno::OPNOTSUPP] {
            let both = ["rename", "link"];
            assert_eq!(
                commit(Some(no_rename), None, false),
                landed(&both),
                "{no_rename:?}: linked"
            );
            assert_eq!(
                commit(Some(no_rename), None, true),
                refused(&both),
                "{no_rename:?}: link refused"
            );
        }
        for no_link in [
            Errno::PERM,
            Errno::NOSYS,
            Errno::NOTSUP,
            Errno::OPNOTSUPP,
            Errno::MLINK,
        ] {
            let both = ["rename", "link"];
            assert_eq!(
                commit(Some(Errno::INVAL), Some(no_link), false),
                landed(&both),
                "{no_link:?}: written in place"
            );
            assert_eq!(
                commit(Some(Errno::INVAL), Some(no_link), true),
                refused(&both),
                "{no_link:?}: written in place refused"
            );
        }

        let failed = |steps: &[&'static str]| Outcome {
            result: Err(format!("{:?}", Some(Errno::IO))),
            steps: steps.to_vec(),
            file: None,
            names: Vec::new(),
        };
        assert_eq!(
            commit(Some(Errno::IO), None, false),
            failed(&["rename"]),
            "a failed rename is no reason to link"
        );
        assert_eq!(
            commit(Some(Errno::INVAL), Some(Errno::IO), false),
            failed(&["rename", "link"]),
            "a failed link is no reason to write in place"
        );
    }

    // ── A removal ────────────────────────────────────────────────────────────────

    /// The file `rel` below the anchor `anchor`, named below `o`.
    fn to_remove(anchor: &Path, rel: &str) -> WriteTarget {
        WriteTarget::below(anchor, Path::new("o"), Path::new(rel))
    }

    /// A file its proof does not accept is kept, and the proof is given the file to be
    /// removed to read; a file its proof cannot read is kept too, as unreadable (#160).
    /// Control: the same file, its proof accepting it, is removed.
    #[test]
    fn a_file_its_proof_does_not_accept_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.json");
        std::fs::write(&file, "stale").unwrap();
        let target = to_remove(dir.path(), "x.json");

        let mut read = String::new();
        let kept = remove_proven(&target, |opened| {
            std::io::Read::read_to_string(opened, &mut read)?;
            Ok(false)
        });
        assert_eq!(kept, Ok(Removal::Kept));
        assert_eq!(read, "stale", "the proof reads the file to be removed");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "stale",
            "it is kept"
        );

        let unreadable = remove_proven(&target, |_| Err(std::io::ErrorKind::InvalidData.into()));
        assert!(
            matches!(unreadable, Err(NotRemoved::Unreadable(_))),
            "{unreadable:?}"
        );
        assert!(file.exists(), "a file its proof cannot read is kept");

        assert_eq!(remove_proven(&target, |_| Ok(true)), Ok(Removal::Removed));
        assert!(!file.exists(), "control: the file proven is removed");
    }

    /// A file that is not there — nor a directory on its way, nor one that is a file — is
    /// nothing to remove, and its proof is never asked (#160). Control: a file that is
    /// there is given to its proof.
    #[test]
    fn a_file_that_is_not_there_is_nothing_to_remove() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file"), "x").unwrap();
        for rel in ["gone.json", "gone/x.json", "file/x.json"] {
            let removal = remove_proven(&to_remove(dir.path(), rel), |_| {
                panic!("{rel}: there is no file to prove")
            });
            assert_eq!(removal, Ok(Removal::Missing), "{rel}");
        }

        std::fs::write(dir.path().join("here.json"), "x").unwrap();
        let mut asked = false;
        let removal = remove_proven(&to_remove(dir.path(), "here.json"), |_| {
            asked = true;
            Ok(false)
        });
        assert_eq!(removal, Ok(Removal::Kept));
        assert!(asked, "control: the proof is asked of a file that is there");
    }

    /// A file whose name is too long for the file system is not there: no file can have
    /// it, so it is nothing to remove and the proof is never asked (#160) — on Windows, a
    /// name std reports as `InvalidFilename` too. Control: a name of the most bytes a
    /// name may have, of a file that is there, is removed.
    #[test]
    fn a_name_too_long_for_the_file_system_is_nothing_to_remove() {
        let dir = tempfile::tempdir().unwrap();
        let too_long = format!("{}.json", "x".repeat(300));
        let removal = remove_proven(&to_remove(dir.path(), &too_long), |_| {
            panic!("there is no file to prove")
        });
        assert_eq!(removal, Ok(Removal::Missing));

        let longest = format!("{}.json", "y".repeat(250));
        std::fs::write(dir.path().join(&longest), "x").unwrap();
        assert_eq!(
            remove_proven(&to_remove(dir.path(), &longest), |_| Ok(true)),
            Ok(Removal::Removed),
            "control"
        );
    }

    /// Only the file's own name is looked past: a directory on its way whose name is too
    /// long for the file system is an error, as any other failure to reach the file is
    /// (#160).
    #[cfg(unix)]
    #[test]
    fn a_directory_name_too_long_on_the_way_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let rel = format!("{}/x.json", "d".repeat(300));
        let removal = remove_proven(&to_remove(dir.path(), &rel), |_| {
            panic!("there is no file to prove")
        });
        assert_eq!(
            removal,
            Err(NotRemoved::Failed(os_text(rustix::io::Errno::NAMETOOLONG)))
        );
    }

    /// A symlink below the anchor is never removed through, and one at the file — live or
    /// dangling — is never removed: each is refused, the directory by the path the user
    /// knows it by, and the file a link leads to is left (#160). Control: the same name
    /// below a real directory is removed.
    #[cfg(unix)]
    #[test]
    fn a_removal_through_a_symlink_or_of_one_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let anchor = dir.path().join("out");
        let victim = dir.path().join("victim");
        std::fs::create_dir_all(anchor.join("real")).unwrap();
        std::fs::create_dir(&victim).unwrap();
        std::fs::write(victim.join("x.json"), "victim").unwrap();
        std::os::unix::fs::symlink(&victim, anchor.join("sub")).unwrap();
        std::os::unix::fs::symlink(victim.join("x.json"), anchor.join("real/live.json")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("nowhere"), anchor.join("real/gone.json"))
            .unwrap();

        assert_eq!(
            remove_proven(&to_remove(&anchor, "sub/x.json"), |_| Ok(true)),
            Err(NotRemoved::Failed(format!("{FOLLOW_REFUSAL} at o/sub")))
        );
        for link in ["real/live.json", "real/gone.json"] {
            let removal = remove_proven(&to_remove(&anchor, link), |_| Ok(true));
            assert_eq!(removal, Err(NotRemoved::Link), "{link}");
            assert_eq!(
                removal.unwrap_err().cause(),
                SYMLINK_REMOVAL_REFUSAL,
                "{link}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(victim.join("x.json")).unwrap(),
            "victim",
            "nothing is removed through a symlink"
        );
        assert_eq!(
            entries(&anchor.join("real")),
            ["gone.json", "live.json"],
            "no symlink is removed"
        );

        std::fs::write(anchor.join("real/x.json"), "stale").unwrap();
        assert_eq!(
            remove_proven(&to_remove(&anchor, "real/x.json"), |_| Ok(true)),
            Ok(Removal::Removed)
        );
        assert_eq!(
            entries(&anchor.join("real")),
            ["gone.json", "live.json"],
            "control: the file is removed"
        );
    }

    /// A FIFO or a directory at the name is no file to remove: refused as not a regular
    /// file, left as it is, and never given to a proof (#160). Control: a regular file
    /// beside them is removed.
    #[cfg(unix)]
    #[test]
    fn anything_but_a_regular_file_is_left() {
        let dir = tempfile::tempdir().unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(dir.path().join("x.json"))
            .status()
            .expect("mkfifo runs");
        assert!(made.success(), "mkfifo x.json");
        std::fs::create_dir(dir.path().join("x.md")).unwrap();
        for name in ["x.json", "x.md"] {
            let removal = remove_proven(&to_remove(dir.path(), name), |_| {
                panic!("{name}: not a file to prove")
            });
            assert_eq!(removal, Err(NotRemoved::NotAFile), "{name}");
        }
        assert_eq!(entries(dir.path()), ["x.json", "x.md"], "both are left");

        std::fs::write(dir.path().join("x.txt"), "stale").unwrap();
        assert_eq!(
            remove_proven(&to_remove(dir.path(), "x.txt"), |_| Ok(true)),
            Ok(Removal::Removed)
        );
        assert_eq!(
            entries(dir.path()),
            ["x.json", "x.md"],
            "control: the file is removed"
        );
    }

    /// A file put at the name after the one there was opened — between its proof and its
    /// removal — is not removed: the removal is refused, and the file put there is left
    /// (#160). Control: with nothing put there, the file proven is removed.
    #[cfg(unix)]
    #[test]
    fn a_file_put_in_the_place_of_the_one_proven_is_not_removed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.json");
        let other = dir.path().join("other.json");
        std::fs::write(&file, "proven").unwrap();
        std::fs::write(&other, "put there").unwrap();
        let target = to_remove(dir.path(), "x.json");

        let swapped = remove_proven(&target, |_| {
            std::fs::rename(&other, &file)?;
            Ok(true)
        });
        assert_eq!(
            swapped,
            Err(NotRemoved::Failed(CHANGED_WHILE_CHECKED.to_owned()))
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "put there",
            "the file put there is left"
        );

        assert_eq!(remove_proven(&target, |_| Ok(true)), Ok(Removal::Removed));
        assert!(!file.exists(), "control: the file proven is removed");
    }

    /// A file edited in place — the same file, written over — after its proof read it is
    /// not removed: the removal is refused, and the edit is left (#160). Control: the same
    /// proof, with no edit, removes the file.
    #[test]
    fn an_edit_made_in_place_after_the_proof_read_it_is_not_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        let target = to_remove(dir.path(), "x.json");
        let read_then = |edit: Option<&'static str>| {
            let path = path.clone();
            move |file: &mut std::fs::File| {
                let mut read = String::new();
                std::io::Read::read_to_string(file, &mut read)?;
                if let Some(edit) = edit {
                    std::fs::write(&path, edit)?;
                }
                Ok(read == "mds bytes")
            }
        };

        std::fs::write(&path, "mds bytes").unwrap();
        let edited = remove_proven(&target, read_then(Some("the user's edit, longer")));
        assert_eq!(
            std::fs::read_to_string(&path).ok().as_deref(),
            Some("the user's edit, longer"),
            "the edit is left"
        );
        assert_eq!(
            edited,
            Err(NotRemoved::Failed(CHANGED_WHILE_CHECKED.to_owned()))
        );

        std::fs::write(&path, "mds bytes").unwrap();
        assert_eq!(
            remove_proven(&target, read_then(None)),
            Ok(Removal::Removed)
        );
        assert!(!path.exists(), "control: the file proven is removed");
    }

    /// A removal whose anchor is not the directory its caller checked is refused before
    /// anything below it is looked at, as a write there is (#160). Control: the directory
    /// checked, opened, has the file removed.
    ///
    /// `#[cfg(unix)]`: two directories made a moment apart are told apart by their inode
    /// there; Windows has their creation times alone, which can be equal.
    #[cfg(unix)]
    #[test]
    fn a_removal_whose_anchor_is_not_the_directory_checked_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let other = dir.path().join("other");
        std::fs::create_dir_all(out.join("sub")).unwrap();
        std::fs::create_dir(&other).unwrap();
        let file = out.join("sub/x.json");
        std::fs::write(&file, "stale").unwrap();
        let identity = |of: &Path| DirIdentity::of(of).expect("a directory");
        let target = to_remove(&out, "sub/x.json");

        let swapped = target.below_checked_anchor(&out, 0, identity(&other));
        assert_eq!(
            remove_proven(&swapped, |_| Ok(true)),
            Err(NotRemoved::out_dir_moved())
        );
        assert!(file.exists(), "nothing is removed below it");

        let checked = target.below_checked_anchor(&out, 0, identity(&out));
        assert_eq!(remove_proven(&checked, |_| Ok(true)), Ok(Removal::Removed));
        assert!(!file.exists(), "control: the file is removed");
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

    /// A replaced file lends its replacement its permission bits alone, never its setuid,
    /// setgid or sticky bit (#160): a `0o4755`, a `0o2755` and a `0o1755` file the user
    /// owns are each replaced by a `0o755` one. Each bit is read back after it is set, so
    /// the file replaced held it. (Writing to a file clears its setuid and setgid bits for
    /// a writer without the privilege to keep them, so for most users only the sticky bit
    /// would otherwise reach the replacement.)
    #[cfg(unix)]
    #[test]
    fn a_replaced_file_lends_only_its_permission_bits() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        for special in [0o4755, 0o2755, 0o1755] {
            let target = dir.path().join(format!("tool-{special:o}.md"));
            let mode = || std::fs::metadata(&target).unwrap().permissions().mode() & 0o7777;
            std::fs::write(&target, "OLD").unwrap();
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(special)).unwrap();
            assert_eq!(mode(), special, "the file replaced holds the bit");

            write_as_typed(&target, "NEW", Durability::RenameOnly).unwrap();

            assert_eq!(std::fs::read_to_string(&target).unwrap(), "NEW");
            assert_eq!(mode(), 0o755, "0{special:o} replaced: got 0{:o}", mode());
        }
    }

    /// A replaced file lends its permission bits only to a file its own owner writes
    /// (#160): from a file another user owns — which only that user chose the mode of —
    /// none, and the replacement keeps the mode a new file is created with. Control: the
    /// same modes, one owner, lend their permission bits and nothing else.
    #[cfg(unix)]
    #[test]
    fn only_a_file_its_writer_owns_lends_its_mode() {
        use rustix::fs::RawMode;

        use super::unix::kept_mode;

        /// `st_mode`'s file type for a regular file, as `stat` gives it above the mode.
        const REGULAR: RawMode = 0o100_000;
        let kept = |st_mode: RawMode, same_owner: bool| {
            kept_mode(st_mode, same_owner).map(|mode| mode.bits())
        };
        assert_eq!(kept(REGULAR | 0o640, true), Some(0o640));
        assert_eq!(kept(REGULAR | 0o7755, true), Some(0o755));
        assert_eq!(kept(REGULAR | 0o666, false), None);
        assert_eq!(kept(REGULAR | 0o4755, false), None);
        assert_eq!(kept(REGULAR | 0o600, false), None);
    }

    /// A file another user owns is replaced by one with the mode a new file is created
    /// with, never that file's (#160): `0o777` lent to the replacement would leave its
    /// bytes open to everyone whatever the writer's umask. Only a writer allowed to give a
    /// file away can set one up, so this runs as root and is skipped, with a reason,
    /// otherwise; [`only_a_file_its_writer_owns_lends_its_mode`] pins the rule everywhere.
    /// Control: a file `std::fs::write` creates beside it has the same mode — never an
    /// execute bit, so never the mode lent.
    #[cfg(unix)]
    #[test]
    fn a_file_another_user_owns_lends_its_replacement_no_mode() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let dir = tempfile::tempdir().unwrap();
        if std::fs::metadata(dir.path()).unwrap().uid() != 0 {
            crate::output::ewriteln!("not running as root; cannot give a file to another user");
            return;
        }
        let target = dir.path().join("shared.md");
        let fresh = dir.path().join("fresh.md");
        std::fs::write(&target, "OLD").unwrap();
        std::os::unix::fs::chown(&target, Some(65_534), None).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o777)).unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(std::fs::metadata(&target).unwrap().uid(), 65_534);
        assert_eq!(mode(&target), 0o777);

        write_as_typed(&target, "NEW", Durability::RenameOnly).unwrap();
        std::fs::write(&fresh, "NEW").unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "NEW");
        assert_eq!(mode(&target), mode(&fresh), "got 0{:o}", mode(&target));
        assert_eq!(temp_residue(dir.path()), Vec::<String>::new());
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
