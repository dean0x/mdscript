use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::ast::*;
use crate::error::MdsError;
use crate::evaluator::evaluate;
use crate::lexer::tokenize;
use crate::parser::parse;
use crate::scope::{FunctionDef, NamespaceScope, Scope};
use crate::validator;
use crate::value::Value;

/// Walk up from a directory to find the project root.
/// Looks for `.git` or `.mdsroot` markers.
/// Falls back to the given directory if no marker is found.
fn find_project_root(start: &Path) -> PathBuf {
    let mut dir = start.to_path_buf();
    loop {
        for marker in &[".git", ".mdsroot"] {
            if dir.join(marker).exists() {
                return dir;
            }
        }
        if !dir.pop() {
            return start.to_path_buf();
        }
    }
}

/// A resolved module with its AST, exports, and prompt body.
#[derive(Debug, Clone)]
pub struct ResolvedModule {
    pub functions: HashMap<String, FunctionDef>,
    pub prompt_body: Option<String>,
    pub has_explicit_exports: bool,
    pub explicit_exports: HashSet<String>,
}

/// Maximum import depth to prevent stack overflow from deeply chained imports.
const MAX_IMPORT_DEPTH: usize = 64;

/// Maximum file size (10 MB) to prevent runaway memory use.
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Module cache to avoid re-resolving the same file.
pub struct ModuleCache {
    modules: HashMap<PathBuf, ResolvedModule>,
    /// Tracks modules currently being resolved (for cycle detection), O(1) lookup.
    resolving: HashSet<PathBuf>,
    /// Preserves insertion order of the resolving set for cycle path reconstruction.
    resolving_stack: Vec<PathBuf>,
    /// Root directory for path-traversal prevention (set on first resolve).
    root_dir: Option<PathBuf>,
}

impl Default for ModuleCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ModuleCache {
    pub fn new() -> Self {
        ModuleCache {
            modules: HashMap::new(),
            resolving: HashSet::new(),
            resolving_stack: Vec::new(),
            root_dir: None,
        }
    }

