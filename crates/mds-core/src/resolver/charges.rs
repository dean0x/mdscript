//! Module budget charges and per-module limits.

use super::{
    parse_with_ctx, tokenize, tree_estimate, Arc, EvalBudget, MdsError, Module, ModuleCache,
    ModuleCtx, ModuleMark, Part, ResolvedModule, VarLayer, MAX_FILE_SIZE, MAX_IMPORT_DEPTH,
    MAX_MODULE_COUNT, SOURCE_LABEL,
};
#[cfg(test)]
use super::{tests, ModuleProbe};

use crate::limits::{MAX_DATA_BYTES_PER_COMPILE, MAX_DATA_ENTRIES_PER_COMPILE, MAX_DATA_FILES};

pub(super) fn count_data_entries(total: &mut usize, count: usize) -> Result<(), MdsError> {
    let Some(next @ ..=MAX_DATA_ENTRIES_PER_COMPILE) = total.checked_add(count) else {
        return Err(MdsError::resource_limit(format!(
            "too many data entries (max {MAX_DATA_ENTRIES_PER_COMPILE} per compile)"
        )));
    };
    *total = next;
    Ok(())
}

pub(super) fn check_data_file_count(count: usize) -> Result<(), MdsError> {
    if count >= MAX_DATA_FILES {
        return Err(MdsError::resource_limit(format!(
            "too many data files (max {MAX_DATA_FILES} per compile)"
        )));
    }
    Ok(())
}

pub(super) fn count_data_bytes(total: &mut usize, count: usize) -> Result<(), MdsError> {
    let Some(next @ ..=MAX_DATA_BYTES_PER_COMPILE) = total.checked_add(count) else {
        return Err(MdsError::resource_limit(format!(
            "data files exceed {MAX_DATA_BYTES_PER_COMPILE} bytes per compile"
        )));
    };
    *total = next;
    Ok(())
}

impl ModuleCache {
    /// Enforce `MAX_FILE_SIZE` on a string entry source at the core string funnels.
    ///
    /// The binding layers (WASM/napi) and `NativeFs::read` carry their own size guards
    /// with their own messages, but `compile_str`/`check_str`/`lint_str_with` and the
    /// opts variants reach the resolver as an in-memory `&str` that never passes those
    /// guards (PF-004). The message shape mirrors `fs.rs` so callers matching on
    /// "too large" behave identically across paths.
    pub(super) fn check_source_size(source: &str) -> Result<(), MdsError> {
        if source.len() as u64 > MAX_FILE_SIZE {
            return Err(MdsError::resource_limit(format!(
                "file too large ({} bytes, max {} bytes): {SOURCE_LABEL}",
                source.len(),
                MAX_FILE_SIZE,
            )));
        }
        Ok(())
    }

    /// Start a new compile budget only at the root of resolution. The root then
    /// charges the compiler's copy of the caller variables once, as buffers: it
    /// stays out of `retained` and out of every module mark.
    fn start_resolution(&mut self, key: &str, runtime: &VarLayer) -> Result<usize, MdsError> {
        let mut layer = Ok(0);
        if self.resolving.is_empty() {
            self.budget = EvalBudget::default();
            self.data.entries = 0;
            self.kept = 0;
            self.commit_retained(0);
            #[cfg(test)]
            {
                self.module_probes.clear();
                self.budget.seed_function_calls(self.function_call_seed);
                let seed = self.temporaries_seed + usize::from(key == tests::TEMPORARIES_SEED_KEY);
                if seed != 0 {
                    self.budget.mem.charge(Part::Temp, Some(seed)).unwrap();
                }
            }
            layer = self.budget.mem.charge(Part::Buffers, runtime.bytes());
        }
        self.resolving.insert(key.to_string());
        layer
    }

    /// Open the module mark. A refused variable charge skips the read charge.
    fn open_module(
        &mut self,
        layer: Result<usize, MdsError>,
        read_bytes: usize,
    ) -> (ModuleMark, Result<(), MdsError>) {
        let mark = ModuleMark {
            temporaries: self.budget.mem.mark(),
            outer_kept: std::mem::take(&mut self.kept),
            #[cfg(test)]
            probe: self.module_probes.len(),
        };
        let charged = layer.and_then(|_| {
            if read_bytes == 0 {
                Ok(0)
            } else {
                self.budget.mem.charge(Part::Temp, Some(read_bytes))
            }
        });
        #[cfg(test)]
        self.module_probes.push(ModuleProbe {
            buffers_before: self.budget.mem.buffers,
            temporaries_before: mark.temporaries,
            temporaries_open: self.budget.mem.mark(),
            ..ModuleProbe::default()
        });
        (mark, charged.map(drop))
    }

