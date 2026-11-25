//! Build one file or read a source from stdin.

use std::path::{Path, PathBuf};

use miette::Result;

use super::admit::{admit_output, EntryPaths};
use super::config::{compile_inputs, load_config, run_reads, ProjectConfig};
use super::flags::{effective_build_flags, warn_build_flags};
use super::net::{compile_source, NetFlags, NetRun};
use super::out_paths::{resolve_output_path_for_kind, warn_output_extension_mismatch, OutputKind};
use super::source_map::{
    apply_source_map_file_label, compute_source_map_base, embed_map_in_content, file_name_of,
    map_path_for, verify_then_delete_map, write_sidecar_map, write_stdout_ladder,
};
use super::stdio::{read_stdin, write_output};
use super::vars::RuntimeVars;
use super::{compile_to_content, serialize_output};
use crate::output::WriteTarget;
use crate::write::Inputs;

pub(super) struct BuildRun {
    pub(super) output: Option<String>,
    pub(super) out_dir: Option<PathBuf>,
    pub(super) quiet: bool,
    pub(super) flag_source_map: bool,
    pub(super) no_source_map: bool,
    pub(super) inline: bool,
    pub(super) flag_embed_sources: bool,
    pub(super) net: NetFlags,
}

// ── Stdin path ───────────────────────────────────────────────────────────────

pub(super) fn run_build_stdin(input: PathBuf, vars: RuntimeVars, run: &BuildRun) -> Result<()> {
    let BuildRun {
        ref output,
        ref out_dir,
        quiet,
        flag_source_map,
        no_source_map,
        inline,
        flag_embed_sources,
        ref net,
    } = *run;
    let flags = effective_build_flags(None, flag_source_map, no_source_map, flag_embed_sources);
    warn_build_flags(&flags, inline, quiet);

    let source_map_base = flags
        .use_source_map
        .then(|| compute_source_map_base(Path::new("-"), output, out_dir, &None))
        .transpose()?;
    let opts = mds::CompileOptions::default()
        .with_source_map(flags.use_source_map)
        .with_include_sources_content(flags.use_embed_sources)
        .with_source_map_base(source_map_base);

    // Stdin has no project config, so only the flags grant the network.
    let net = NetRun::start(net, None)?;
    let source = read_stdin()?;
    // AD-211-1 / AD-211-5: same stdin relabel as `compile_to_content`; `None` is the
    // working directory, shown as "." (see `read_stdin`).
    let result = compile_source(&source, vars.vars, opts, net.as_ref())
        .map_err(|e| crate::output::relabel_stdin_error(&e, &source))?;
    if !quiet {
        for w in &result.warnings {
            crate::output::eprint_warning(w);
        }
    }

    let source_map = result.source_map;
    let kind = OutputKind::from(&result.output);
    let content = serialize_output(result.output)?;
    let inputs = compile_inputs(
        None,
        &result.dependencies,
        &run_reads(vars.vars_file.as_deref(), None),
    );

    // Stdin: no project config; output path follows -o flag or defaults to stdout.
    // There is no entry file to refuse (#425), so the `-o` warning is printed now.
    let output_path = resolve_output_path_for_kind(
        Some(EntryPaths::as_typed(&input)),
        output,
        out_dir,
        &None,
        kind,
    )?;
    warn_output_extension_mismatch(output, kind, quiet);
    finish_stdin_output(
        run,
        flags.use_source_map,
        source_map,
        content,
        output_path,
        inputs,
    )
}

fn finish_stdin_output(
    run: &BuildRun,
    use_source_map: bool,
    mut source_map: Option<mds::SourceMap>,
    content: String,
    output_path: Option<WriteTarget>,
    inputs: Inputs,
) -> Result<()> {
    let BuildRun { quiet, inline, .. } = *run;
    if let Some(ref mut sm) = source_map {
        // Set `file` field and relabel source entry for stdin builds (AC-FUNC-12).
        apply_source_map_file_label(sm, output_path.as_ref().map(|t| t.path.as_path()), true);
    }

    if !use_source_map {
        return write_output(output_path.as_ref(), &content, &inputs, quiet, true);
    }
    if inline {
        let final_content = embed_map_in_content(content, source_map.as_ref());
        return write_output(output_path.as_ref(), &final_content, &inputs, quiet, true);
    }

    let Some(out) = &output_path else {
        // Stdin has no config. Only the explicit flag can request a map.
        return write_stdout_ladder(source_map.is_some(), true, None, content, &inputs, quiet);
    };
    // Write the output first, with the same bytes as a build without a map.
    write_output(Some(out), &content, &inputs, quiet, true)?;
    if let Some(ref sm) = source_map {
        write_sidecar_map(out, sm, &inputs, quiet)?;
    }
    Ok(())
}

