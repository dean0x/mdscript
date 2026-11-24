//! Module processing, output evaluation, and source-map finalization.

use super::{
    build_scope_from_frontmatter, bytes, evaluate_messages_seeded, evaluate_seeded,
    evaluate_with_map_seeded, fragment_map_from, has_message_block, is_visible_export,
    parse_frontmatter_mapping, seed_effective_blocks, spliced_regions, validate_exports, validator,
    Arc, CollectedDefs, CompileOptions, ExtendsComponents, MapBuilder, MdsError, MemBudget, Module,
    ModuleCache, ModuleCtx, Node, Origin, Part, PromptText, ResolvedModule, Scope,
    MAX_SOURCES_CONTENT_BYTES,
};
use crate::MarkdownFrontmatter;

/// Warning emitted when `source_map: true` is used with a messages-mode template.
///
/// Source maps operate on a flat text stream; messages-mode boundaries
/// (`@message` blocks) are not representable in the SMv3 segment model.
/// The warning is surface-neutral (no mention of specific option names or APIs).
const MSG_MODE_SOURCE_MAP_WARNING: &str =
    "source maps are not supported for messages-mode templates (@message blocks); \
     no source map will be generated";

type StandalonePrompt<'ctx> = (
    Option<PromptText>,
    Option<Arc<crate::sourcemap::FragmentMap>>,
    &'ctx Origin,
);

impl ModuleCache {
    /// Common intrinsic processing: tokenize, parse, build scope, then dispatch on
    /// output shape.
    ///
    /// Shares setup with `process_module` but dispatches on `has_message_block` at the
    /// end: a body containing any `@message` block evaluates to
    /// [`crate::CompiledOutput::Messages`] (via `evaluate_messages_seeded`), otherwise
    /// to [`crate::CompiledOutput::Markdown`] (via `evaluate_seeded` or mapped evaluation,
    /// then `finalize_markdown`).
    ///
    /// When the parsed module has an `@extends` directive the shared extends pipeline
    /// (`resolve_extends_components`) builds the effective blocks and `scope`
    /// identically to text mode — then the dispatch is performed on the spliced
    /// regions (NOT `module.body`), so @message blocks inside base @block defaults are
    /// correctly detected (avoids PF-004 divergence, decision #8).
    pub(super) fn process_module_intrinsic(
        &mut self,
        ctx: &ModuleCtx<'_>,
        is_md: bool,
        warnings: &mut Vec<String>,
    ) -> Result<crate::CompiledOutput, MdsError> {
        self.process_module_intrinsic_opts(ctx, is_md, &Default::default(), warnings)
            .map(|(output, _)| output)
    }

    /// Opts-bearing variant of `process_module_intrinsic`.
    ///
    /// Returns `(CompiledOutput, Option<SourceMap>)`.  The source map is
    /// populated when `opts.source_map` is `true`; `None` otherwise.
    ///
    /// For Markdown output the source map is built by threading a
    /// [`crate::sourcemap::MapBuilder`] through the evaluator:
    ///
    /// - Standalone path: [`evaluate_with_map_seeded`] on `module.body`.
    /// - `@extends` path: [`Self::evaluate_extends_regions`] over
    ///   `spliced_regions(skeleton, effective_blocks, skeleton_origin)` so each
    ///   region's segments are attributed to the correct source file.
    ///
    /// Messages templates do not generate a source map (the source map covers
    /// compiled Markdown only; messages mode is not a single contiguous string).
    pub(super) fn process_module_intrinsic_opts(
        &mut self,
        ctx: &ModuleCtx<'_>,
        is_md: bool,
        opts: &crate::sourcemap::CompileOptions,
        warnings: &mut Vec<String>,
    ) -> Result<(crate::CompiledOutput, Option<crate::SourceMap>), MdsError> {
        // Gate sub-module FragmentMap construction on whether source maps are
        // requested for this compilation.  Must be set before any import
        // resolution (collect_definitions_and_imports / resolve_frontmatter_imports)
        // so that transitively-imported modules also build their FragmentMaps.
        // Zero-cost path: when opts.source_map=false this remains false and no
        // MapBuilder is allocated for sub-modules (AC-PERF-01).
        self.source_map_mode = opts.source_map;

        // The entry holds its tree charge as a temporary until the module closes.
        let module = self.parse_charged(ctx, false)?;

        let raw_frontmatter = module.frontmatter.as_ref().map(|fm| fm.raw.as_str());

        // ── Extends branch ────────────────────────────────────────────────────
        if let Some(ext) = module.extends.clone() {
            return self.intrinsic_extends(&module, &ext, ctx, opts, warnings);
        }

        self.intrinsic_standalone(&module, raw_frontmatter, ctx, is_md, opts, warnings)
    }