    pub(super) fn close_module(&mut self, mark: ModuleMark) -> usize {
        self.budget.mem.release_to(mark.temporaries);
        #[cfg(test)]
        {
            self.module_probes[mark.probe].temporaries_closed = self.budget.mem.mark();
        }
        std::mem::replace(&mut self.kept, mark.outer_kept)
    }

    /// Steps 1 to 3 of the module order: the root reset and the variable charge,
    /// the module mark and the read charge. One out-of-line copy serves every
    /// resolve function.
    #[inline(never)]
    pub(super) fn begin_module(
        &mut self,
        key: &str,
        runtime: &VarLayer,
        read_bytes: usize,
    ) -> (ModuleMark, Result<(), MdsError>) {
        let layer = self.start_resolution(key, runtime);
        self.open_module(layer, read_bytes)
    }

    /// Tokenize and parse the module source. The tree estimate is charged as a
    /// temporary between the two steps, so a refused tree is never built. With
    /// `keep`, the module entry keeps the charge; else it stays a temporary until
    /// the module closes. The tokens drop here, after the parse.
    #[inline(never)]
    pub(super) fn parse_charged(
        &mut self,
        ctx: &ModuleCtx<'_>,
        keep: bool,
    ) -> Result<Module, MdsError> {
        let tokens = tokenize(ctx.source, ctx.file_str)?;
        let tree = self
            .budget
            .mem
            .charge(Part::Temp, tree_estimate(&tokens, ctx.source.len()))?;
        if keep {
            self.keep(tree);
        }
        parse_with_ctx(&tokens, ctx.file_str, ctx.source)
    }

    pub(super) fn charge_kept(&mut self, n: usize) -> Result<(), MdsError> {
        self.budget.mem.charge(Part::Buffers, Some(n))?;
        // The kept sum is part of buffers, which charge already checked.
        self.kept += n;
        Ok(())
    }

    /// Move `n` bytes charged as a temporary to the kept sum, because the module
    /// entry keeps the value they count.
    pub(super) fn keep(&mut self, n: usize) {
        self.budget.mem.transfer(Part::Temp, Part::Buffers, n);
        self.kept += n;
    }

    pub(super) fn retain_module(&mut self, key: &str, module: &Arc<ResolvedModule>, kept: usize) {
        // Do not publish an entry from a failed counter check.
        if self.budget.mem.underflow || (self.resolving.is_empty() && self.budget.mem.mark() != 0) {
            return;
        }
        self.commit_retained(kept);
        if !self.budget.mem.underflow {
            self.modules.insert(key.to_string(), Arc::clone(module));
        }
    }

    pub(super) fn copy_raw_frontmatter(
        &mut self,
        module: &crate::ast::Module,
    ) -> Result<Option<String>, MdsError> {
        let Some(fm) = &module.frontmatter else {
            return Ok(None);
        };
        self.charge_kept(fm.raw.len())?;
        Ok(Some(fm.raw.clone()))
    }

    /// Guard against excessively deep import chains.
    pub(super) fn check_import_depth(&self) -> Result<(), MdsError> {
        if self.resolving.len() >= MAX_IMPORT_DEPTH {
            return Err(MdsError::import_error(format!(
                "import depth exceeds maximum of {MAX_IMPORT_DEPTH} (possible deep chain)"
            )));
        }
        Ok(())
    }

    /// Permit the entry plus at most `MAX_MODULE_COUNT` other distinct modules.
    ///
    /// Count cached keys, keys still being resolved, and the candidate before any
    /// read. A cached skeleton can also be on the resolving stack during an
    /// upgrade; count that key once. Binding input guards separately bound the
    /// supplied virtual map, including unused entries.
    pub(super) fn check_module_count(&self, key: &str) -> Result<(), MdsError> {
        let distinct = self.modules.len()
            + self
                .resolving
                .iter()
                .filter(|key| !self.modules.contains_key(key.as_str()))
                .count()
            + usize::from(!self.modules.contains_key(key) && !self.resolving.contains(key));
        if distinct > MAX_MODULE_COUNT + 1 {
            return Err(crate::error::module_count_exceeded());
        }
        Ok(())
    }
}