    /// Resolve a module from file path. Handles caching and cycle detection.
    pub fn resolve(
        &mut self,
        path: &Path,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<ResolvedModule, MdsError> {
        let canonical = path
            .canonicalize()
            .map_err(|_| MdsError::file_not_found(path.display().to_string()))?;

        // Set root_dir on first resolve (project root, not just entry point directory)
        if self.root_dir.is_none() {
            let entry_dir = canonical.parent().unwrap_or(Path::new("."));
            self.root_dir = Some(find_project_root(entry_dir));
        }

        // Check cache
        if let Some(cached) = self.modules.get(&canonical) {
            return Ok(cached.clone());
        }

        // Check for circular imports
        if self.resolving.contains(&canonical) {
            let cycle = build_cycle_string(&self.resolving_stack, &canonical);
            return Err(MdsError::CircularImport { cycle });
        }

        // Guard against excessively deep import chains
        if self.resolving.len() >= MAX_IMPORT_DEPTH {
            return Err(MdsError::import_error(format!(
                "import depth exceeds maximum of {MAX_IMPORT_DEPTH} (possible deep chain)"
            )));
        }

        // Prevent path traversal: resolved path must stay within the root directory
        if let Some(ref root) = self.root_dir {
            if !canonical.starts_with(root) {
                return Err(MdsError::import_error(format!(
                    "import path escapes project directory: {}",
                    canonical.display()
                )));
            }
        }

        // Check file size before reading
        let metadata = std::fs::metadata(&canonical).map_err(|e| MdsError::Io {
            message: format!("cannot read {}: {e}", canonical.display()),
        })?;
        if metadata.len() > MAX_FILE_SIZE {
            return Err(MdsError::Io {
                message: format!(
                    "file too large ({} bytes, max {} bytes): {}",
                    metadata.len(),
                    MAX_FILE_SIZE,
                    canonical.display()
                ),
            });
        }

        // Read source
        let source = std::fs::read_to_string(&canonical).map_err(|e| MdsError::Io {
            message: format!("cannot read {}: {e}", canonical.display()),
        })?;

        // Validate file type (uses already-read source for .md frontmatter check)
        validate_file_type(&canonical, &source)?;

        let file_str = canonical.display().to_string();
        let base_dir = canonical.parent().unwrap_or(Path::new(".")).to_path_buf();
        let is_md = canonical.extension().and_then(|e| e.to_str()) == Some("md");

        // Mark as resolving before recursing into process_module
        self.resolving.insert(canonical.clone());
        self.resolving_stack.push(canonical.clone());

        let resolved =
            self.process_module(&source, &file_str, &base_dir, is_md, runtime_vars, warnings);

        // Unmark regardless of success or failure
        self.resolving.remove(&canonical);
        self.resolving_stack.pop();

        let resolved = resolved?;

        // Cache
        self.modules.insert(canonical, resolved.clone());

        Ok(resolved)
    }

    /// Resolve a module from an in-memory source string.
    /// Imports within the source are resolved relative to `base_dir`.
    pub fn resolve_source(
        &mut self,
        source: &str,
        base_dir: &Path,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<ResolvedModule, MdsError> {
        // Set root_dir on first use so path-traversal checks work for imports
        if self.root_dir.is_none() {
            self.root_dir = Some(base_dir.to_path_buf());
        }
        self.process_module(source, "<source>", base_dir, false, runtime_vars, warnings)
    }

    /// Common module processing: tokenize, parse, build scope, evaluate.
    fn process_module(
        &mut self,
        source: &str,
        file_str: &str,
        base_dir: &Path,
        is_md: bool,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<ResolvedModule, MdsError> {
        // Tokenize and parse
        let tokens = tokenize(source, file_str)?;
        let module = parse(&tokens)?;

        // Build scope from frontmatter
        let mut scope = Scope::new();

        if let Some(ref fm) = module.frontmatter {
            let yaml_vars = parse_frontmatter(&fm.raw)?;
            for (key, value) in yaml_vars {
                if key == "type" && is_md {
                    // Skip the 'type' meta-field for .md files (it's a file-type marker)
                    continue;
                }
                scope.set_var(&key, value);
            }
        }

        // Apply runtime vars (override frontmatter)
        for (key, value) in runtime_vars {
            scope.set_var(key, value.clone());
        }

        // Collect function definitions and process imports
        let mut functions = HashMap::new();
        let mut has_explicit_exports = false;
        let mut explicit_exports = HashSet::new();

        for node in &module.body {
            match node {
                Node::Define(def) => {
                    if functions.contains_key(&def.name) {
                        return Err(MdsError::name_collision_at(
                            &def.name,
                            file_str,
                            source,
                            def.offset,
                            def.name.len(),
                        ));
                    }
                    let mut func = FunctionDef::from(def);
                    // Capture definition-site scope for lexical closure semantics so the
                    // function body can resolve alias imports, sibling functions, and
                    // frontmatter variables from its defining module even when called from
                    // a different module.
                    func.captured_namespaces = scope.get_all_namespaces();
                    func.captured_functions = scope.get_all_functions();
                    func.captured_vars = scope.get_all_vars();
                    functions.insert(def.name.clone(), func.clone());
                    scope.set_function(&def.name, func);
                }
                Node::Import(import) => {
                    self.resolve_import(import, base_dir, runtime_vars, &mut scope, warnings)?;
                }
                Node::Export(export) => {
                    has_explicit_exports = true;
                    match export {
                        ExportDirective::Named { name } => {
                            explicit_exports.insert(name.clone());
                        }
                        ExportDirective::ReExport {
                            name,
                            path: import_path,
                        } => {
                            // Resolve the source module and bring in the function for
                            // re-export only. Per spec: "@export from does not bring the
                            // symbol into the current file's scope".
                            validate_import_path(import_path)?;
                            let resolved_path = resolve_path(base_dir, import_path);
                            let source_module =
                                self.resolve(&resolved_path, runtime_vars, warnings)?;
                            if let Some(func) = source_module.get_export(name) {
                                functions.insert(name.clone(), func);
                            }
                            explicit_exports.insert(name.clone());
                        }
                        ExportDirective::Wildcard { path: import_path } => {
                            // Re-export all exports from the target module. These are
                            // available to importers but NOT in the current file's scope.
                            validate_import_path(import_path)?;
                            let resolved_path = resolve_path(base_dir, import_path);
                            let source_module =
                                self.resolve(&resolved_path, runtime_vars, warnings)?;
                            for (name, func) in source_module.get_all_exports() {
                                if functions.contains_key(&name) {
                                    return Err(MdsError::name_collision(name));
                                }
                                functions.insert(name.clone(), func);
                                explicit_exports.insert(name);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Validate that all named exports refer to defined functions or "prompt"
        for name in &explicit_exports {
            if name != "prompt" && !functions.contains_key(name) {
                return Err(MdsError::export_error(format!(
                    "cannot export '{name}': not defined in this module"
                )));
            }
        }

        // Validate semantic correctness before evaluation
        validator::validate(&module.body, &scope, file_str, source)?;

        // Evaluate the body to get prompt text
        let prompt_body = evaluate(&module.body, &mut scope, warnings)?;
        let prompt_body = if prompt_body.trim().is_empty() {
            None
        } else {
            Some(prompt_body)
        };

        Ok(ResolvedModule {
            functions,
            prompt_body,
            has_explicit_exports,
            explicit_exports,
        })
    }

    fn resolve_import(
        &mut self,
        import: &ImportDirective,
        base_dir: &Path,
        runtime_vars: &HashMap<String, Value>,
        scope: &mut Scope,
        warnings: &mut Vec<String>,
    ) -> Result<(), MdsError> {
        match import {
            ImportDirective::Alias { path, alias } => {
                validate_import_path(path)?;
                let import_path = resolve_path(base_dir, path);
                let resolved = self.resolve(&import_path, runtime_vars, warnings)?;
                let ns = module_to_namespace(&resolved);
                scope.set_namespace(alias, ns);
            }
            ImportDirective::Merge { path } => {
                validate_import_path(path)?;
                let import_path = resolve_path(base_dir, path);
                let resolved = self.resolve(&import_path, runtime_vars, warnings)?;
                // Per spec: only functions and the prompt body are imported via merge.
                // Frontmatter variables from the imported module are NOT brought into scope.
                for (name, func) in resolved.get_all_exports() {
                    if scope.get_function(&name).is_some() {
                        return Err(MdsError::name_collision(name));
                    }
                    scope.set_function(&name, func);
                }
                if let Some(val) = resolved.get_prompt_value() {
                    scope.set_var("prompt", val);
                }
            }
            ImportDirective::Selective { names, path } => {
                validate_import_path(path)?;
                let import_path = resolve_path(base_dir, path);
                let resolved = self.resolve(&import_path, runtime_vars, warnings)?;
                for name in names {
                    if name == "prompt" {
                        if let Some(val) = resolved.get_prompt_value() {
                            scope.set_var("prompt", val);
                        } else {
                            return Err(MdsError::import_error(format!(
                                "'{name}' is not exported from '{path}'"
                            )));
                        }
                    } else if let Some(func) = resolved.get_export(name) {
                        scope.set_function(name, func);
                    } else {
                        return Err(MdsError::import_error(format!(
                            "'{name}' is not exported from '{path}'"
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}

impl ResolvedModule {
    /// Get a single export by name.
    pub fn get_export(&self, name: &str) -> Option<FunctionDef> {
        if self.has_explicit_exports && !self.explicit_exports.contains(name) {
            return None;
        }
        self.functions.get(name).cloned()
    }

    /// Get all exported functions.
    pub fn get_all_exports(&self) -> Vec<(String, FunctionDef)> {
        self.functions
            .iter()
            .filter(|(name, _)| !self.has_explicit_exports || self.explicit_exports.contains(*name))
            .map(|(name, func)| (name.clone(), func.clone()))
            .collect()
    }

    /// Check if `prompt` is an available export for this module.
    pub fn has_prompt_export(&self) -> bool {
        self.prompt_body.is_some()
            && (!self.has_explicit_exports || self.explicit_exports.contains("prompt"))
    }

    /// Get the prompt body as a Value, if exportable.
    pub fn get_prompt_value(&self) -> Option<Value> {
        if !self.has_prompt_export() {
            return None;
        }
        self.prompt_body
            .as_ref()
            .map(|body| Value::String(body.clone()))
    }
}

/// Create a NamespaceScope from a resolved module.
fn module_to_namespace(module: &ResolvedModule) -> NamespaceScope {
    NamespaceScope {
        functions: module.get_all_exports().into_iter().collect(),
        prompt_body: module.prompt_body.clone(),
    }
}

/// Resolve a relative path against a base directory.
/// Per spec: only relative paths are allowed (must start with "./" or "../").
fn resolve_path(base_dir: &Path, relative: &str) -> PathBuf {
    base_dir.join(relative)
}

/// Validate that an import path is safe and relative.
///
/// Rejects absolute paths and paths containing components that could escape
/// the project directory (e.g., null bytes).
fn validate_import_path(path: &str) -> Result<(), MdsError> {
    if !path.starts_with("./") && !path.starts_with("../") {
        return Err(MdsError::import_error(format!(
            "import path must be relative (start with './' or '../'): \"{path}\""
        )));
    }
    // Reject null bytes which could truncate paths in some OS APIs
    if path.contains('\0') {
        return Err(MdsError::import_error("import path contains null byte"));
    }
    Ok(())
}

/// Validate that a file is a valid MDS file.
/// Accepts the already-read source content to avoid double-reading for `.md` files.
fn validate_file_type(path: &Path, source: &str) -> Result<(), MdsError> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    match ext {
        "mds" => Ok(()),
        "md" => {
            // Check for `type: mds` in frontmatter by parsing YAML properly
            if let Some(after_prefix) = source
                .strip_prefix("---\n")
                .or_else(|| source.strip_prefix("---\r\n"))
            {
                if let Some(end) = after_prefix.find("\n---") {
                    let fm = &after_prefix[..end];
                    if let Ok(map) = serde_yaml::from_str::<HashMap<String, serde_yaml::Value>>(fm)
                    {
                        if map.get("type").and_then(|v| v.as_str()) == Some("mds") {
                            return Ok(());
                        }
                    }
                }
            }
            Err(MdsError::NotMdsFile {
                path: path.display().to_string(),
            })
        }
        _ => Err(MdsError::NotMdsFile {
            path: path.display().to_string(),
        }),
    }
}

/// Format a cycle chain like "a.mds → b.mds → a.mds" from the resolving stack.
fn build_cycle_string(resolving_stack: &[PathBuf], repeated: &Path) -> String {
    let start = resolving_stack
        .iter()
        .position(|p| p == repeated)
        .expect("repeated path must be in resolving_stack");
    let mut parts: Vec<String> = resolving_stack[start..]
        .iter()
        .map(|p| path_display_name(p))
        .collect();
    parts.push(path_display_name(repeated));
    parts.join(" \u{2192} ")
}

/// Return a short display name for a path (filename, falling back to full path, then "?").
fn path_display_name(p: &Path) -> String {
    p.file_name()
        .and_then(|n| n.to_str())
        .or_else(|| p.to_str())
        .unwrap_or("?")
        .to_string()
}

fn parse_frontmatter(raw: &str) -> Result<HashMap<String, Value>, MdsError> {
    let yaml: serde_yaml::Value = serde_yaml::from_str(raw).map_err(|e| MdsError::YamlError {
        message: e.to_string(),
    })?;

    let mut vars = HashMap::new();
    if let serde_yaml::Value::Mapping(map) = yaml {
        for (key, val) in map {
            let serde_yaml::Value::String(key_str) = key else {
                continue;
            };
            let value = Value::from_yaml(val)?;
            vars.insert(key_str, value);
        }
    }
    Ok(vars)
}
