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

/// A resolved module with its AST, exports, and prompt body.
#[derive(Debug, Clone)]
pub struct ResolvedModule {
    pub path: PathBuf,
    pub module: Module,
    pub functions: HashMap<String, FunctionDef>,
    pub vars: HashMap<String, Value>,
    pub prompt_body: Option<String>,
    pub has_explicit_exports: bool,
    pub explicit_exports: HashSet<String>,
}

/// Maximum import depth to prevent stack overflow from deeply chained imports.
const MAX_IMPORT_DEPTH: usize = 64;

/// Module cache to avoid re-resolving the same file.
pub struct ModuleCache {
    modules: HashMap<PathBuf, ResolvedModule>,
    /// Tracks modules currently being resolved (for cycle detection).
    resolving: HashSet<PathBuf>,
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
        }
    }

    /// Resolve a module from file path. Handles caching and cycle detection.
    pub fn resolve(
        &mut self,
        path: &Path,
        runtime_vars: &HashMap<String, Value>,
    ) -> Result<ResolvedModule, MdsError> {
        let canonical = path.canonicalize().map_err(|_| MdsError::FileNotFound {
            path: path.display().to_string(),
        })?;

        // Check cache
        if let Some(cached) = self.modules.get(&canonical) {
            return Ok(cached.clone());
        }

        // Check for circular imports
        if self.resolving.contains(&canonical) {
            let cycle = canonical.display().to_string();
            return Err(MdsError::CircularImport { cycle });
        }

        // Guard against excessively deep import chains
        if self.resolving.len() >= MAX_IMPORT_DEPTH {
            return Err(MdsError::ImportError {
                message: format!(
                    "import depth exceeds maximum of {MAX_IMPORT_DEPTH} (possible deep chain)"
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

        // Mark as resolving before recursing into process_module
        self.resolving.insert(canonical.clone());

        let resolved = self.process_module(
            &source,
            &file_str,
            &base_dir,
            canonical.clone(),
            runtime_vars,
        )?;

        // Cache and unmark
        self.resolving.remove(&canonical);
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
    ) -> Result<ResolvedModule, MdsError> {
        let virtual_path = base_dir.join("<source>");
        self.process_module(source, "<source>", base_dir, virtual_path, runtime_vars)
    }

    /// Common module processing: tokenize, parse, build scope, evaluate.
    fn process_module(
        &mut self,
        source: &str,
        file_str: &str,
        base_dir: &Path,
        path: PathBuf,
        runtime_vars: &HashMap<String, Value>,
    ) -> Result<ResolvedModule, MdsError> {
        // Tokenize and parse
        let tokens = tokenize(source, file_str)?;
        let module = parse(&tokens, file_str)?;

        // Build scope from frontmatter
        let mut scope = Scope::new();
        let mut vars = HashMap::new();

        if let Some(ref fm) = module.frontmatter {
            let yaml_vars = parse_frontmatter(&fm.raw)?;
            for (key, value) in yaml_vars {
                if key == "type" {
                    continue; // Skip the 'type' meta-field
                }
                scope.set_var(&key, value.clone());
                vars.insert(key, value);
            }
        }

        // Apply runtime vars (override frontmatter)
        for (key, value) in runtime_vars {
            scope.set_var(key, value.clone());
            vars.insert(key.clone(), value.clone());
        }

        // Collect function definitions and process imports
        let mut functions = HashMap::new();
        let mut has_explicit_exports = false;
        let mut explicit_exports = HashSet::new();

        for node in &module.body {
            match node {
                Node::Define(def) => {
                    let func = FunctionDef::from(def);
                    functions.insert(def.name.clone(), func.clone());
                    scope.set_function(&def.name, func);
                }
                Node::Import(import) => {
                    self.resolve_import(import, base_dir, runtime_vars, &mut scope)?;
                }
                Node::Export(export) => {
                    has_explicit_exports = true;
                    match export {
                        ExportDirective::Named { name, .. } => {
                            explicit_exports.insert(name.clone());
                        }
                        ExportDirective::ReExport {
                            name,
                            path: import_path,
                            ..
                        } => {
                            // Resolve the source module and bring in the function for
                            // re-export only. Per spec: "@export from does not bring the
                            // symbol into the current file's scope".
                            validate_import_path(import_path)?;
                            let resolved_path = resolve_path(base_dir, import_path);
                            let source_module = self.resolve(&resolved_path, runtime_vars)?;
                            if let Some(func) = source_module.get_export(name) {
                                functions.insert(name.clone(), func);
                            }
                            explicit_exports.insert(name.clone());
                        }
                        ExportDirective::Wildcard {
                            path: import_path, ..
                        } => {
                            // Re-export all exports from the target module. These are
                            // available to importers but NOT in the current file's scope.
                            validate_import_path(import_path)?;
                            let resolved_path = resolve_path(base_dir, import_path);
                            let source_module = self.resolve(&resolved_path, runtime_vars)?;
                            for (name, func) in source_module.get_all_exports() {
                                if functions.contains_key(&name) {
                                    return Err(MdsError::NameCollision { name });
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

        // Validate semantic correctness before evaluation
        validator::validate(&module.body, &scope)?;

        // Evaluate the body to get prompt text
        let prompt_body = evaluate(&module.body, &mut scope)?;
        let prompt_body = if prompt_body.trim().is_empty() {
            None
        } else {
            Some(prompt_body)
        };

        Ok(ResolvedModule {
            path,
            module,
            functions,
            vars,
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
    ) -> Result<(), MdsError> {
        match import {
            ImportDirective::Alias { path, alias, .. } => {
                validate_import_path(path)?;
                let import_path = resolve_path(base_dir, path);
                let resolved = self.resolve(&import_path, runtime_vars)?;
                let ns = module_to_namespace(&resolved);
                scope.set_namespace(alias, ns);
            }
            ImportDirective::Merge { path, .. } => {
                validate_import_path(path)?;
                let import_path = resolve_path(base_dir, path);
                let resolved = self.resolve(&import_path, runtime_vars)?;
                for (name, func) in resolved.get_all_exports() {
                    if scope.get_function(&name).is_some() {
                        return Err(MdsError::NameCollision { name });
                    }
                    scope.set_function(&name, func);
                }
                for (name, value) in &resolved.vars {
                    scope.set_var(name, value.clone());
                }
            }
            ImportDirective::Selective { names, path, .. } => {
                validate_import_path(path)?;
                let import_path = resolve_path(base_dir, path);
                let resolved = self.resolve(&import_path, runtime_vars)?;
                for name in names {
                    if let Some(func) = resolved.get_export(name) {
                        scope.set_function(name, func);
                    } else {
                        return Err(MdsError::ImportError {
                            message: format!("'{name}' is not exported from '{path}'"),
                        });
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
}

/// Create a NamespaceScope from a resolved module.
fn module_to_namespace(module: &ResolvedModule) -> NamespaceScope {
    NamespaceScope {
        functions: module.get_all_exports().into_iter().collect(),
        vars: module.vars.clone(),
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
/// the project directory (e.g., null bytes or excessively long paths).
fn validate_import_path(path: &str) -> Result<(), MdsError> {
    if !path.starts_with("./") && !path.starts_with("../") {
        return Err(MdsError::ImportError {
            message: format!("import path must be relative (start with './' or '../'): \"{path}\""),
        });
    }
    // Reject null bytes which could truncate paths in some OS APIs
    if path.contains('\0') {
        return Err(MdsError::ImportError {
            message: "import path contains null byte".to_string(),
        });
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
            // Check for `type: mds` in frontmatter
            if let Some(after_prefix) = source.strip_prefix("---") {
                if let Some(end) = after_prefix.find("---") {
                    let fm = &after_prefix[..end];
                    if fm.contains("type: mds") || fm.contains("type: \"mds\"") {
                        return Ok(());
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

/// Parse YAML frontmatter into a map of values.
pub fn parse_frontmatter(raw: &str) -> Result<HashMap<String, Value>, MdsError> {
    let yaml: serde_yml::Value = serde_yml::from_str(raw).map_err(|e| MdsError::YamlError {
        message: e.to_string(),
    })?;

    let mut vars = HashMap::new();
    if let serde_yml::Value::Mapping(map) = yaml {
        for (key, val) in map {
            let serde_yml::Value::String(key_str) = key else {
                continue;
            };
            let value = Value::from_yaml(val).map_err(|e| MdsError::YamlError { message: e })?;
            vars.insert(key_str, value);
        }
    }
    Ok(vars)
}