// ── File input path ──────────────────────────────────────────────────────────

pub(super) fn run_build_file(input: PathBuf, vars: RuntimeVars, run: &BuildRun) -> Result<()> {
    let BuildRun {
        ref output,
        ref out_dir,
        quiet,
        flag_source_map,
        no_source_map,
        inline,
        flag_embed_sources,
        ref net,
    } = *run;
    let config = load_config(&input)?;
    let flags = effective_build_flags(
        config.as_ref(),
        flag_source_map,
        no_source_map,
        flag_embed_sources,
    );
    warn_build_flags(&flags, inline, quiet);

    let source_map_base = flags
        .use_source_map
        .then(|| compute_source_map_base(&input, output, out_dir, &config))
        .transpose()?;
    let opts = mds::CompileOptions::default()
        .with_source_map(flags.use_source_map)
        .with_include_sources_content(flags.use_embed_sources)
        .with_source_map_base(source_map_base);

    let net = NetRun::start(net, config.as_ref())?;
    let compiled = compile_to_content(&input, vars.vars, quiet, opts, net.as_ref())?;
    let inputs = compile_inputs(
        Some(&input),
        &compiled.dependencies,
        &run_reads(vars.vars_file.as_deref(), config.as_ref()),
    );
    // `mds build` holds only the typed path: the output is resolved from it, and
    // `admit_output` canonicalizes both sides itself.
    let entry = EntryPaths::as_typed(&input);
    let output_path =
        resolve_output_path_for_kind(Some(entry), output, out_dir, &config, compiled.kind)?;
    // #425: nothing — output or sidecar map — is written once the output is the entry,
    // and no warning announces that write.
    let written_path = output_path.as_ref().map(|t| t.path.as_path());
    admit_output(written_path, entry, output, compiled.kind, quiet).map_err(miette::Error::from)?;

    let mut source_map = compiled.source_map;
    if let Some(ref mut sm) = source_map {
        apply_source_map_file_label(sm, written_path, false);
    }

    write_file_output(
        run,
        flags.use_source_map,
        config.as_ref(),
        output_path,
        inputs,
        compiled.content,
        source_map,
    )
}

fn write_file_output(
    run: &BuildRun,
    use_source_map: bool,
    config: Option<&ProjectConfig>,
    output_path: Option<WriteTarget>,
    inputs: Inputs,
    content: String,
    source_map: Option<mds::SourceMap>,
) -> Result<()> {
    let BuildRun {
        quiet,
        flag_source_map,
        inline,
        ..
    } = *run;
    if use_source_map {
        if inline {
            // Inline: embed carrier into the output, then write (ADR-002 not applicable:
            // inline mode intentionally modifies the output file).
            let final_content = embed_map_in_content(content, source_map.as_ref());
            write_output(output_path.as_ref(), &final_content, &inputs, quiet, true)?;

            // Stale sidecar reconciliation: if there is an existing .map file from
            // a prior sidecar build, remove it (AC-FUNC-10).
            if let Some(ref out) = output_path {
                verify_then_delete_map(&out.sibling(map_path_for), &file_name_of(out), quiet)?;
            }
        } else {
            let Some(out) = &output_path else {
                return write_stdout_ladder(
                    source_map.is_some(),
                    flag_source_map,
                    config,
                    content,
                    &inputs,
                    quiet,
                );
            };
            // Write the output first, with the same bytes as a build without a map.
            write_output(Some(out), &content, &inputs, quiet, true)?;
            if let Some(ref sm) = source_map {
                write_sidecar_map(out, sm, &inputs, quiet)?;
            }
        }
    } else {
        write_output(output_path.as_ref(), &content, &inputs, quiet, true)?;

        // No-source-map build: if a stale sidecar exists from a prior source-map build,
        // remove it (AC-FUNC-10).
        if let Some(ref out) = output_path {
            verify_then_delete_map(&out.sibling(map_path_for), &file_name_of(out), quiet)?;
        }
    }

    Ok(())
}
