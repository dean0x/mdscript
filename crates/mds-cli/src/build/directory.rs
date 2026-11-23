//! Check directory mode arguments and dispatch the build.

use std::path::{Path, PathBuf};

use miette::Result;

#[cfg(doc)]
use super::config::compile_inputs;
use super::config::{load_config, run_reads, source_inputs};
use super::single::BuildRun;
use super::source_map::{apply_source_map_file_label, embed_carrier, map_path_for};
use super::vars::RuntimeVars;
use super::{run_build_directory, CompileOutput};
use crate::output::{
    collect_mds_files_detailed, output_path_for, probe_and_remove_stale, resolve_output_base,
    stale_output, OutputBase, RootPaths, WalkResult,
};
use crate::write::{write_compiled, write_compiled_and_look, Beside};

pub(super) fn build_directory_mode(
    input: PathBuf,
    resolved: RuntimeVars,
    run: BuildRun,
) -> Result<()> {
    let BuildRun {
        output,
        out_dir,
        quiet,
        flag_source_map,
        no_source_map,
        inline,
        flag_embed_sources,
    } = run;
    // Reject -o/--output in directory mode: output goes to files, not a single destination.
    if output.is_some() {
        return Err(miette::miette!(
            "build directory mode does not support -o/--output; \
             use --out-dir to specify an output directory"
        ));
    }
    // #413: the one directory-argument check every directory-mode subcommand makes
    // (a symlink, the filesystem root, a forbidden character — all `mds::io`).
    crate::input::resolve_directory_argument(&input).map_err(miette::Error::from)?;

    // Load project config to determine effective flags for directory mode.
    let dir_config = load_config(&input)?;
    let cfg_source_map = dir_config
        .as_ref()
        .map(|c| c.config.build.source_map)
        .unwrap_or(false);
    let cfg_embed_sources = dir_config
        .as_ref()
        .map(|c| c.config.build.embed_sources)
        .unwrap_or(false);
    let use_source_map = (flag_source_map || cfg_source_map) && !no_source_map;
    let use_embed_sources = flag_embed_sources || cfg_embed_sources;

    // C3/D6: embed_sources without source_map is a no-op (applies PF-004 — all paths).
    if use_embed_sources && !use_source_map && !quiet {
        crate::output::ewriteln!(
            "warning: embed_sources has no effect without source maps; \
             set build.source_map=true or pass --source-map"
        );
    }

    run_build_directory(
        &input,
        out_dir,
        resolved,
        quiet,
        use_source_map,
        use_embed_sources,
        inline,
    )
}

pub(super) struct DirectoryWalk {
    pub(super) walk: WalkResult,
    pub(super) canonical_dir: PathBuf,
    pub(super) output_base: OutputBase,
    reads: Vec<PathBuf>,
}