    fn intrinsic_extends(
        &mut self,
        module: &Module,
        ext: &crate::ast::ExtendsDirective,
        ctx: &ModuleCtx<'_>,
        opts: &crate::sourcemap::CompileOptions,
        warnings: &mut Vec<String>,
    ) -> Result<(crate::CompiledOutput, Option<crate::SourceMap>), MdsError> {
        let (frontmatter_values, frontmatter_bytes) =
            parse_frontmatter_mapping(module.frontmatter.as_ref(), &mut self.budget.mem)?;
        let components = self.resolve_extends_components(
            module,
            ext,
            ctx,
            &frontmatter_values,
            frontmatter_bytes,
            warnings,
        )?;

        {
            let mut scope = components.scope.clone();
            Self::validate_extends_components(&components, &mut scope)?;
        }

        let ExtendsComponents {
            mut scope,
            merged_frontmatter,
            effective_skeleton,
            effective_blocks,
            skeleton_origin,
            ..
        } = components;

        // Per-region evaluation in both modes, with or without a source map:
        // base-skeleton nodes, base defaults and child overrides index into
        // different files, so each region is evaluated against its own origin
        // (#114, #115).
        let regions = spliced_regions(&effective_skeleton, &effective_blocks, &skeleton_origin);

        if regions.iter().any(|(nodes, _)| has_message_block(nodes)) {
            // AC-FUNC-07: source_map=true is incompatible with messages-mode templates.
            // The evaluator only has text-stream semantics; messages boundaries don't
            // have stable byte offsets relative to the source.  Degrade gracefully.
            if opts.source_map {
                warnings.push(MSG_MODE_SOURCE_MAP_WARNING.to_string());
            }
            let messages = Self::evaluate_message_regions(
                &regions,
                &mut scope,
                warnings,
                self.budget.start_module(),
            )?;
            return Ok((
                crate::CompiledOutput::Messages(
                    messages.into_iter().map(crate::Message::from).collect(),
                ),
                None, // source maps cover Markdown only (AC-FUNC-07)
            ));
        }

        // Seed the builder with the skeleton's root file (source maps only).
        let seed = opts.source_map.then_some(&skeleton_origin);
        let (body_raw, builder) = Self::evaluate_extends_regions(
            &regions,
            seed,
            &mut scope,
            warnings,
            self.budget.start_module(),
        )?;
        self.finalize_markdown(
            body_raw,
            merged_frontmatter
                .as_deref()
                .map(MarkdownFrontmatter::Merged),
            builder.map(|b| (b, 0)),
            ctx.base_dir,
            opts,
            warnings,
        )
    }

