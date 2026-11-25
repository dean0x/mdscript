//! Definition collection, scope construction, and export validation.

use super::charges::{check_data_file_count, count_data_bytes, count_data_entries};
use super::{
    has_type_mds_frontmatter_raw, parse_frontmatter_imports_from_yaml, Arc, BlockNode,
    CollectedDefs, DefineBlock, FrontmatterImport, FunctionDef, HashMap, HashSet, MdsError,
    MemBudget, Module, ModuleCache, ModuleCtx, Node, Part, Scope, Value, VarLayer,
    INDENTED_DIRECTIVES, MAX_BLOCKS_PER_MODULE, MAX_DEFINES_PER_MODULE,
};
use crate::data::{decl::Source, resolve, step, DataLabel};

/// Return `true` when the AST body contains at least one `@message` block
/// (possibly nested inside `@if` or `@for` — a shallow scan is enough for the
/// "no messages at all" guard; evaluation handles deeper nesting).
pub(super) fn has_message_block(nodes: &[Node]) -> bool {
    nodes.iter().any(|n| match n {
        Node::Message(_) => true,
        Node::If(block) => {
            has_message_block(&block.then_body)
                || block
                    .elseif_branches
                    .iter()
                    .any(|branch| has_message_block(&branch.body))
                || block
                    .else_body
                    .as_deref()
                    .map(has_message_block)
                    .unwrap_or(false)
        }
        Node::For(block) => has_message_block(&block.body),
        // A @block's default body may contain @message blocks.
        Node::Block(block) => has_message_block(&block.body),
        // Leaf nodes that can never contain @message blocks. Enumerated
        // explicitly so a future new Node variant fails to compile here
        // and at the sibling match in evaluate_messages_intrinsic, rather
        // than silently being misclassified as markdown.
        Node::Text(_)
        | Node::Interpolation(_)
        | Node::EscapedBrace { .. }
        | Node::Define(_)
        | Node::Import(_)
        | Node::Export(_)
        | Node::Include(_) => false,
    })
}

/// Count declarations in every body, even when a branch does not run.
/// The parser bounds the depth of these bodies before collection.
pub(super) fn count_module_defines(node: &Node, count: &mut usize) -> Result<(), MdsError> {
    let body = match node {
        Node::Define(def) => {
            if *count == MAX_DEFINES_PER_MODULE {
                return Err(MdsError::resource_limit(format!(
                    "module has more than {MAX_DEFINES_PER_MODULE} @define declarations"
                )));
            }
            *count += 1;
            &def.body
        }
        Node::If(block) => {
            for child in block
                .then_body
                .iter()
                .chain(block.elseif_branches.iter().flat_map(|branch| &branch.body))
                .chain(block.else_body.iter().flatten())
            {
                count_module_defines(child, count)?;
            }
            return Ok(());
        }
        Node::For(block) => &block.body,
        Node::Block(block) => &block.body,
        Node::Message(block) => &block.body,
        Node::Text(_)
        | Node::Interpolation(_)
        | Node::EscapedBrace { .. }
        | Node::Import(_)
        | Node::Export(_)
        | Node::Include(_) => return Ok(()),
    };
    body.iter()
        .try_for_each(|child| count_module_defines(child, count))
}

/// Process a single `@define` directive, updating `defs` and `scope` in place.
///
/// Captures the definition-site scope for lexical closure semantics so the
/// function body can resolve alias imports, sibling functions, and frontmatter
/// variables from its defining module even when called from a different module.
pub(super) fn collect_define(
    def: &DefineBlock,
    defs: &mut CollectedDefs,
    scope: &mut Scope,
    ctx: &ModuleCtx<'_>,
    cache: &mut ModuleCache,
) -> Result<(), MdsError> {
    if defs.functions.contains_key(&def.name) || defs.block_names.contains(&def.name) {
        return Err(MdsError::name_collision_at(
            &def.name,
            ctx.file_str,
            ctx.source,
            def.offset,
            def.name.len(),
        ));
    }
    let mut func = FunctionDef::from(def);
    // Freeze the definition site before inserting this function into scope.
    func.captured = scope.capture()?;
    func.origin = Some(ctx.origin(cache)?.clone());
    // Wrap in Arc for cheap storage and O(1) scope insertion.
    let arc = Arc::new(func);
    defs.functions.insert(def.name.clone(), Arc::clone(&arc));
    scope.set_function(&def.name, arc);
    Ok(())
}

