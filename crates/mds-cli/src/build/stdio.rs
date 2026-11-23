//! Read source text from stdin and write compiled output.

use std::io::Read;

#[cfg(doc)]
use super::config::compile_inputs;
#[cfg(doc)]
use super::out_paths::resolve_output_path_for_kind;
use crate::output::WriteTarget;
use crate::write::{write_compiled, Inputs};
use mds::{MdsError, MAX_FILE_SIZE};
use miette::Result;

/// Read the source from stdin (see [`read_stdin_from`]).
///
/// A stdin source resolves its imports against the working directory. Callers pass
/// `None` as the base directory for that, never the absolute `current_dir()`: core
/// anchors `None` at the working directory itself, and a refusal of it (a forbidden
/// path character, #265) then names it `"."` — the caller typed no path, so no
/// message shows the absolute one.
pub(crate) fn read_stdin() -> Result<String, MdsError> {
    read_stdin_from(&mut std::io::stdin().lock())
}

/// Read a stdin source from `reader`, holding no more than one byte over
/// `MAX_FILE_SIZE` of it and reading no further (#428): [`mds::read_at_most`], as
/// mds-core reads a module file. More than the cap is refused before the bytes are
/// checked as UTF-8; bytes that are not UTF-8 keep the message `read_to_string` gave
/// them.
///
/// # Errors
///
/// More than the cap is `mds::resource_limit` (exit 3), as a file over the same cap is;
/// a read that fails and bytes that are not UTF-8 are `mds::io` (exit 2) (#157).
fn read_stdin_from(reader: &mut impl Read) -> Result<String, MdsError> {
    let bytes = mds::read_at_most(reader, MAX_FILE_SIZE + 1, 0).map_err(|e| MdsError::Io {
        message: format!(
            "cannot read stdin: {}",
            crate::output::safe_inline(crate::output::io_cause(&e))
        ),
    })?;
    if bytes.len() as u64 > MAX_FILE_SIZE {
        return Err(MdsError::ResourceLimit {
            message: "stdin input exceeds maximum size of 10 MiB".to_owned(),
        });
    }
    String::from_utf8(bytes).map_err(|_| MdsError::Io {
        message: "cannot read stdin: stream did not contain valid UTF-8".to_owned(),
    })
}