    // ── Standalone path ───────────────────────────────────────────────────
    fn intrinsic_standalone(
        &mut self,
        module: &Module,
        raw_frontmatter: Option<&str>,
        ctx: &ModuleCtx<'_>,
        is_md: bool,
        opts: &crate::sourcemap::CompileOptions,
        warnings: &mut Vec<String>,
    ) -> Result<(crate::CompiledOutput, Option<crate::SourceMap>), MdsError> {
        // The parsed tree is dropped once the scope layer is built from it.
        let (mut scope, fm_imports) = {
            let (mapping, spent) =
                parse_frontmatter_mapping(module.frontmatter.as_ref(), &mut self.budget.mem)?;
            build_scope_from_frontmatter(
                module.frontmatter.as_ref(),
                mapping.as_ref(),
                spent,
                is_md,
                ctx.runtime_vars,
                &mut self.budget.mem,
            )?
        };

        self.resolve_frontmatter_imports(&fm_imports, &mut scope, ctx, warnings)?;

        let CollectedDefs {
            functions,
            explicit_exports,
            ..
        } = self.collect_definitions_and_imports(&module.body, &mut scope, ctx, warnings)?;

        validate_exports(&explicit_exports, &functions)?;

        validator::validate(&module.body, &mut scope, ctx.file_str, ctx.source)?;

        if has_message_block(&module.body) {
            // AC-FUNC-07: source_map=true is incompatible with messages-mode templates.
            if opts.source_map {
                warnings.push(MSG_MODE_SOURCE_MAP_WARNING.to_string());
            }
            let mut messages = Vec::new();
            evaluate_messages_seeded(
                &module.body,
                &mut scope,
                warnings,
                ctx.file_str,
                ctx.source,
                self.budget.start_module(),
                &mut messages,
            )?;
            return Ok((
                crate::CompiledOutput::Messages(
                    messages.into_iter().map(crate::Message::from).collect(),
                ),
                None, // source maps cover Markdown only (AC-FUNC-07)
            ));
        }

        self.eval_standalone_markdown(
            &module.body,
            &mut scope,
            raw_frontmatter,
            ctx,
            opts,
            warnings,
        )
    }

    fn eval_standalone_markdown(
        &mut self,
        body: &[Node],
        scope: &mut Scope,
        raw_frontmatter: Option<&str>,
        ctx: &ModuleCtx<'_>,
        opts: &crate::sourcemap::CompileOptions,
        warnings: &mut Vec<String>,
    ) -> Result<(crate::CompiledOutput, Option<crate::SourceMap>), MdsError> {
        let mut body_raw = String::new();
        let builder = if opts.source_map {
            // The builder's seed (current source 0) is ctx.key / ctx.file_str / ctx.source;
            // evaluate_with_map derives display/source from builder (issue #58 / R3).
            let (origin, map_origin_bytes) = match ctx.origin.get() {
                Some(origin) => (origin.clone(), 0),
                // A map-only origin is part of optional map work. It is at most one
                // capped source copy; charge it with the live segments at finalize.
                None => (ctx.make_origin(), ctx.origin_bytes()),
            };
            let builder = MapBuilder::new(origin);
            let returned = evaluate_with_map_seeded(
                body,
                scope,
                warnings,
                builder,
                self.budget.start_module(),
                &mut body_raw,
            )?;
            Some((returned, map_origin_bytes))
        } else {
            evaluate_seeded(
                body,
                scope,
                warnings,
                ctx.file_str,
                ctx.source,
                self.budget.start_module(),
                &mut body_raw,
            )?;
            None
        };

        self.finalize_markdown(
            body_raw,
            raw_frontmatter.map(MarkdownFrontmatter::Standalone),
            builder,
            ctx.base_dir,
            opts,
            warnings,
        )
    }