/// Check a single `@block` directive and add its name to `defs.block_names`.
///
/// Detects:
/// - Duplicate `@block` names within the same module (mds::name_collision).
/// - `@block` name colliding with a `@define` function name (mds::name_collision).
///   This is intentional: blocks and functions share the same namespace (decision #10).
/// - `MAX_BLOCKS_PER_MODULE` cap (mds::resource_limit).
///
/// Note: `count` is the running total of @block nodes seen so far (1-indexed after increment
/// at the call site), used to enforce the per-module cap.
pub(super) fn collect_block(
    block: &BlockNode,
    defs: &mut CollectedDefs,
    count: usize,
    ctx: &ModuleCtx<'_>,
) -> Result<(), MdsError> {
    if count > MAX_BLOCKS_PER_MODULE {
        return Err(MdsError::resource_limit(format!(
            "module has more than {MAX_BLOCKS_PER_MODULE} @block declarations"
        )));
    }
    // Check for duplicate @block name or collision with an existing @define.
    if defs.block_names.contains(&block.name) || defs.functions.contains_key(&block.name) {
        return Err(MdsError::name_collision_at(
            &block.name,
            ctx.file_str,
            ctx.source,
            block.offset,
            block.name.len(),
        ));
    }
    defs.block_names.insert(block.name.clone());
    Ok(())
}

/// Make one runtime layer to share across the modules in a resolution call.
pub(super) fn runtime_layer(vars: &HashMap<String, Value>) -> Arc<VarLayer> {
    Arc::new(
        vars.iter()
            .map(|(name, value)| (name.clone(), Arc::new(value.clone())))
            .collect(),
    )
}

/// Build a scope with runtime variables above frontmatter variables, from the
/// module's one parsed `mapping`. `spent` is the bytes its parse spent: the layer
/// is charged with them as a temporary before the conversion, because the
/// converted values are never larger than their tree.
/// A Markdown file uses `type` as a file marker, not a variable.
/// Return `imports` separately for MDS files. Plain Markdown keeps it as a variable.
pub(super) fn build_scope_from_frontmatter(
    frontmatter: Option<&crate::ast::Frontmatter>,
    mapping: Option<&serde_yaml_ng::Mapping>,
    spent: usize,
    is_md: bool,
    runtime_vars: &Arc<VarLayer>,
    mem: &mut MemBudget,
) -> Result<(Scope, Vec<FrontmatterImport>), MdsError> {
    let is_mds = uses_mds_scope(is_md, frontmatter);
    let (frontmatter_vars, fm_imports) =
        frontmatter_layer(mapping, spent, is_md, is_mds, runtime_vars, mem)?;
    Ok((
        Scope::with_layers(Arc::clone(runtime_vars), Arc::new(frontmatter_vars)),
        fm_imports,
    ))
}

fn uses_mds_scope(is_md: bool, frontmatter: Option<&crate::ast::Frontmatter>) -> bool {
    !is_md || frontmatter.is_some_and(|fm| has_type_mds_frontmatter_raw(&fm.raw))
}

fn frontmatter_layer(
    mapping: Option<&serde_yaml_ng::Mapping>,
    spent: usize,
    is_md: bool,
    is_mds: bool,
    runtime_vars: &VarLayer,
    mem: &mut MemBudget,
) -> Result<(VarLayer, Vec<FrontmatterImport>), MdsError> {
    let mut frontmatter_vars = VarLayer::new();
    let mut fm_imports: Vec<FrontmatterImport> = Vec::new();

    mem.charge(Part::Temp, Some(spent))?;
    for (key, val) in mapping.into_iter().flatten() {
        let serde_yaml_ng::Value::String(key_str) = key else {
            continue;
        };
        // SYNC POINT: audit the output, merge and lint reserved keys.
        if key_str == INDENTED_DIRECTIVES || (key_str == "type" && is_md) {
            continue;
        }
        if key_str == "data" && is_mds {
            continue;
        }
        if key_str == "imports" && is_mds {
            // Parse as structured import declarations, not a scope variable.
            // Plain .md keeps `imports` as a regular variable.
            fm_imports = parse_frontmatter_imports_from_yaml(val)?;
            continue;
        }
        let value = Value::from_yaml_ref(val)?;
        frontmatter_vars.insert(key_str.clone(), Arc::new(value));
    }

    if is_mds && runtime_vars.contains_key("imports") {
        return Err(MdsError::import_error(
            "'imports' is a reserved frontmatter key for MDS files and cannot be set \
             via --set",
        ));
    }

    Ok((frontmatter_vars, fm_imports))
}

