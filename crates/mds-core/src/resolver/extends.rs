//! Inheritance resolution, merged frontmatter, and skeleton modules.

use crate::data::resolve::Chain;

use super::{
    apply_block_overrides, attach_import_span, build_cycle_string, build_scope_from_frontmatter,
    bytes, check_child_only_blocks, check_module_type, evaluate_messages_seeded, is_visible_export,
    merged_frontmatter_layer, parse_frontmatter_imports_from_yaml, parse_frontmatter_mapping,
    seed_effective_blocks, spliced_regions, validate_exports, validate_import_path, validator, Arc,
    CollectedDefs, EffectiveBlock, EvalBudget, ExtendsComponents, FrontmatterImport, ImportSite,
    IndexMap, MdsError, ModuleCache, ModuleCtx, ModuleRef, Node, OnceCell, Origin, Part,
    PromptText, ResolvedModule, Scope, VarLayer,
};

pub(super) struct MergedScope {
    pub(super) scope: Scope,
    pub(super) mapping: serde_yaml_ng::Mapping,
    pub(super) charge: usize,
    pub(super) data_chain: Chain,
}

impl ModuleCache {
    /// Evaluate an `@extends` chain's spliced regions: the terminal step the entry's
    /// extends path (`process_module_intrinsic_opts`) and an imported extending module
    /// (`process_module_extends`) share.
    ///
    /// With a `seed` — the chain's `skeleton_origin`, so a map lists the root base
    /// first however the chain is reached — every region is recorded into a
    /// [`crate::sourcemap::MapBuilder`] seeded with it; without one nothing is
    /// recorded. The body is evaluated either way, all regions under one
    /// [`EvalBudget`] ([`Self::evaluate_regions_with_map`]).
    pub(super) fn evaluate_extends_regions(
        regions: &[(&[crate::ast::Node], &Origin)],
        seed: Option<&Origin>,
        scope: &mut crate::scope::Scope,
        warnings: &mut Vec<String>,
        budget: &mut EvalBudget,
    ) -> Result<(String, Option<crate::sourcemap::MapBuilder>), MdsError> {
        let builder = seed.map(|origin| crate::sourcemap::MapBuilder::new(origin.clone()));
        Self::evaluate_regions_with_map(regions, scope, warnings, builder, budget)
    }

    /// Messages-mode twin of [`Self::evaluate_regions_with_map`]: collect the
    /// `@message` blocks of spliced `@extends` regions in order.
    ///
    /// Each region is evaluated against its own [`Origin`]'s display path and source,
    /// so a span-bearing error — orphan text outside a `@message`
    /// (`mds::mixed_content`) or a cross-type comparison — names the file the region
    /// came from (#115). PF-004: one message vector and one [`EvalBudget`] cover every
    /// region, so the message-count, message-byte and iteration caps apply to the
    /// whole module evaluation rather than to each region.
    pub(super) fn evaluate_message_regions(
        regions: &[(&[crate::ast::Node], &Origin)],
        scope: &mut crate::scope::Scope,
        warnings: &mut Vec<String>,
        budget: &mut EvalBudget,
    ) -> Result<Vec<crate::evaluator::EvalMessage>, MdsError> {
        let mut messages = Vec::new();
        for (nodes, origin) in regions {
            evaluate_messages_seeded(
                nodes,
                scope,
                warnings,
                &origin.display,
                &origin.source,
                budget,
                &mut messages,
            )?;
        }
        Ok(messages)
    }