/// Set up the directory walk and check that it has work.
///
/// **Nothing to build is an error (#204, #387):** when the walk yields no files to
/// compile the run exits 1 with a one-line stderr diagnostic that bypasses `--quiet`.
/// Three shapes, in the order they are checked: the all-excluded count diagnostic
/// (every candidate sits under a default-excluded directory), `no .mds files found
/// in <dir>; nothing was built` (a genuinely empty tree), and the partials-only
/// count diagnostic (#387 — the tree is non-empty but every candidate is a
/// `_`-prefixed partial). All three exit through [`crate::output::exit`]: no `MdsError`
/// variant exists for "nothing to do" and `exit_code` must not grow one for a
/// non-error class.
/// `mds check` mirrors all three with exit 1; `mds fmt` (exit 1) and `mds lint`
/// (exit 2) mirror only the first two — they iterate every file including partials,
/// so a partials-only tree is real work for them, not "nothing to do".
/// `mds watch <dir>` deliberately does NOT error on any of the three.
///
/// Per-file source_map_base: the output directory for this file, computed
/// from the kind-independent directory oracle, without creating it — an early
/// create_dir_all would leave an empty directory on compile failure (Step 6
/// Caveat 1). A walked file's output stem always has a parent, the directory it
/// lands in, so the base is always that directory.
/// Compile (all reads go through mds-core which enforces MAX_FILE_SIZE — PF-004).
/// A panic in it fails this file alone, and the batch goes on (#389).
pub(super) fn setup_walk(
    dir: &Path,
    out_dir: Option<&PathBuf>,
    vars_file: Option<&Path>,
) -> Result<DirectoryWalk> {
    const MAX_DEPTH: usize = 64;

    // Load project config from the directory root.
    let config = load_config(dir)?;
    let reads = run_reads(vars_file, config.as_ref());
    // The out-dir's canonical form keeps the starts_with checks reliable; its shown form
    // is the out-dir as typed (#390).
    let output_base = resolve_output_base(out_dir, &config)?;

    // Keep the canonical directory for output exclusion and the walk-time entry pins.
    let canonical_dir =
        crate::input::resolve_directory_argument(dir).map_err(miette::Error::from)?;

    // Exclude the out-dir from collection when it is nested inside the source root.
    let exclude_prefix: Option<PathBuf> = match &output_base {
        OutputBase::Dir { canonical: d, .. } if d.starts_with(&canonical_dir) => Some(d.clone()),
        _ => None,
    };

    let walk = collect_mds_files_detailed(dir, MAX_DEPTH, exclude_prefix.as_deref());
    let files = &walk.files;

    if files.is_empty() {
        if walk.excluded_by_default > 0 {
            // All candidates were inside default-excluded directories.  Emit the
            // diagnostic even under --quiet (avoids a silent CI green pass — avoids
            // PF-004 enforcement gap where the limit is real on one path and absent
            // on another).
            crate::output::ewriteln!(
                "{} .mds file(s) found but all are under default-excluded directories \
                 (hidden dirs, node_modules); nothing was built",
                walk.excluded_by_default
            );
            crate::output::exit(1);
        }
        // #204: an empty tree is "nothing to build", not success.  Same shape as the
        // all-excluded arm above — emitted even under --quiet (a silent green pass on
        // a mistyped or not-yet-populated directory is the CI failure mode this
        // closes) and exit 1, the build/check/fmt "nothing was done" code (spec §7.9).
        // `mds watch <dir>` deliberately still starts on an empty tree: a file created
        // later is a valid flow there.
        crate::output::ewriteln!(
            "no .mds files found in {}; nothing was built",
            crate::output::safe_path(dir)
        );
        crate::output::exit(1);
    }

    // #387: a tree whose only .mds files are partials is "nothing to build" too. The
    // walker collects partials (watch/fmt/lint need them) but this loop skips them, so
    // without this arm the run ends `0 built, 0 failed`, exit 0 — the silent green pass
    // #204 closed for the empty tree. Same shape as the all-excluded arm: count-carrying,
    // emitted even under --quiet, exit 1. fmt and lint operate on partials and keep their
    // behaviour; `mds watch <dir>` still starts.
    if let Some(partials_only_count) = crate::output::partials_only(files) {
        crate::output::ewriteln!(
            "{partials_only_count} .mds file(s) found in {} but all are _-prefixed partials; \
             nothing was built",
            crate::output::safe_path(dir)
        );
        crate::output::exit(1);
    }
    Ok(DirectoryWalk {
        walk,
        canonical_dir,
        output_base,
        reads,
    })
}

pub(super) struct WriteFlags {
    pub(super) quiet: bool,
    pub(super) source_map: bool,
    pub(super) inline: bool,
}

pub(super) enum WriteOutcome {
    Built,
    Failed,
}