impl ModuleCache {
    /// Bind the module's own entries before it can capture or use the scope.
    pub(super) fn build_scope_from_frontmatter(
        &mut self,
        module: &Module,
        mapping: Option<&serde_yaml_ng::Mapping>,
        spent: usize,
        is_md: bool,
        ctx: &ModuleCtx<'_>,
    ) -> Result<(Scope, Vec<FrontmatterImport>, resolve::Chain), MdsError> {
        let is_mds = uses_mds_scope(is_md, module.frontmatter.as_ref());
        let (mut vars, imports) = frontmatter_layer(
            mapping,
            spent,
            is_md,
            is_mds,
            ctx.runtime_vars,
            &mut self.budget.mem,
        )?;
        let data = if is_mds {
            self.extend_data_chain(&None, ctx, is_md, mapping, &imports, &module.body)?
        } else {
            None
        };
        let (names, pending) =
            self.bind_data(&data, mapping, is_md, ctx.runtime_vars, &mut vars)?;
        let mut scope = Scope::with_layers(Arc::clone(ctx.runtime_vars), Arc::new(vars));
        scope.set_data_names(names);
        scope.set_pending_data(pending);
        Ok((scope, imports, data))
    }

    /// Keep the declaring location even when a cached module becomes a base.
    pub(super) fn extend_data_chain(
        &mut self,
        base: &resolve::Chain,
        ctx: &ModuleCtx<'_>,
        is_md: bool,
        mapping: Option<&serde_yaml_ng::Mapping>,
        imports: &[FrontmatterImport],
        body: &[Node],
    ) -> Result<resolve::Chain, MdsError> {
        let mem = &mut self.budget.mem;
        let mark = mem.mark();
        let data = resolve::Declarations::extend(
            base,
            resolve::DeclarationSite {
                key: ctx.key,
                module: ctx.file_str,
                dir: ctx.base_dir,
                is_md,
                preflight: false,
            },
            mapping,
            imports,
            body,
            mem,
        )?;
        self.keep(self.budget.mem.mark() - mark);
        Ok(data)
    }

    /// Bind the winning entries of `chain` into `vars`. Return the names of all
    /// winning entries, replayed entries included, and the names that stay pending.
    pub(super) fn bind_data(
        &mut self,
        chain: &resolve::Chain,
        mapping: Option<&serde_yaml_ng::Mapping>,
        is_md: bool,
        runtime: &VarLayer,
        vars: &mut VarLayer,
    ) -> Result<(HashSet<String>, HashSet<String>), MdsError> {
        if !resolve::Declarations::contains_data(chain) {
            return Ok(Default::default());
        }
        // No data is read unless the whole graph passed its check.
        if let Some(refusal) = &self.graph_refusal {
            return Err(refusal.clone());
        }
        let before = self.budget.mem.buffers;
        let fs = &*self.fs;
        let data = &mut self.data;
        let analyze_data = self.analyze_data;
        // The module that the compile starts from: only its entries can send a secret.
        let entry_key = self.resolving.first().map(String::as_str);
        let result = resolve::Declarations::with_graph(
            chain,
            resolve::FrontmatterNames::Parsed { mapping, is_md },
            runtime,
            &mut self.budget.mem,
            |count, graph, mem| {
                count_data_entries(&mut data.entries, count)?;
                // The role check reads these names, so a replayed entry keeps its source.
                // Replay does not lift the secret header rule, so check every URL entry.
                let mut names = HashSet::new();
                for node in graph.nodes() {
                    if let Source::Url(template) = &node.entry.source {
                        step::check_secret_module(
                            node,
                            template,
                            Some(node.position.0) == entry_key,
                        )?;
                    }
                    let name = &node.entry.name;
                    mem.charge(
                        Part::Buffers,
                        name.len().checked_add(2 * size_of::<String>()),
                    )?;
                    names.insert(name.clone());
                }
                let mut pending = HashSet::new();
                if !data.compile {
                    for node in graph.unreplayed() {
                        if !analyze_data {
                            return Err(MdsError::undefined_var(&node.entry.name));
                        }
                        if !data.pending {
                            return Err(MdsError::internal());
                        }
                        let name = &node.entry.name;
                        mem.charge(
                            Part::Buffers,
                            name.len().checked_add(2 * size_of::<String>()),
                        )?;
                        pending.insert(name.clone());
                    }
                    return Ok((names, pending));
                }
                // Only a step compile has a context for its URL entries.
                if graph.frontier().next().is_some() && data.net.is_none() {
                    return Err(MdsError::internal());
                }
                for node in graph.in_level_order() {
                    let written = match &node.entry.source {
                        Source::File(written) => written,
                        Source::Url(template) => {
                            let ctx = data.net.as_mut().ok_or_else(MdsError::internal)?;
                            let scope = step::EntryScope {
                                runtime,
                                vars,
                                pending: &pending,
                                entry_module: Some(node.position.0) == entry_key,
                            };
                            let bound =
                                step::bind_url_entry(node, template, &scope, ctx, mem, |len| {
                                    count_data_bytes(&mut data.bytes, len)
                                })?;
                            let name = &node.entry.name;
                            match bound {
                                step::Bound::Value(value) => {
                                    bind_entry(name, value, vars, mem)?;
                                }
                                step::Bound::Pending => {
                                    mem.charge(
                                        Part::Buffers,
                                        name.len().checked_add(2 * size_of::<String>()),
                                    )?;
                                    pending.insert(name.clone());
                                    data.pending = true;
                                }
                            }
                            continue;
                        }
                    };
                    let label = DataLabel {
                        module: node.module,
                        entry: &node.entry.name,
                        input: crate::data::DataInput::Declaration,
                    };
                    let (key, accept) = resolve::canonical_file(fs, node.dir, written, label)?;
                    let value = if let Some(value) = data.files.get(&key) {
                        Arc::clone(value)
                    } else {
                        check_data_file_count(data.files.len())?;
                        let metadata = mem.charge(Part::Temp, data_memo_bytes(key.capacity()))?;
                        let value = resolve::read_file(fs, &key, accept, label, mem, |len| {
                            count_data_bytes(&mut data.bytes, len)
                        })?;
                        mem.transfer(Part::Temp, Part::Buffers, metadata);
                        let value = Arc::new(value);
                        data.files.insert(key, Arc::clone(&value));
                        value
                    };
                    bind_entry(&node.entry.name, value, vars, mem)?;
                }
                Ok((names, pending))
            },
        );
        self.commit_retained(self.budget.mem.buffers - before);
        result.map(Option::unwrap_or_default)
    }
}

