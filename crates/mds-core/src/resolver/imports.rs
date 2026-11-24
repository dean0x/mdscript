//! Definition and import walks, with export collection.

use super::{
    attach_frontmatter_index, attach_import_span, collect_block, collect_define,
    count_module_defines, line_len_at, validate_import_path, Arc, CollectedDefs, ExportDirective,
    FrontmatterImport, HashMap, HashSet, ImportDirective, ImportSite, MdsError, ModuleCache,
    ModuleCtx, ModuleRef, Node, ResolvedModule, Scope,
};

impl ModuleCache {
    /// Resolve an import relative to an explicit directory `base_dir`.
    ///
    /// Validates the import path, resolves it via `FileSystem::normalize_in_dir`
    /// (directory-anchored — no sentinel-path coupling), then delegates to
    /// [`ModuleCache::resolve_by_key`], which names the module by `relative` as written.
    fn resolve_import_from(
        &mut self,
        ctx: &ModuleCtx<'_>,
        relative: &str,
        import_site: Option<ImportSite<'_>>,
        warnings: &mut Vec<String>,
    ) -> Result<Arc<ResolvedModule>, MdsError> {
        validate_import_path(relative)?;
        let key = self.normalize_module_edge(ctx, relative)?;
        let module = ModuleRef {
            key: &key,
            typed: relative,
        };
        self.resolve_by_key(module, ctx.runtime_vars, import_site, warnings)
    }

    /// Bind an exported prompt, or declare its type unknown if output is pending.
    fn import_prompt(&self, resolved: &ResolvedModule, scope: &mut Scope) -> bool {
        if let Some(prompt) = resolved.shared_prompt_value() {
            scope.set_var_shared("prompt", prompt);
            if resolved.data {
                // Imports bind the global frame, so its variables can now hold data.
                scope.mark_frame_data();
            }
            true
        } else if self.data.pending && resolved.is_exported("prompt") {
            scope.mark_pending_prompt();
            true
        } else {
            false
        }
    }

