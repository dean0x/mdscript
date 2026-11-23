//! Source map flags and their build warnings.

use super::config::ProjectConfig;

pub(super) struct BuildFlags {
    pub(super) use_source_map: bool,
    pub(super) use_embed_sources: bool,
}

#[must_use]
pub(super) fn effective_build_flags(
    config: Option<&ProjectConfig>,
    flag_source_map: bool,
    no_source_map: bool,
    flag_embed_sources: bool,
) -> BuildFlags {
    let cfg_source_map = config.is_some_and(|c| c.config.build.source_map);
    let cfg_embed_sources = config.is_some_and(|c| c.config.build.embed_sources);
    BuildFlags {
        use_source_map: (flag_source_map || cfg_source_map) && !no_source_map,
        use_embed_sources: flag_embed_sources || cfg_embed_sources,
    }
}

pub(super) fn warn_build_flags(flags: &BuildFlags, warn_inline: bool, quiet: bool) {
    let BuildFlags {
        use_source_map,
        use_embed_sources,
    } = *flags;

    if use_embed_sources && !use_source_map && !quiet {
        crate::output::ewriteln!(
            "warning: embed_sources has no effect without source maps; \
             set build.source_map=true or pass --source-map"
        );
    }

    // AC-SEC-02: warn when the output includes full source text.
    if use_embed_sources && warn_inline && !quiet {
        crate::output::ewriteln!(
            "warning: --embed-sources with --inline ships full source text in the output \
             (AC-SEC-02)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warns_about_inline_embed_even_without_source_maps() {
        const CHILD: &str = "MDS_TEST_BUILD_FLAG_WARNINGS_CHILD";
        const NAME: &str = "build::flags::tests::warns_about_inline_embed_even_without_source_maps";
        if std::env::var(CHILD).as_deref() == Ok(NAME) {
            let flags = BuildFlags {
                use_source_map: false,
                use_embed_sources: true,
            };
            warn_build_flags(&flags, true, false);
            return;
        }

        let binary = std::env::current_exe().expect("the test binary's path");
        let child = std::process::Command::new(binary)
            .args(["--exact", NAME])
            .env(CHILD, NAME)
            .output()
            .expect("the child runs");
        assert!(child.status.success());
        assert_eq!(
            String::from_utf8_lossy(&child.stderr),
            concat!(
                "warning: embed_sources has no effect without source maps; ",
                "set build.source_map=true or pass --source-map\n",
                "warning: --embed-sources with --inline ships full source text in the output ",
                "(AC-SEC-02)\n",
            ),
        );
    }
}