/// Bind the value of the entry `name` into the module variables.
fn bind_entry(
    name: &str,
    value: Arc<Value>,
    vars: &mut VarLayer,
    mem: &mut MemBudget,
) -> Result<(), MdsError> {
    let binding_bytes = name
        .len()
        .checked_add(2 * size_of::<(String, crate::scope::Slot)>());
    mem.charge(Part::Buffers, binding_bytes)?;
    vars.insert(name.to_owned(), value);
    Ok(())
}

fn data_memo_bytes(key: usize) -> Option<usize> {
    key.checked_add(2 * size_of::<(String, Arc<Value>, usize)>())?
        .checked_add(2 * size_of::<usize>())
}

/// Check that each named export refers to a defined function or the special `"prompt"` export.
pub(super) fn validate_exports(
    explicit_exports: &HashSet<String>,
    functions: &HashMap<String, Arc<FunctionDef>>,
) -> Result<(), MdsError> {
    for name in explicit_exports {
        if name != "prompt" && !functions.contains_key(name) {
            return Err(MdsError::export_error(format!(
                "cannot export '{name}': not defined in this module"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileSystem, VirtualFs};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn plain_markdown_keeps_data_as_a_frontmatter_variable() {
        let yaml = "data:\n  items: ordinary\n";
        let mapping = super::super::parse_frontmatter_yaml(yaml).unwrap();
        let (scope, imports) = build_scope_from_frontmatter(
            None,
            mapping.as_mapping(),
            4096,
            true,
            &Arc::new(VarLayer::new()),
            &mut MemBudget::default(),
        )
        .unwrap();
        assert!(imports.is_empty());
        assert_eq!(
            scope.get_var("data"),
            Some(&Value::Object(HashMap::from([(
                "items".into(),
                Value::from("ordinary")
            ),])))
        );
    }

    /// Each reserved output key has a decision for an `.mds` module scope, so a new
    /// key in the shared table fails here until this site decides it.
    /// Control: an ordinary key stays a frontmatter variable.
    #[test]
    fn mds_module_scope_decides_every_reserved_output_key() {
        for key in crate::RESERVED_OUTPUT_KEYS {
            // Each arm gives a value of the key's YAML shape, and states if the key
            // stays a scope variable in an `.mds` module.
            let (value, stays_variable) = match key.name {
                // Only a `.md` file uses `type` as a file marker.
                // An `.mds` file keeps it as an ordinary variable.
                "type" => ("mds", true),
                // The declarations go to the import list of the module.
                "imports" => ("\n  - path: ./other.mds\n    as: other", false),
                // The entries bind later as data, not as one variable.
                "data" => ("\n  item: ./item.json", false),
                // A lexer setting, never a variable.
                crate::lexer::INDENTED_DIRECTIVES => ("true", false),
                name => panic!("decide if `{name}` is a scope variable in an .mds module"),
            };
            let yaml = format!("{}: {value}\ninfo: ordinary\n", key.name);
            let mapping = super::super::parse_frontmatter_yaml(&yaml).unwrap();
            let (scope, _) = build_scope_from_frontmatter(
                None,
                mapping.as_mapping(),
                4096,
                false,
                &Arc::new(VarLayer::new()),
                &mut MemBudget::default(),
            )
            .unwrap();
            assert_eq!(
                scope.get_var(key.name).is_some(),
                stays_variable,
                "{}",
                key.name
            );
            assert_eq!(
                scope.get_var("info"),
                Some(&Value::from("ordinary")),
                "{}",
                key.name
            );
        }
    }

    struct CountReads {
        fs: VirtualFs,
        data_reads: Arc<AtomicUsize>,
        module_reads: Arc<AtomicUsize>,
    }

    impl FileSystem for CountReads {
        fn resolve_entry(&self, path: &str) -> Result<String, MdsError> {
            self.fs.resolve_entry(path)
        }
        fn normalize_in_dir(&self, dir: &str, relative: &str) -> Result<String, MdsError> {
            self.fs.normalize_in_dir(dir, relative)
        }
        fn parent_dir(&self, key: &str) -> String {
            self.fs.parent_dir(key)
        }
        fn read(&self, key: &str) -> Result<String, MdsError> {
            self.module_reads.fetch_add(1, Ordering::Relaxed);
            self.fs.read(key)
        }
        fn read_data(&self, key: &str) -> Result<String, MdsError> {
            self.data_reads.fetch_add(1, Ordering::Relaxed);
            self.fs.read_data(key)
        }
        fn is_markdown(&self, key: &str) -> bool {
            self.fs.is_markdown(key)
        }
    }

    #[test]
    fn one_canonical_file_is_read_once_across_three_modules() {
        let declarations = "---\ndata:\n  item: ./item.json\n---\n";
        let main = format!("{declarations}@import \"./first.mds\" as first\n@import \"./second.mds\" as second\n@include first\n@include second\n{{{{item}}}}\n");
        let first = format!("{declarations}{{{{item}}}}\n");
        let second = first.replace("./item.json", "./alias.json");
        let fs = VirtualFs::new(HashMap::from([
            ("main.mds".into(), main),
            ("first.mds".into(), first),
            ("second.mds".into(), second),
            ("item.json".into(), "42".into()),
        ]))
        .with_aliases(HashMap::from([("alias.json".into(), "item.json".into())]))
        .unwrap();
        let reads = Arc::new(AtomicUsize::new(0));
        let module_reads = Arc::new(AtomicUsize::new(0));
        let mut cache = ModuleCache::compiling(Box::new(CountReads {
            fs,
            data_reads: Arc::clone(&reads),
            module_reads: Arc::clone(&module_reads),
        }));
        let out = cache
            .resolve_virtual_intrinsic("main.mds", &HashMap::new(), &mut vec![])
            .unwrap();
        assert!(
            matches!(out, crate::CompiledOutput::Markdown(text) if text.ends_with("42\n42\n42\n"))
        );
        assert_eq!(reads.load(Ordering::Relaxed), 1);
        assert_eq!(module_reads.load(Ordering::Relaxed), 3);
        assert_eq!(cache.data.bytes, 2);
    }

    /// The entry cap binds first in a real compile, so the memo starts with files
    /// that earlier reads would have left. A file it already holds needs no new slot.
    #[test]
    fn data_file_cap_refuses_a_new_file_past_the_cap() {
        let source = "---\ndata:\n  held: ./held.json\n  a: ./a.json\n  b: ./b.json\n---\n{{held}}{{a}}{{b}}\n";
        let compile = |held: usize| {
            let fs = VirtualFs::new(HashMap::from([
                ("main.mds".into(), source.into()),
                ("held.json".into(), "0".into()),
                ("a.json".into(), "1".into()),
                ("b.json".into(), "2".into()),
            ]));
            let mut cache = ModuleCache::compiling(Box::new(fs));
            cache
                .data
                .files
                .insert("held.json".into(), Arc::new(Value::Number(9.0)));
            for i in 1..held {
                let value = Arc::new(Value::Null);
                cache.data.files.insert(format!("seed{i}.json"), value);
            }
            cache.resolve_virtual_intrinsic("main.mds", &HashMap::new(), &mut vec![])
        };
        let cap = crate::limits::MAX_DATA_FILES;
        let out = compile(cap - 2).unwrap();
        assert!(matches!(out, crate::CompiledOutput::Markdown(text) if text.ends_with("912\n")));
        let err = compile(cap - 1).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("resource limit exceeded: too many data files (max {cap} per compile)")
        );
    }
}