    /// Add the frontmatter prefix to the Markdown body.
    ///
    /// For a standalone module, use the same raw text and stripper as the parse-time proof.
    /// For `@extends`, use the checked serializer output directly.
    fn finalize_markdown(
        &mut self,
        body_raw: String,
        frontmatter: Option<MarkdownFrontmatter<'_>>,
        builder: Option<(MapBuilder, usize)>,
        base_dir: &str,
        opts: &CompileOptions,
        warnings: &mut Vec<String>,
    ) -> Result<(crate::CompiledOutput, Option<crate::SourceMap>), MdsError> {
        let mem = &mut self.budget.mem;
        let mark = mem.mark();
        let prefix = if let Some(frontmatter) = frontmatter {
            mem.charge(
                Part::Temp,
                bytes(frontmatter.text().len(), 2).and_then(|n| n.checked_add(9)),
            )?;
            crate::frontmatter_prefix(frontmatter)
        } else {
            None
        };
        let fm_prefix_len = prefix.as_ref().map_or(0, String::len);
        // Reserve the output's charge before optional map work can use headroom.
        // The empty body takes the prefix as-is, without a final newline reserve.
        let growth = fm_prefix_len + usize::from(!body_raw.trim_end().is_empty());
        mem.charge(Part::Buffers, Some(growth))?;
        let charged_capacity = body_raw.capacity() + growth;

        let pending_map = builder.and_then(|(mut builder, map_origin_bytes)| {
            let total_src_bytes = builder.sources_content_bytes();
            let label_base_bytes = opts
                .source_map_base
                .as_ref()
                .map_or(0, |base| base.as_os_str().len());
            let prefix_lines = prefix.as_deref().map_or(0, count_newlines);
            if !builder.segments_dropped
                && !charge_finalize(
                    mem,
                    &builder,
                    &body_raw,
                    FinalizeCosts {
                        content: if opts.include_sources_content
                            && total_src_bytes <= MAX_SOURCES_CONTENT_BYTES
                        {
                            total_src_bytes
                        } else {
                            0
                        },
                        map_origin_bytes,
                        prefix_lines,
                        label_base_bytes,
                    },
                )
            {
                builder.segments_dropped = true;
            }
            apply_map_degradation(builder, total_src_bytes, opts, warnings)
                .map(|b| b.finalize_points(&body_raw))
        });
        let mut body = crate::clean_output_owned(body_raw, fm_prefix_len);
        if let Some(prefix) = prefix {
            if body.is_empty() {
                body = prefix;
            } else {
                body.insert_str(0, &prefix);
            }
        }
        if body.capacity() > charged_capacity {
            mem.charge(Part::Buffers, Some(body.capacity() - charged_capacity))?;
        } else {
            mem.release_buffers(charged_capacity - body.capacity());
        }

        let source_map = pending_map.map(|map| map.finish(&body, fm_prefix_len, None));
        mem.release_to(mark);
        self.anchor_root_for_source_map(base_dir);
        // Relativize every source label before the caller receives the map.
        let source_map = source_map.map(|mut sm| {
            let root_str = self.fs.source_root();
            let root = root_str.as_deref().map(std::path::Path::new);
            let base = opts.source_map_base.as_deref();
            for src in &mut sm.sources {
                *src = crate::source_path::relativize_source(src, base, root);
            }
            sm
        });
        Ok((crate::CompiledOutput::Markdown(body), source_map))
    }

    /// Common module processing: tokenize, parse, build scope, evaluate.
    ///
    /// `ctx.file_str` is the display path for error messages (may be `SOURCE_LABEL`).
    /// `ctx.base_dir` is the directory used to resolve relative `@import`/`@extends`.
    /// `is_md` controls whether the `type` frontmatter key is treated as a file-type marker.
    /// `upgrade` is true when the module replaces a cached skeleton entry.
    pub(super) fn process_module(
        &mut self,
        ctx: &ModuleCtx<'_>,
        is_md: bool,
        upgrade: bool,
        warnings: &mut Vec<String>,
    ) -> Result<ResolvedModule, MdsError> {
        // The module entry keeps its tree. An upgrade of a cached skeleton keeps the
        // tree that the skeleton entry counts, so the new tree stays a temporary.
        let module = self.parse_charged(ctx, !upgrade)?;

        // Parse frontmatter YAML once for both scope building and storage. The
        // module entry keeps the tree.
        let (frontmatter_values, frontmatter_bytes) =
            parse_frontmatter_mapping(module.frontmatter.as_ref(), &mut self.budget.mem)?;
        self.keep(frontmatter_bytes);

        // Branch: child template (@extends) vs. standalone module.
        if let Some(ext) = module.extends.clone() {
            return self.process_module_extends(
                module,
                ext,
                ctx,
                frontmatter_values,
                frontmatter_bytes,
                warnings,
            );
        }

        // ── Standalone (non-extending) path ──────────────────────────────────
        let raw_frontmatter = self.copy_raw_frontmatter(&module)?;

        let (mut scope, defs) = self.standalone_scope(
            &module,
            frontmatter_values.as_ref(),
            frontmatter_bytes,
            ctx,
            is_md,
            warnings,
        )?;

        // Whether "prompt" is an available export of this module (the rule
        // `ResolvedModule::is_exported` applies).
        let prompt_exported =
            is_visible_export(defs.has_explicit_exports, &defs.explicit_exports, "prompt");

        let (prompt_body, prompt_map, origin) =
            self.eval_standalone_prompt(&module.body, &mut scope, prompt_exported, ctx, warnings)?;

        // Build effective_blocks first so module.body can be moved into the Arc below.
        let effective_blocks = seed_effective_blocks(&module.body, &defs.block_names, origin);

        // Move module.body into Arc<[Node]> (reuses the Vec allocation, no element clones, P1).
        let effective_skeleton: Arc<[Node]> = Arc::from(module.body);

        let namespace = ResolvedModule::build_namespace(
            &defs.functions,
            defs.has_explicit_exports,
            &defs.explicit_exports,
            prompt_body.as_ref(),
            prompt_map.as_ref(),
        );
        Ok(ResolvedModule {
            functions: defs.functions,
            prompt_body,
            namespace,
            prompt_map,
            raw_frontmatter,
            has_explicit_exports: defs.has_explicit_exports,
            explicit_exports: defs.explicit_exports,
            effective_skeleton,
            effective_blocks,
            skeleton_origin: origin.clone(),
            frontmatter_values,
            frontmatter_bytes,
            is_skeleton: false,
        })
    }

