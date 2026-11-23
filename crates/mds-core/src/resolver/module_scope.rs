//! Definition collection, scope construction, and export validation.

use super::{
    has_type_mds_frontmatter_raw, parse_frontmatter_imports_from_yaml, Arc, BlockNode,
    CollectedDefs, DefineBlock, FrontmatterImport, FunctionDef, HashMap, HashSet, MdsError,
    MemBudget, ModuleCache, ModuleCtx, Node, Part, Scope, Value, VarLayer, INDENTED_DIRECTIVES,
    MAX_BLOCKS_PER_MODULE, MAX_DEFINES_PER_MODULE,
};

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
    let mut frontmatter_vars = VarLayer::new();
    let mut fm_imports: Vec<FrontmatterImport> = Vec::new();

    // A .mds file is always MDS; a .md file is MDS only when its frontmatter
    // contains `type: mds`. Determine this early — it gates both frontmatter
    // parsing and the runtime_vars guard below.
    let is_mds = !is_md || frontmatter.is_some_and(|fm| has_type_mds_frontmatter_raw(&fm.raw));

    mem.charge(Part::Temp, Some(spent))?;
    for (key, val) in mapping.into_iter().flatten() {
        let serde_yaml_ng::Value::String(key_str) = key else {
            continue;
        };
        // SYNC POINT: audit the output, merge and lint reserved keys.
        if key_str == INDENTED_DIRECTIVES || (key_str == "type" && is_md) {
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

    Ok((
        Scope::with_layers(Arc::clone(runtime_vars), Arc::new(frontmatter_vars)),
        fm_imports,
    ))
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
