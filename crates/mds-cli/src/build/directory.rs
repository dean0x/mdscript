//! Check directory mode arguments and dispatch the build.

use std::path::PathBuf;

use miette::Result;

use super::config::load_config;
use super::run_build_directory;
use super::single::BuildRun;
use super::vars::RuntimeVars;

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