    /// Walk the AST body and collect `@define` functions (with closure capture),
    /// process `@import` directives, and record `@export` / `@export...from` entries.
    ///
    /// Returns a `CollectedDefs` struct with self-documenting field names.
    pub(super) fn collect_definitions_and_imports(
        &mut self,
        body: &[Node],
        scope: &mut Scope,
        ctx: &ModuleCtx<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<CollectedDefs, MdsError> {
        let mut defs = CollectedDefs {
            functions: HashMap::new(),
            has_explicit_exports: false,
            explicit_exports: HashSet::new(),
            block_names: HashSet::new(),
        };

        let mut block_count: usize = 0;
        let mut define_count: usize = 0;
        for node in body {
            count_module_defines(node, &mut define_count)?;
            match node {
                Node::Define(def) => collect_define(def, &mut defs, scope, ctx, self)?,
                Node::Import(import) => self.resolve_import(import, scope, ctx, warnings)?,
                Node::Export(export) => self.collect_export(export, &mut defs, ctx, warnings)?,
                Node::Block(block) => {
                    block_count += 1;
                    collect_block(block, &mut defs, block_count, ctx)?;
                }
                _ => {}
            }
        }

        Ok(defs)
    }

    /// Process a single `@export` directive, updating `defs` in place.
    ///
    /// Handles the three export forms: named (`@export foo`), re-export
    /// (`@export foo from "./bar"`), and wildcard (`@export * from "./bar"`).
    fn collect_export(
        &mut self,
        export: &ExportDirective,
        defs: &mut CollectedDefs,
        ctx: &ModuleCtx<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<(), MdsError> {
        defs.has_explicit_exports = true;
        match export {
            ExportDirective::Named { name, .. } => {
                defs.explicit_exports.insert(name.clone());
            }
            ExportDirective::ReExport {
                name,
                path: import_path,
                offset,
            } => {
                // Resolve the source module and bring in the function for
                // re-export only. Per spec: "@export from does not bring the
                // symbol into the current file's scope".
                // Note: resolve_import_from calls validate_import_path internally,
                // so path validation errors surface with correct messages automatically.
                let source_module = self
                    .resolve_import_from(ctx, import_path, Some(ctx.import_site(*offset)), warnings)
                    .map_err(|e| {
                        attach_import_span(e, import_path, ctx.file_str, ctx.source, *offset)
                    })?;
                let func = source_module.get_export(name).ok_or_else(|| {
                    MdsError::export_error(format!(
                        "cannot re-export '{name}': not exported from \"{import_path}\""
                    ))
                })?;
                defs.functions.insert(name.clone(), func);
                defs.explicit_exports.insert(name.clone());
            }
            ExportDirective::Wildcard {
                path: import_path,
                offset,
            } => {
                // Re-export all exports from the target module. These are
                // available to importers but NOT in the current file's scope.
                // Note: resolve_import_from calls validate_import_path internally,
                // so path validation errors surface with correct messages automatically.
                let source_module = self
                    .resolve_import_from(ctx, import_path, Some(ctx.import_site(*offset)), warnings)
                    .map_err(|e| {
                        attach_import_span(e, import_path, ctx.file_str, ctx.source, *offset)
                    })?;
                let line_len = line_len_at(ctx.source, *offset);
                for (name, func) in source_module.get_all_exports() {
                    if defs.functions.contains_key(&name) {
                        return Err(MdsError::name_collision_at(
                            &name,
                            ctx.file_str,
                            ctx.source,
                            *offset,
                            line_len,
                        ));
                    }
                    defs.functions.insert(name.clone(), func);
                    defs.explicit_exports.insert(name);
                }
            }
        }
        Ok(())
    }

    fn resolve_alias_import(
        &mut self,
        path: &str,
        alias: &str,
        offset: usize,
        scope: &mut Scope,
        ctx: &ModuleCtx<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<(), MdsError> {
        if scope.get_namespace(alias).is_some() {
            return Err(MdsError::name_collision_at(
                alias,
                ctx.file_str,
                ctx.source,
                offset,
                alias.len(),
            ));
        }
        let resolved = self
            .resolve_import_from(ctx, path, Some(ctx.import_site(offset)), warnings)
            .map_err(|e| attach_import_span(e, path, ctx.file_str, ctx.source, offset))?;
        scope.set_namespace(alias, resolved.shared_namespace());
        Ok(())
    }

    /// Resolve all frontmatter imports, populating `scope` in the same order
    /// as the declarations. Frontmatter imports run before body imports.
    pub(super) fn resolve_frontmatter_imports(
        &mut self,
        imports: &[FrontmatterImport],
        scope: &mut Scope,
        ctx: &ModuleCtx<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<(), MdsError> {
        for (i, imp) in imports.iter().enumerate() {
            match imp {
                FrontmatterImport::Alias { path, alias } => {
                    if scope.get_namespace(alias).is_some() {
                        return Err(MdsError::name_collision(alias.to_string()));
                    }
                    let resolved = self
                        .resolve_import_from(ctx, path, None, warnings)
                        .map_err(|e| attach_frontmatter_index(e, i))?;
                    scope.set_namespace(alias, resolved.shared_namespace());
                }
                FrontmatterImport::Merge { path } => {
                    let resolved = self
                        .resolve_import_from(ctx, path, None, warnings)
                        .map_err(|e| attach_frontmatter_index(e, i))?;
                    for (name, func) in resolved.get_all_exports() {
                        if scope.get_function(&name).is_some() {
                            return Err(MdsError::name_collision(name));
                        }
                        scope.set_function(&name, func);
                    }
                    self.import_prompt(&resolved, scope);
                }
                FrontmatterImport::Selective { path, names } => {
                    let resolved = self
                        .resolve_import_from(ctx, path, None, warnings)
                        .map_err(|e| attach_frontmatter_index(e, i))?;
                    let not_exported = |name: &str| {
                        MdsError::import_error(format!(
                            "'{name}' is not exported from '{path}' (in frontmatter imports[{i}])"
                        ))
                    };
                    for name in names {
                        if name == "prompt" {
                            if !self.import_prompt(&resolved, scope) {
                                return Err(not_exported(name));
                            }
                        } else {
                            scope.set_function(
                                name,
                                resolved
                                    .get_export(name)
                                    .ok_or_else(|| not_exported(name))?,
                            );
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn resolve_merge_import(
        &mut self,
        path: &str,
        offset: usize,
        scope: &mut Scope,
        ctx: &ModuleCtx<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<(), MdsError> {
        let resolved = self
            .resolve_import_from(ctx, path, Some(ctx.import_site(offset)), warnings)
            .map_err(|e| attach_import_span(e, path, ctx.file_str, ctx.source, offset))?;
        // Per spec: only functions and the prompt body are imported via merge.
        // Frontmatter variables from the imported module are NOT brought into scope.
        let line_len = line_len_at(ctx.source, offset);
        for (name, func) in resolved.get_all_exports() {
            if scope.get_function(&name).is_some() {
                return Err(MdsError::name_collision_at(
                    &name,
                    ctx.file_str,
                    ctx.source,
                    offset,
                    line_len,
                ));
            }
            scope.set_function(&name, func);
        }
        self.import_prompt(&resolved, scope);
        Ok(())
    }

    fn resolve_selective_import(
        &mut self,
        names: &[String],
        path: &str,
        offset: usize,
        scope: &mut Scope,
        ctx: &ModuleCtx<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<(), MdsError> {
        let resolved = self
            .resolve_import_from(ctx, path, Some(ctx.import_site(offset)), warnings)
            .map_err(|e| attach_import_span(e, path, ctx.file_str, ctx.source, offset))?;
        let line_len = line_len_at(ctx.source, offset);
        let not_exported = |name: &str| {
            MdsError::import_error_at(
                format!("'{name}' is not exported from '{path}'"),
                ctx.file_str,
                ctx.source,
                offset,
                line_len,
            )
        };
        for name in names {
            if name == "prompt" {
                if !self.import_prompt(&resolved, scope) {
                    return Err(not_exported(name));
                }
            } else {
                scope.set_function(
                    name,
                    resolved
                        .get_export(name)
                        .ok_or_else(|| not_exported(name))?,
                );
            }
        }
        Ok(())
    }

    fn resolve_import(
        &mut self,
        import: &ImportDirective,
        scope: &mut Scope,
        ctx: &ModuleCtx<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<(), MdsError> {
        match import {
            ImportDirective::Alias {
                path,
                alias,
                offset,
            } => self.resolve_alias_import(path, alias, *offset, scope, ctx, warnings),
            ImportDirective::Merge { path, offset } => {
                self.resolve_merge_import(path, *offset, scope, ctx, warnings)
            }
            ImportDirective::Selective {
                names,
                path,
                offset,
                ..
            } => self.resolve_selective_import(names, path, *offset, scope, ctx, warnings),
        }
    }
}