/// Write compiled output to a file or stdout.
///
/// When `target` is `Some`, write the compiled string to `target.path` and create its parent directories as needed.
/// Print `"Compiled to {target.shown}"` to stderr only if `announce` is true and `quiet` is false (#390).
/// When `target` is `None`, write the compiled string to stdout without an extra newline.
/// A closed stdout is not an error (the reader
/// is gone, so nothing more is written and the run keeps its verdict), any other stdout
/// failure is `mds::io` (#157). This is where a single-file output's directory is
/// created — [`resolve_output_path_for_kind`] creates nothing — so an output refused
/// before the write leaves no directory behind (#425).
///
/// Set `announce = false` in watch-loop rebuilds so only the `"Recompiled …"`
/// summary line is emitted (not a redundant `"Compiled to …"` line).
/// Set `announce = true` for the initial/startup compile and for `mds build`.
///
/// The file write goes through [`write_compiled`] (#227, #160, #425): a crash or write
/// error mid-way never leaves a truncated artifact — the previous output, if any, survives
/// until the rename — the anchor and the directories below it are created as needed, a
/// symlink below the anchor or at the output path is refused, and so are an MDS module
/// and one of `inputs` — the files the run read ([`compile_inputs`]) — at the output path,
/// which are left as they are. The stdout arm writes and flushes the whole output at once
/// ([`crate::output::write_stdout`]).
///
/// Compiled artifacts are written with `Durability::RenameOnly`: they are derived files
/// a rebuild reproduces, and `F_FULLFSYNC` per artifact tripled a 500-template watch
/// startup. Source rewrites (`fmt`, `lint --fix`) keep the fsync.
pub(crate) fn write_output(
    target: Option<&WriteTarget>,
    compiled: &str,
    inputs: &Inputs,
    quiet: bool,
    announce: bool,
) -> Result<()> {
    match target {
        Some(target) => {
            write_compiled(target, compiled, inputs)?;
            if !quiet && announce {
                crate::output::ewriteln!("Compiled to {}", crate::output::safe_path(&target.shown));
            }
        }
        None => crate::output::write_stdout(compiled.as_bytes()).into_batch_result()?,
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::exit_code;
    use super::*;

    /// A stream of `x` bytes — `len` of them, or without end — handed out at most 64 KiB
    /// per read, as a pipe does, counting how many it served.
    pub(crate) struct Stream {
        left: Option<u64>,
        pub(crate) served: u64,
    }

    impl Stream {
        pub(crate) fn sized(len: u64) -> Self {
            Self {
                left: Some(len),
                served: 0,
            }
        }

        fn endless() -> Self {
            Self {
                left: None,
                served: 0,
            }
        }
    }

    impl Read for Stream {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(64 * 1024);
            let n = self
                .left
                .map_or(n, |left| n.min(usize::try_from(left).unwrap_or(n)));
            buf[..n].fill(b'x');
            if let Some(left) = self.left.as_mut() {
                *left -= n as u64;
            }
            self.served += n as u64;
            Ok(n)
        }
    }

    /// #428: stdin is read into a buffer that never holds more than the per-file cap
    /// plus one byte — a buffer only grows, so its final capacity is the most it ever
    /// held — and never past that byte: exactly the cap is accepted, one byte more is
    /// refused, and an endless stream is read to one byte past the cap. #157: more than
    /// the cap is `mds::resource_limit` (exit 3); bytes that are not UTF-8 and a read
    /// that fails are `mds::io` (exit 2).
    #[test]
    fn read_stdin_holds_at_most_the_cap_plus_one_byte() {
        let at_cap = read_stdin_from(&mut Stream::sized(MAX_FILE_SIZE)).unwrap();
        assert_eq!(
            at_cap.len() as u64,
            MAX_FILE_SIZE,
            "exactly the cap is read"
        );
        assert!(
            at_cap.capacity() as u64 <= MAX_FILE_SIZE + 1,
            "capacity {} for {MAX_FILE_SIZE} bytes, over the cap plus one byte",
            at_cap.capacity()
        );

        let over_cap = "resource limit exceeded: stdin input exceeds maximum size of 10 MiB";
        let over: miette::Report = read_stdin_from(&mut Stream::sized(MAX_FILE_SIZE + 1))
            .unwrap_err()
            .into();
        assert_eq!(
            (exit_code(&over), over.to_string()),
            (3, over_cap.to_owned())
        );

        let mut endless = Stream::endless();
        let err: miette::Report = read_stdin_from(&mut endless).unwrap_err().into();
        assert_eq!((exit_code(&err), err.to_string()), (3, over_cap.to_owned()));
        assert_eq!(endless.served, MAX_FILE_SIZE + 1, "read no further");

        let err: miette::Report = read_stdin_from(&mut &b"ok \xff"[..]).unwrap_err().into();
        assert_eq!(
            (exit_code(&err), err.to_string()),
            (
                2,
                "cannot read stdin: stream did not contain valid UTF-8".to_owned()
            )
        );

        let err: miette::Report = read_stdin_from(&mut Unreadable).unwrap_err().into();
        assert_eq!(
            (exit_code(&err), err.to_string()),
            (
                2,
                format!("cannot read stdin: {}", std::io::ErrorKind::Other)
            ),
            "the error's kind, not the text it carries (#390)"
        );
        assert_eq!(read_stdin_from(&mut &b"Hello!\n"[..]).unwrap(), "Hello!\n");
    }

    /// A reader whose every read fails.
    struct Unreadable;

    impl Read for Unreadable {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("stream broke"))
        }
    }
}