    /// Build the merged scope for an `@extends` child (step 3d of `resolve_extends_components`).
    ///
    /// Covers steps 3d-i through 3d-vi:
    /// - Parse FM imports from base and child frontmatter (3d-i).
    /// - Deep-merge base and child FM mappings (3d-ii).
    /// - Build scope from the merged mapping with runtime vars (3d-iii).
    /// - Resolve base FM imports against the base file (3d-iv, frontmatter-first ordering).
    /// - Resolve child FM imports against the child file (3d-v).
    /// - Merge base functions into scope (3d-vi).
    ///
    /// # Borrow note
    /// `base` is taken as an explicit `&ResolvedModule` (not via `self`) so that the
    /// borrow of `base.skeleton_origin.source` (needed for `base_ctx.source`) is
    /// independent of `&mut self`. The caller passes `&*arc` to deref from `Arc`.
    ///
    /// # Invariants preserved
    /// - Base FM imports resolved BEFORE child FM imports (frontmatter-first ordering).
    /// - `deep_merge_yaml` applies `MAX_FRONTMATTER_MERGE_DEPTH` cap.
    /// - `resolve_frontmatter_imports` → `resolve_import_from` → `resolve_by_key_skeleton`
    ///   preserves PF-004 safety (cycle detection, `check_import_depth`, file-size cap).
    pub(super) fn build_merged_extends_scope(
        &mut self,
        base: &ResolvedModule,
        child_fm: &Option<serde_yaml_ng::Mapping>,
        child_fm_bytes: usize,
        base_key: &str,
        ctx: &ModuleCtx<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<MergedScope, MdsError> {
        // 3d-i: Extract frontmatter imports from BOTH base and child BEFORE the deep merge.
        //
        // Base imports resolve relative to the BASE file's directory (base_base_dir, derived
        // from base_key via FileSystem::parent_dir). Child imports resolve relative to the
        // CHILD file's directory (ctx.base_dir). Both sets are resolved; a duplicate alias
        // across base+child → mds::name_collision (a duplicate alias across frontmatter
        // and body is a compile error).
        let base_fm_imports: Vec<FrontmatterImport> = base
            .frontmatter_values
            .as_ref()
            .and_then(|m| m.get("imports"))
            .map(parse_frontmatter_imports_from_yaml)
            .transpose()?
            .unwrap_or_default();

        let child_fm_imports: Vec<FrontmatterImport> = child_fm
            .as_ref()
            .and_then(|m| m.get("imports"))
            .map(parse_frontmatter_imports_from_yaml)
            .transpose()?
            .unwrap_or_default();

        // 3d-ii: Deep-merge base and child frontmatter value Mappings.
        // Reserved keys (imports, type, extends) are excluded by deep_merge_yaml.
        // Precedence: base < child (child overrides base on collision).
        // The merged mapping is a temporary.
        let (merged_mapping, charge) = self.merge_frontmatter(base, child_fm, child_fm_bytes)?;

        // 3d-iii: Build scope from the merged mapping. Runtime vars applied LAST
        // (base < child < runtime, F7, decision #3). The layer is charged with the
        // merge charge as a temporary first. It stays a temporary: an extending child
        // has only @block overrides, so no function of its own can capture the layer.
        self.budget.mem.charge(Part::Temp, Some(charge))?;
        let mut vars = merged_frontmatter_layer(&merged_mapping)?;
        let is_md = self.fs.is_markdown(ctx.key);
        // A valid extending child has no top-level body imports.
        let data_chain = self.extend_data_chain(
            &base.data_chain,
            ctx,
            is_md,
            child_fm.as_ref(),
            &child_fm_imports,
            &[],
        )?;
        let (names, pending) = self.bind_data(
            &data_chain,
            Some(&merged_mapping),
            false,
            ctx.runtime_vars,
            &mut vars,
        )?;
        let mut scope = Scope::with_layers(Arc::clone(ctx.runtime_vars), Arc::new(vars));
        scope.set_data_names(names);
        scope.set_pending_data(pending);

        // 3d-iv: Resolve base frontmatter imports against base_key (frontmatter-first ordering,
        // PF-004 safe via resolve_frontmatter_imports → resolve_import_from).
        // Use a ctx pointing to the base file with its REAL source bytes so that any
        // span-carrying error here attributes correctly AND the at() debug_assert can't
        // false-fire. The skeleton_origin already carries these bytes; we use them here
        // for consistency.
        let base_source_ref: &str = &base.skeleton_origin.source;
        let base_base_dir = self.fs.parent_dir(base_key);
        let base_display_owned = crate::source_path::display_path_for(&*self.fs, base_key);
        let base_ctx = ModuleCtx {
            key: base_key,
            file_str: &base_display_owned,
            source: base_source_ref,
            base_dir: &base_base_dir,
            runtime_vars: ctx.runtime_vars,
            origin: OnceCell::new(),
        };
        self.resolve_frontmatter_imports(&base_fm_imports, &mut scope, &base_ctx, warnings)?;

        // 3d-v: Resolve child frontmatter imports against child directory (ctx.base_dir).
        // Duplicate alias across base+child → mds::name_collision (same error as today).
        self.resolve_frontmatter_imports(&child_fm_imports, &mut scope, ctx, warnings)?;

        // 3d-vi: Merge base functions into scope (F12: base default block calling a base @define).
        // Collision with child frontmatter-imported functions → name_collision.
        let rebound = self.collect_cached_base_definitions(base, ctx.runtime_vars, warnings)?;
        let functions = rebound
            .as_ref()
            .map_or(&base.functions, |defs| &defs.functions);
        for (name, func) in functions {
            if scope.get_function(name).is_some() {
                return Err(MdsError::name_collision(name.clone()));
            }
            scope.set_function(name, Arc::clone(func));
        }

        Ok(MergedScope {
            scope,
            mapping: merged_mapping,
            charge,
            data_chain,
        })
    }

    /// Rebuild a cached standalone base's lexical closures without its data values.
    /// The leaf scope supplies the winning values, as it does for skeleton closures.
    fn collect_cached_base_definitions(
        &mut self,
        base: &ResolvedModule,
        runtime: &Arc<VarLayer>,
        warnings: &mut Vec<String>,
    ) -> Result<Option<CollectedDefs>, MdsError> {
        let origin = &base.skeleton_origin;
        let Some(root) = self.modules.get(origin.file.as_ref()).cloned() else {
            return Ok(None);
        };
        if root.is_skeleton
            || !crate::data::resolve::Declarations::contains_data(&root.data_chain)
            || !base
                .effective_skeleton
                .iter()
                .any(|node| matches!(node, Node::Define(_)))
            || base.functions.is_empty()
        {
            return Ok(None);
        }
        let dir = self.fs.parent_dir(&origin.file);
        let ctx = ModuleCtx {
            key: &origin.file,
            file_str: &origin.display,
            source: &origin.source,
            base_dir: &dir,
            runtime_vars: runtime,
            origin: OnceCell::from(origin.clone()),
        };
        // The tree charge covers the new definitions and capture maps.
        let module = self.parse_charged(&ctx, true)?;
        let (mut scope, imports) = build_scope_from_frontmatter(
            module.frontmatter.as_ref(),
            root.frontmatter_values.as_ref(),
            root.frontmatter_bytes,
            self.fs.is_markdown(ctx.key),
            runtime,
            &mut self.budget.mem,
        )?;
        self.keep(root.frontmatter_bytes);
        self.resolve_frontmatter_imports(&imports, &mut scope, &ctx, warnings)?;
        self.collect_definitions_and_imports(&module.body, &mut scope, &ctx, warnings)
            .map(Some)
    }

    /// Serialize a deep-merged frontmatter `Mapping` for `frontmatter_prefix`, by
    /// reference.
    ///
    /// Return `Ok(None)` for an empty mapping. Otherwise return the canonical YAML
    /// representation with a final newline and no fences.
    ///
    /// `charge` is the merge charge of the mapping. Four text bytes per charged byte,
    /// plus one for the newline, bound the text: an escaped control byte takes four.
    /// The bound is charged as a temporary before the text is allocated. A text past
    /// the bound grows once, and that growth is charged after it. The entry keeps the
    /// text, so its charge moves to the kept bytes at the text capacity.
    ///
    /// Used by `resolve_extends_components` for the `@extends` deep-merged FM output (#154).
    ///
    /// # Errors
    ///
    /// Return `mds::internal` if the mapping has a reserved output key at the top level.
    /// Return `mds::yaml` if YAML serialization fails. Do not omit the frontmatter on error.
    pub(super) fn serialize_merged_frontmatter(
        &mut self,
        mapping: &serde_yaml_ng::Mapping,
        charge: usize,
    ) -> Result<Option<String>, MdsError> {
        if mapping.is_empty() {
            return Ok(None);
        }
        if crate::RESERVED_OUTPUT_KEYS
            .iter()
            .any(|key| mapping.contains_key(key.name))
        {
            return Err(MdsError::internal());
        }
        let bound = self
            .budget
            .mem
            .charge(Part::Temp, bytes(charge, 4).and_then(|n| n.checked_add(1)))?;
        let mut yaml = Vec::with_capacity(bound);
        serde_yaml_ng::to_writer(&mut yaml, mapping)
            .map_err(|e| MdsError::yaml_error(e.to_string()))?;
        // The serializer writes UTF-8 only.
        let mut text = String::from_utf8(yaml).map_err(|_| MdsError::internal())?;
        text.truncate(text.trim_end().len());
        if text.is_empty() {
            return Ok(None);
        }
        text.push('\n');
        self.budget
            .mem
            .charge(Part::Temp, text.capacity().checked_sub(bound))?;
        self.keep(text.capacity());
        Ok(Some(text))
    }

    /// Shared extends-pipeline: steps 3a-3d are identical for the text-cached and
    /// intrinsic paths.
    ///
    /// Builds the effective skeleton and blocks (base skeleton plus the winning block
    /// overrides, each with its `Origin`) and the `scope` (deep-merged frontmatter + FM
    /// imports + functions) needed by both `process_module_extends` (cached text path)
    /// and `process_module_intrinsic`.
    ///
    /// Callers differ only in the terminal step (step 3e), which walks the skeleton's
    /// `spliced_regions` and evaluates each against its own origin:
    /// - Cached text path: `validate` → `evaluate_extends_regions` (+ a FragmentMap)
    /// - Intrinsic path:   `has_message_block` dispatch → `evaluate_message_regions`
    ///   (Messages) or `evaluate_extends_regions` + clean/frontmatter (Markdown)
    ///
    /// Factoring here enforces that BOTH modes go through the same PF-004-safe
    /// `resolve_by_key_skeleton` path for the base, and share one copy of the
    /// scope-construction pipeline (re-validated at the leaf even though every part
    /// passed its parse-time check; decision #3/7).
    pub(super) fn resolve_extends_components(
        &mut self,
        module: &crate::ast::Module,
        ext: &crate::ast::ExtendsDirective,
        ctx: &ModuleCtx<'_>,
        frontmatter_values: &Option<serde_yaml_ng::Mapping>,
        frontmatter_bytes: usize,
        warnings: &mut Vec<String>,
    ) -> Result<ExtendsComponents, MdsError> {
        // ── Step 3a: validate and resolve the base in skeleton mode ──────────
        validate_import_path(&ext.path)
            .map_err(|e| attach_import_span(e, ctx.file_str, ctx.source, ext.offset))?;

        let base_key = self
            .normalize_module_edge(ctx, &ext.path)
            .map_err(|e| attach_import_span(e, ctx.file_str, ctx.source, ext.offset))?;

        // PF-004 (avoids PF-004): resolve through resolve_by_key_skeleton so cycle
        // detection, MAX_IMPORT_DEPTH, dependency tracking, and MAX_FILE_SIZE all apply.
        // This guard holds for BOTH text and messages modes — they share this path.
        let base_ref = ModuleRef {
            key: &base_key,
            typed: &ext.path,
        };
        let base = self
            .resolve_by_key_skeleton(
                base_ref,
                ctx.runtime_vars,
                ctx.import_site(ext.offset),
                warnings,
            )
            .map_err(|e| attach_import_span(e, ctx.file_str, ctx.source, ext.offset))?;

        // ── Step 3b: child-only-blocks check ─────────────────────────────────
        // Every top-level node in module.body must be Node::Block or whitespace-only Text.
        // (Frontmatter and @extends are already split out of module.body by the parser.)
        check_child_only_blocks(&module.body, ctx)?;

        // ── Step 3c: build effective_blocks from base, applying child overrides ──
        // Clone base's map first (diamond-inheritance safe — never mutate cached base).
        let effective_blocks =
            apply_block_overrides(&base.effective_blocks, &module.body, ctx, self)?;

        // effective_skeleton is the root ancestor's body (Arc::clone — O(1), no deep-copy, P1).
        let effective_skeleton = Arc::clone(&base.effective_skeleton);

        // skeleton_origin is the root base's Origin — Arc::clone'd (O(1)), never re-allocated.
        // This is how the base file's source bytes "ride along" to the leaf for span attribution.
        let skeleton_origin = base.skeleton_origin.clone();

        // ── Step 3d: build merged scope (Phase 3: deep merge + per-file FM imports) ──
        // Applies decision #3 (base < child < runtime) and decision #7 (reserved-key
        // exclusion, array wholesale replace, both sets of FM imports resolved per-file).
        // The merged_mapping is also returned so we can serialize it for the output
        // frontmatter (#154: @extends emits deep-merged frontmatter).
        let MergedScope {
            mut scope,
            mapping: merged_mapping,
            charge,
            data_chain,
        } = self.build_merged_extends_scope(
            &base,
            frontmatter_values,
            frontmatter_bytes,
            &base_key,
            ctx,
            warnings,
        )?;
        let merged_frontmatter = self.serialize_merged_frontmatter(&merged_mapping, charge)?;

        // Collect child's own definitions from its body (currently zero @define after
        // child-only-blocks check, but structurally correct).
        let CollectedDefs {
            functions: child_functions,
            has_explicit_exports,
            explicit_exports,
            block_names: _,
        } = self.collect_definitions_and_imports(&module.body, &mut scope, ctx, warnings)?;

        // Merge child-defined functions over base (child wins).
        let mut functions = base.functions.clone();
        for (name, func) in child_functions {
            functions.insert(name, func);
        }

        validate_exports(&explicit_exports, &functions)?;

        Ok(ExtendsComponents {
            scope,
            functions,
            effective_skeleton,
            effective_blocks,
            skeleton_origin,
            has_explicit_exports,
            explicit_exports,
            merged_frontmatter,
            data_chain,
            merged_mapping,
            merged_mapping_bytes: charge,
        })
    }

    /// Validate an extends pipeline's spliced regions, each against its own origin source.
    ///
    /// Walks `spliced_regions` and calls `validator::validate` per region so each region's
    /// AST node offsets are paired with the correct source string (fixing the cross-source
    /// OutOfBounds diagnostic bug), under the region's display path — never the canonical
    /// `origin.file`, which is absolute for `NativeFs` (R3 / CWE-209, #114). The scope is
    /// threaded through all regions so `@define` / `@for` scope push/pop behaves
    /// identically to a whole-slice validate.
    ///
    /// This single helper is called by BOTH `process_module_extends` (cached text path)
    /// and `process_module_intrinsic` (@extends branch) — enforcing PF-004 parity: the two
    /// parallel paths can never drift because they share one implementation.
    ///
    /// Re-validate at the leaf (on the spliced regions), not at intermediate bases.
    pub(super) fn validate_extends_components(
        components: &ExtendsComponents,
        scope: &mut Scope,
    ) -> Result<(), MdsError> {
        for (nodes, origin) in spliced_regions(
            &components.effective_skeleton,
            &components.effective_blocks,
            &components.skeleton_origin,
        ) {
            validator::validate(nodes, scope, &origin.display, &origin.source)?;
        }
        Ok(())
    }

    /// Evaluate an extending child template in text mode.
    ///
    /// Delegates the shared pipeline (steps 3a-3d) to `resolve_extends_components`,
    /// then runs `validate_extends_components` + a region-by-region evaluation (step 3e),
    /// which in source-map mode also yields the module's FragmentMap (#412).
    ///
    /// Decision #2: base is NEVER validated/evaluated standalone — deferred to leaf.
    /// PF-004: base is read via resolve_by_key_skeleton (FileSystem trait, never std::fs).
    pub(super) fn process_module_extends(
        &mut self,
        module: crate::ast::Module,
        ext: crate::ast::ExtendsDirective,
        ctx: &ModuleCtx<'_>,
        frontmatter_values: Option<serde_yaml_ng::Mapping>,
        frontmatter_bytes: usize,
        warnings: &mut Vec<String>,
    ) -> Result<ResolvedModule, MdsError> {
        let components = self.resolve_extends_components(
            &module,
            &ext,
            ctx,
            &frontmatter_values,
            frontmatter_bytes,
            warnings,
        )?;

        // ── Step 3e: validate + evaluate the spliced regions ──────────────────
        // Validate per-region so each region's offsets are checked against the correct
        // source (fixes the cross-source OutOfBounds diagnostic bug). This is what makes
        // E12 work: a base default block referencing an undefined var is caught HERE
        // against the merged leaf scope. (dynamically-assembled content is re-validated
        // even though each part was checked at parse time.)
        {
            let mut scope = components.scope.clone();
            Self::validate_extends_components(&components, &mut scope)?;
        }

        let ExtendsComponents {
            mut scope,
            functions,
            effective_skeleton,
            effective_blocks,
            skeleton_origin,
            has_explicit_exports,
            explicit_exports,
            merged_frontmatter,
            data_chain,
            merged_mapping,
            merged_mapping_bytes,
        } = components;

        // Base-skeleton nodes, base defaults and child overrides index into different
        // files, so each region is evaluated against its own origin (#114). In
        // source-map mode the regions are recorded into a builder seeded with the
        // chain's root, as for a direct compile of this file, and kept as the
        // FragmentMap an importer's `@include` splices (#412) — under the same gate as
        // a standalone module. The body is evaluated either way.
        let prompt_exported = is_visible_export(has_explicit_exports, &explicit_exports, "prompt");
        let regions = spliced_regions(&effective_skeleton, &effective_blocks, &skeleton_origin);
        let seed = (self.source_map_mode && prompt_exported).then_some(&skeleton_origin);
        let (prompt_body, prompt_map) = if self.data.pending {
            (None, None)
        } else {
            let (body_raw, builder) = Self::evaluate_extends_regions(
                &regions,
                seed,
                &mut scope,
                warnings,
                self.budget.start_module(),
            )?;
            self.prompt_body_and_map(body_raw, builder, ctx.file_str, warnings)
        };

        let namespace = ResolvedModule::build_namespace(
            &functions,
            has_explicit_exports,
            &explicit_exports,
            prompt_body.as_ref(),
            prompt_map.as_ref(),
        );
        self.keep(merged_mapping_bytes);
        Ok(ResolvedModule {
            functions,
            prompt_body,
            namespace,
            prompt_map,
            // #154: emit deep-merged frontmatter (base < child, reserved keys excluded)
            // rather than the child's raw frontmatter.
            raw_frontmatter: merged_frontmatter,
            has_explicit_exports,
            explicit_exports,
            effective_skeleton,
            effective_blocks,
            skeleton_origin,
            frontmatter_values: Some(merged_mapping),
            frontmatter_bytes: merged_mapping_bytes,
            data_chain,
            data: scope.reaches_data(),
            is_skeleton: false,
        })
    }

    /// Resolve a module in skeleton mode: tokenize → parse → collect only (no validate/evaluate).
    ///
    /// Uses the same module cache and resolving stack as resolve_by_key, so cycle detection
    /// (mds::circular_import), MAX_IMPORT_DEPTH, dependency tracking, and the MAX_FILE_SIZE
    /// guard all apply automatically (decision #1, PF-004).
    ///
    /// Cache-poisoning invariant: both skeleton and full-compile entries are stored under the
    /// same normalized key. The first resolution wins. See ResolvedModule doc comment for details.
    fn resolve_by_key_skeleton(
        &mut self,
        module: ModuleRef<'_>,
        runtime_vars: &Arc<VarLayer>,
        import_site: ImportSite<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<Arc<ResolvedModule>, MdsError> {
        let key = module.key;
        // Cache hit — return immediately (full or skeleton entry, both are valid bases).
        if let Some(cached) = self.modules.get(key) {
            return Ok(Arc::clone(cached));
        }

        // Cycle detection — same resolving stack as resolve_by_key (decision #1, E5).
        if self.resolving.contains(key) {
            let cycle = build_cycle_string(&self.resolving, key);
            return Err(MdsError::circular_import(cycle));
        }

        // Depth guard (E6).
        self.check_import_depth()?;

        // Module count guard (REL-2, applies PF-004).
        self.check_module_count(key)?;

        // PF-004: read via FileSystem trait — NEVER std::fs.
        let (source, read_bytes) = self.read_module(key)?;
        let is_md = self.fs.is_markdown(key);
        check_module_type(module, &source).map_err(|err| import_site.attach_type_error(err))?;

        let (mark, read) = self.begin_module(key, runtime_vars, read_bytes);
        let resolved = read.and_then(|()| {
            let base_dir_owned = self.fs.parent_dir(key);
            let display_owned = crate::source_path::display_path_for(&*self.fs, key);
            let ctx = ModuleCtx {
                key,
                file_str: &display_owned,
                source: &source,
                base_dir: &base_dir_owned,
                runtime_vars,
                origin: OnceCell::new(),
            };
            self.process_module_skeleton(&ctx, is_md, warnings)
        });
        #[cfg(test)]
        let probe = mark.probe;
        let kept = self.close_module(mark);

        let popped = self.pop_resolving(key);
        let resolved = resolved?;
        popped?;

        let arc = Arc::new(resolved);
        self.retain_module(key, &arc, kept);
        #[cfg(test)]
        {
            self.module_probes[probe].buffers_after_insert = Some(self.budget.mem.buffers);
        }
        Ok(arc)
    }

    /// Handle the intermediate-base arm of `process_module_skeleton`.
    ///
    /// Called when a module being resolved as a skeleton itself extends a grandparent.
    /// Resolves the grandparent in skeleton mode, applies block overrides from the
    /// intermediate base's body, Arc::clone's the skeleton_origin, and computes the
    /// transitive frontmatter merge (grandparent FM < own FM) so that a leaf descending
    /// from this intermediate base gets the full chain without re-traversal.
    ///
    /// # Invariants preserved
    /// - `resolve_by_key_skeleton` (never `resolve_by_key`) — skeleton-only evaluation.
    /// - `check_import_depth` + cycle detection (`self.resolving`) apply via
    ///   `resolve_by_key_skeleton`.
    /// - `deep_merge_yaml` depth cap (`MAX_FRONTMATTER_MERGE_DEPTH`) applies transitively.
    /// - `skeleton_origin` Arc is cloned from the grandparent (origin rides along the
    ///   data — never a path→source lookup; legacy decision 022 in the same
    ///   git-history file).
    #[allow(clippy::type_complexity)]
    fn resolve_intermediate_base(
        &mut self,
        ext: &crate::ast::ExtendsDirective,
        own_frontmatter: (&Option<serde_yaml_ng::Mapping>, usize),
        imports: &[FrontmatterImport],
        module_body: &[Node],
        ctx: &ModuleCtx<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<
        (
            Arc<[Node]>,
            IndexMap<String, EffectiveBlock>,
            Origin,
            Option<serde_yaml_ng::Mapping>,
            usize,
            Chain,
        ),
        MdsError,
    > {
        let (own_fm, own_fm_bytes) = own_frontmatter;
        validate_import_path(&ext.path)
            .map_err(|e| attach_import_span(e, ctx.file_str, ctx.source, ext.offset))?;
        let grandparent_key = self
            .normalize_module_edge(ctx, &ext.path)
            .map_err(|e| attach_import_span(e, ctx.file_str, ctx.source, ext.offset))?;
        let grandparent_ref = ModuleRef {
            key: &grandparent_key,
            typed: &ext.path,
        };
        let grandparent = self
            .resolve_by_key_skeleton(
                grandparent_ref,
                ctx.runtime_vars,
                ctx.import_site(ext.offset),
                warnings,
            )
            .map_err(|e| attach_import_span(e, ctx.file_str, ctx.source, ext.offset))?;

        // Child-only-blocks check for this intermediate base (3b).
        check_child_only_blocks(module_body, ctx)?;

        let eff_blocks =
            apply_block_overrides(&grandparent.effective_blocks, module_body, ctx, self)?;

        // skeleton_origin Arc::clone'd from grandparent — the root base's source bytes
        // ride down the chain so the leaf's validate_extends_components can attribute
        // non-block skeleton node diagnostics to the root file (Risk #2; origin rides
        // along via Arc, never reconstructed from a path cache).
        let skel_origin = grandparent.skeleton_origin.clone();

        // Phase 3: transitive FM merge: grandparent.frontmatter_values < own_fm_values.
        // This produces the accumulated FM for this intermediate base, so a leaf
        // descending from it gets the full transitive chain without re-traversing.
        // The entry keeps the merged tree with its merge charge; the own tree stays
        // a temporary.
        let (merged_fm, charge) = self.merge_frontmatter(&grandparent, own_fm, own_fm_bytes)?;
        let (accumulated_fm, accumulated_bytes) = if merged_fm.is_empty() {
            (None, 0)
        } else {
            (Some(merged_fm), charge)
        };

        let data_chain = self.extend_data_chain(
            &grandparent.data_chain,
            ctx,
            self.fs.is_markdown(ctx.key),
            own_fm.as_ref(),
            imports,
            module_body,
        )?;
        Ok((
            Arc::clone(&grandparent.effective_skeleton),
            eff_blocks,
            skel_origin,
            accumulated_fm,
            accumulated_bytes,
            data_chain,
        ))
    }

    /// Tokenize → parse → collect (functions/blocks/frontmatter), NO validate/evaluate.
    ///
    /// Called when this file is a base for @extends. The resulting ResolvedModule has
    /// prompt_body = None. All fields required by extending children are populated.
    fn process_module_skeleton(
        &mut self,
        ctx: &ModuleCtx<'_>,
        is_md: bool,
        warnings: &mut Vec<String>,
    ) -> Result<ResolvedModule, MdsError> {
        // The skeleton entry keeps its tree.
        let module = self.parse_charged(ctx, true)?;

        let raw_frontmatter = self.copy_raw_frontmatter(&module)?;
        // own_fm_values: this module's raw frontmatter (not yet merged with ancestors).
        let (own_fm_values, own_fm_bytes) =
            parse_frontmatter_mapping(module.frontmatter.as_ref(), &mut self.budget.mem)?;

        // Build scope for @define closure capture (base functions must be available).
        let (mut scope, fm_imports) = build_scope_from_frontmatter(
            module.frontmatter.as_ref(),
            own_fm_values.as_ref(),
            own_fm_bytes,
            is_md,
            ctx.runtime_vars,
            &mut self.budget.mem,
        )?;
        self.resolve_frontmatter_imports(&fm_imports, &mut scope, ctx, warnings)?;

        let CollectedDefs {
            functions,
            has_explicit_exports,
            explicit_exports,
            block_names,
        } = self.collect_definitions_and_imports(&module.body, &mut scope, ctx, warnings)?;
        // A function can capture the scope layer, so the entry keeps the layer too.
        if !functions.is_empty() {
            self.keep(own_fm_bytes);
        }

        // Multi-level chain (A←B←C): B may itself extend A.
        // B's effective_skeleton = A's effective_skeleton (Arc::clone, O(1) fold).
        // B's effective_blocks = clone(A.effective_blocks) + B's overrides (most-derived wins, F3).
        // B's skeleton_origin = A's skeleton_origin (Arc::clone — root source rides down the chain).
        //
        // Phase 3: B's frontmatter_values must be the transitive deep-merge of A's accumulated FM
        // with B's own FM (A < B), so that when C later merges against B, it gets A+B+C.
        let (
            effective_skeleton,
            effective_blocks,
            skeleton_origin,
            frontmatter_values,
            frontmatter_bytes,
            data_chain,
        ) = if let Some(ext) = module.extends.as_ref() {
            self.resolve_intermediate_base(
                ext,
                (&own_fm_values, own_fm_bytes),
                &fm_imports,
                &module.body,
                ctx,
                warnings,
            )?
        } else {
            // Root base: own body is the skeleton; blocks seeded from own @block declarations.
            let data_chain = self.extend_data_chain(
                &None,
                ctx,
                is_md,
                own_fm_values.as_ref(),
                &fm_imports,
                &module.body,
            )?;
            let root_origin = ctx.origin(self)?.clone();
            // Seed blocks first so module.body can be moved into the Arc (no element clones, P1).
            let eff_blocks = seed_effective_blocks(&module.body, &block_names, &root_origin);
            let eff_skeleton: Arc<[Node]> = Arc::from(module.body);
            // Root base: frontmatter_values is its own raw FM (no ancestors).
            (
                eff_skeleton,
                eff_blocks,
                root_origin,
                own_fm_values,
                own_fm_bytes,
                data_chain,
            )
        };
        // The entry keeps its frontmatter tree.
        self.keep(frontmatter_bytes);

        // Skeleton entries have no evaluated text or fragment map.
        let prompt_body: Option<PromptText> = None;
        debug_assert!(
            prompt_body.is_none(),
            "A1 invariant: skeleton entry (is_skeleton=true) must have prompt_body=None"
        );
        let namespace = ResolvedModule::build_namespace(
            &functions,
            has_explicit_exports,
            &explicit_exports,
            prompt_body.as_ref(),
            None,
        );
        Ok(ResolvedModule {
            functions,
            namespace,
            prompt_body,
            prompt_map: None,
            raw_frontmatter,
            has_explicit_exports,
            explicit_exports,
            effective_skeleton,
            effective_blocks,
            skeleton_origin,
            frontmatter_values,
            frontmatter_bytes,
            data: scope.reaches_data()
                || crate::data::resolve::Declarations::contains_data(&data_chain),
            data_chain,
            is_skeleton: true,
        })
    }
}