/// Write a successful directory compile and report its outcome.
///
/// **I/O failures (#157):** an output directory that cannot be created, an output or
/// `.map` sidecar that cannot be written, and a stale sibling that cannot be read or
/// whose proven removal fails are each reported as one `mds::io` error through
/// [`crate::output::eprint_io_failure`], which records it for the exit code, so the run
/// exits 2 while the other files are still built. A compile that fails is reported
/// through [`crate::output::eprint_file_failure`], which records it the same way when it
/// is an I/O or file-system failure — a source that cannot be read, say — and as a
/// resource limit when it is one, so the run exits 3. All but the stale sibling count
/// their file as failed; its file was built. A run whose failures are only template
/// errors exits 1.
///
/// Subtree mirroring: with `--out-dir`, mirrors the source subtree into the out-dir
/// with the intrinsic extension per file (AC-FUNC-16). Without `--out-dir`, each
/// output is placed next to its source (AC-FUNC-19).
///
/// Stale-output cleanup: after writing an output, looks at the other kind's output of
/// the same name, left when the source compiled to that kind, and removes it only when
/// mds provably wrote it — a stale `.md` never — keeping anything else with a warning
/// ([`crate::output::probe_and_remove_stale`], #160).
///
/// No output is written over a file the run reads (#425): the source it was compiled
/// from, the modules that compile imported, the `--vars` file `vars` came from, the
/// `mds.json` in force ([`compile_inputs`]) and the `mds.json` nearest that source
/// ([`source_inputs`]).
///
/// Set the map's `file` field from the output path. Core puts the sources in relative form.
/// In inline mode, embed the map in the output. Otherwise, write a separate map if required.
///
/// R5: Check if the final content is empty before the write. Count it only if the output write
/// succeeds. If the sidecar write fails, keep the empty count but skip stale cleanup and report failure.
///
/// The directory write has the same atomic contract as `write_output` (#227).
/// `tests/write_funnel.rs` checks both call sites. The write creates the out-dir and its mirrored
/// directories. It refuses a symlink among them (#160) or an MDS module at the output path (#425).
/// Report write errors as given. The message already names the path.
///
/// Check for stale output only below an out-dir. Next to the source, that path can hold a file
/// the user wrote or an output from a different source. For example, `a.b.mds` and `a.mds` both
/// name their output `a.md` or `a.json` there. Skip cleanup when the write found nothing to remove.
/// If removal fails, count the file as built but record the I/O error for the exit code (#157).
pub(super) fn write_file(
    mut compiled: CompileOutput,
    file: &Path,
    dir: &Path,
    setup: &DirectoryWalk,
    flags: &WriteFlags,
    empty_count: &mut usize,
) -> WriteOutcome {
    let WriteFlags {
        quiet,
        source_map,
        inline,
    } = *flags;
    let output_base = &setup.output_base;
    let ext = compiled.kind.extension();
    let inputs = source_inputs(file, &compiled.dependencies, &setup.reads);
    let target = output_path_for(file, RootPaths::as_typed(dir), output_base, ext);

    if let Some(ref mut sm) = compiled.source_map {
        apply_source_map_file_label(sm, Some(&target.path), false);
    }

    let final_content = if source_map && inline {
        if let Some(ref sm) = compiled.source_map {
            embed_carrier(compiled.content, &sm.to_json())
        } else {
            compiled.content
        }
    } else {
        compiled.content
    };

    let wrote_empty = final_content.is_empty();

    let stale =
        matches!(output_base, OutputBase::Dir { .. }).then(|| stale_output(&target, compiled.kind));
    let written = match &stale {
        Some(stale) => write_compiled_and_look(&target, &final_content, &inputs, stale).map(Some),
        None => write_compiled(&target, &final_content, &inputs).map(|()| None),
    };
    match written {
        Ok(beside) => {
            if wrote_empty {
                *empty_count += 1;
            }
            if !quiet {
                crate::output::ewriteln!("Compiled to {}", crate::output::safe_path(&target.shown));
            }

            if source_map && !inline {
                if let Some(ref sm) = compiled.source_map {
                    let map = target.sibling(map_path_for);
                    let map_json = sm.to_json();
                    if let Err(e) = write_compiled(&map, &map_json, &inputs) {
                        crate::output::eprint_io_failure(e);
                        return WriteOutcome::Failed;
                    }
                    if !quiet {
                        crate::output::ewriteln!(
                            "Source map written to {}",
                            crate::output::safe_path(&map.shown)
                        );
                    }
                }
            }

            if beside == Some(Beside::Something) {
                if let Err(e) = probe_and_remove_stale(&target, compiled.kind, quiet) {
                    crate::output::eprint_io_failure(e);
                }
            }
            WriteOutcome::Built
        }
        Err(e) => {
            crate::output::eprint_io_failure(e);
            WriteOutcome::Failed
        }
    }
}