    fn standalone_scope(
        &mut self,
        module: &Module,
        frontmatter_values: Option<&serde_yaml_ng::Mapping>,
        frontmatter_bytes: usize,
        ctx: &ModuleCtx<'_>,
        is_md: bool,
        warnings: &mut Vec<String>,
    ) -> Result<(Scope, CollectedDefs), MdsError> {
        // Build scope from frontmatter + runtime vars; extract any frontmatter imports.
        let (mut scope, fm_imports) = build_scope_from_frontmatter(
            module.frontmatter.as_ref(),
            frontmatter_values,
            frontmatter_bytes,
            is_md,
            ctx.runtime_vars,
            &mut self.budget.mem,
        )?;

        // Resolve frontmatter imports BEFORE body imports (per spec: frontmatter imports
        // resolve before body imports; a duplicate alias is a compile error — legacy
        // decision 014 in 88ddbcc~1:.devflow/decisions/decisions.md).
        self.resolve_frontmatter_imports(&fm_imports, &mut scope, ctx, warnings)?;

        // Walk the AST: collect @define functions (with closure capture), process imports/exports
        let defs = self.collect_definitions_and_imports(&module.body, &mut scope, ctx, warnings)?;
        // A function can capture the scope layer, so the entry keeps the layer too.
        if !defs.functions.is_empty() {
            self.keep(frontmatter_bytes);
        }

        // Validate that all named exports refer to defined functions or "prompt"
        validate_exports(&defs.explicit_exports, &defs.functions)?;

        // Validate semantic correctness before evaluation
        validator::validate(&module.body, &mut scope, ctx.file_str, ctx.source)?;
        Ok((scope, defs))
    }

    fn eval_standalone_prompt<'ctx>(
        &mut self,
        body: &[Node],
        scope: &mut Scope,
        prompt_exported: bool,
        ctx: &'ctx ModuleCtx<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<StandalonePrompt<'ctx>, MdsError> {
        let origin = ctx.origin(self)?;

        // Evaluate the body to get prompt text (and optionally a FragmentMap).
        //
        // When source-map mode is active and the module exports "prompt", run a
        // single evaluate_with_map pass to collect both the text and its segment records.
        // The module and its namespace share the fragment map for @include attribution.
        // Evaluate the body even if it is not exported, to distinguish the two include warnings.
        let mut body_raw = String::new();
        let builder = if self.source_map_mode && prompt_exported {
            let builder = crate::sourcemap::MapBuilder::new(origin.clone());
            // evaluate_with_map derives file/source from the builder's current source
            // (issue #58).
            Some(evaluate_with_map_seeded(
                body,
                scope,
                warnings,
                builder,
                self.budget.start_module(),
                &mut body_raw,
            )?)
        } else {
            evaluate_seeded(
                body,
                scope,
                warnings,
                ctx.file_str,
                ctx.source,
                self.budget.start_module(),
                &mut body_raw,
            )?;
            None
        };
        let (prompt_body, prompt_map) =
            self.prompt_body_and_map(body_raw, builder, ctx.file_str, warnings);
        Ok((prompt_body, prompt_map, origin))
    }
}

impl ModuleCache {
    pub(super) fn prompt_body_and_map(
        &mut self,
        body_raw: String,
        builder: Option<MapBuilder>,
        display: &str,
        warnings: &mut Vec<String>,
    ) -> (
        Option<PromptText>,
        Option<Arc<crate::sourcemap::FragmentMap>>,
    ) {
        let capacity = body_raw.capacity();
        let prompt_body = if body_raw.trim().is_empty() {
            self.budget
                .mem
                .transfer(Part::Buffers, Part::Temp, capacity);
            None
        } else {
            // Evaluation charged this capacity before its allocation.
            self.kept += capacity;
            Some(PromptText::from(body_raw))
        };
        let prompt_map = builder.and_then(|mut builder| {
            if prompt_body.is_some()
                && !builder.segments_dropped
                && self.charge_kept(builder.segments_bytes()).is_err()
            {
                builder.segments_dropped = true;
            }
            fragment_map_from(builder, prompt_body.as_deref(), display, warnings)
        });
        (prompt_body, prompt_map)
    }
}

pub(super) struct FinalizeCosts {
    pub(super) content: usize,
    pub(super) map_origin_bytes: usize,
    pub(super) prefix_lines: usize,
    pub(super) label_base_bytes: usize,
}

fn count_newlines(text: &str) -> usize {
    text.bytes().filter(|&byte| byte == b'\n').count()
}

pub(super) fn charge_finalize(
    mem: &mut MemBudget,
    builder: &MapBuilder,
    body: &str,
    costs: FinalizeCosts,
) -> bool {
    let n = builder.segment_count() + count_newlines(body);
    let points = bytes(n, 36);
    // Points (16 B), resolved points (20 B), and the sampled CR table.
    let work = points
        .and_then(|n| n.checked_add(4 * (body.len() / 64 + 1)))
        .and_then(|n| n.checked_add(builder.segments_bytes()))
        .and_then(|n| n.checked_add(costs.map_origin_bytes));
    // A frontmatter line can also add a semicolon before the first point.
    let output = points
        .and_then(|n| n.checked_add(bytes(costs.prefix_lines, 2)?))
        .and_then(|n| n.checked_add(costs.content))
        .and_then(|n| n.checked_add(builder.source_labels_bytes(costs.label_base_bytes)?));
    let mark = mem.mark();
    if mem.charge(Part::Temp, work).is_err() {
        return false;
    }
    if mem.charge(Part::Buffers, output).is_err() {
        mem.release_to(mark);
        return false;
    }
    true
}

/// Apply AC-PERF-03 and AC-SEC-04 degradation checks to a completed builder.
///
/// - AC-PERF-03: if the segment cap was hit, degrade to `None` (a partial map
///   is misleading; warn and discard the builder).
/// - AC-SEC-04: if `sourcesContent` total bytes exceed the ceiling, set
///   `no_sources_content` so [`crate::sourcemap::PendingMap::finish`] omits the array.
/// - Caller opt-out: `!opts.include_sources_content` also sets the flag.
pub(super) fn apply_map_degradation(
    mut builder: MapBuilder,
    total_src_bytes: usize,
    opts: &CompileOptions,
    warnings: &mut Vec<String>,
) -> Option<crate::sourcemap::MapBuilder> {
    if builder.segments_dropped {
        warnings.push(format!(
            "source map segment cap ({} segments) exceeded; \
             no source map will be generated",
            crate::limits::MAX_SOURCEMAP_SEGMENTS,
        ));
        return None;
    }
    if total_src_bytes > MAX_SOURCES_CONTENT_BYTES {
        warnings.push(format!(
            "sourcesContent total size ({} bytes) exceeds the {} byte ceiling; \
             sourcesContent will be omitted from the source map",
            total_src_bytes,
            crate::limits::MAX_SOURCES_CONTENT_BYTES,
        ));
        builder.no_sources_content = true;
    }
    if !opts.include_sources_content {
        builder.no_sources_content = true;
    }
    Some(builder)
}
