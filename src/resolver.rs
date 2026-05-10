use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::ast::*;
use crate::error::MdsError;
use crate::evaluator::evaluate;
use crate::lexer::tokenize;
use crate::parser::parse;
use crate::scope::{FunctionDef, NamespaceScope, Scope};
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

/// Module cache to avoid re-resolving the same file.
pub struct ModuleCache {
    modules: HashMap<PathBuf, ResolvedModule>,
    /// Tracks modules currently being resolved (for cycle detection).
    resolving: HashSet<PathBuf>,
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
        let canonical = path
            .canonicalize()
            .map_err(|_| MdsError::FileNotFound {
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

        // Validate file type
        validate_file_type(&canonical)?;

        // Read source
        let source = std::fs::read_to_string(&canonical).map_err(|e| MdsError::Io {
            message: format!("cannot read {}: {e}", canonical.display()),
        })?;

        let file_str = canonical.display().to_string();

        // Tokenize and parse
        let tokens = tokenize(&source, &file_str)?;
        let module = parse(&tokens, &file_str)?;

        // Mark as resolving
        self.resolving.insert(canonical.clone());

        // Build scope from frontmatter
        let mut scope = Scope::new();
        let mut vars = HashMap::new();

        if let Some(ref fm) = module.frontmatter {
            let yaml_vars = parse_frontmatter(&fm.raw)?;
            for (key, value) in &yaml_vars {
                if key != "type" {
                    // Skip the 'type' meta-field
                    scope.set_var(key, value.clone());
                    vars.insert(key.clone(), value.clone());
                }
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

        // First pass: resolve imports and collect definitions
        let base_dir = canonical.parent().unwrap_or(Path::new("."));

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
                        ExportDirective::ReExport { name, path, .. } => {
                            // Resolve the source module and bring in the function
                            let import_path = resolve_path(base_dir, path);
                            let source_module = self.resolve(&import_path, runtime_vars)?;
                            if let Some(func) = source_module.get_export(name) {
                                functions.insert(name.clone(), func);
                                scope.set_function(name, functions.get(name).unwrap().clone());
                            }
                            explicit_exports.insert(name.clone());
                        }
                        ExportDirective::Wildcard { path, .. } => {
                            let import_path = resolve_path(base_dir, path);
                            let source_module = self.resolve(&import_path, runtime_vars)?;
                            for (name, func) in source_module.get_all_exports() {
                                if functions.contains_key(&name) {
                                    return Err(MdsError::NameCollision { name });
                                }
                                functions.insert(name.clone(), func.clone());
                                scope.set_function(&name, func);
                                explicit_exports.insert(name);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Evaluate the body to get prompt text
        let prompt_body = evaluate(&module.body, &mut scope)?;
        let prompt_body = if prompt_body.trim().is_empty() {
            None
        } else {
            Some(prompt_body)
        };

        let resolved = ResolvedModule {
            path: canonical.clone(),
            module,
            functions,
            vars,
            prompt_body,
            has_explicit_exports,
            explicit_exports,
        };

        // Cache and unmark
        self.resolving.remove(&canonical);
        self.modules.insert(canonical, resolved.clone());

        Ok(resolved)
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
                let import_path = resolve_path(base_dir, path);
                let resolved = self.resolve(&import_path, runtime_vars)?;
                let ns = module_to_namespace(&resolved);
                scope.set_namespace(alias, ns);
            }
            ImportDirective::Merge { path, .. } => {
                let import_path = resolve_path(base_dir, path);
                let resolved = self.resolve(&import_path, runtime_vars)?;
                // Merge exports into current scope
                for (name, func) in resolved.get_all_exports() {
                    scope.set_function(&name, func);
                }
                for (name, value) in &resolved.vars {
                    scope.set_var(&name, value.clone());
                }
            }
            ImportDirective::Selective { names, path, .. } => {
                let import_path = resolve_path(base_dir, path);
                let resolved = self.resolve(&import_path, runtime_vars)?;
                for name in names {
                    if let Some(func) = resolved.get_export(name) {
                        scope.set_function(name, func);
                    } else {
                        return Err(MdsError::ImportError {
                            message: format!(
                                "'{name}' is not exported from '{path}'"
                            ),
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
        if self.has_explicit_exports {
            self.functions
                .iter()
                .filter(|(name, _)| self.explicit_exports.contains(*name))
                .map(|(name, func)| (name.clone(), func.clone()))
                .collect()
        } else {
            self.functions
                .iter()
                .map(|(name, func)| (name.clone(), func.clone()))
                .collect()
        }
    }
}

/// Create a NamespaceScope from a resolved module.
fn module_to_namespace(module: &ResolvedModule) -> NamespaceScope {
    let mut functions = HashMap::new();
    for (name, func) in module.get_all_exports() {
        functions.insert(name, func);
    }

    NamespaceScope {
        functions,
        vars: module.vars.clone(),
        prompt_body: module.prompt_body.clone(),
    }
}

/// Resolve a relative path against a base directory.
fn resolve_path(base_dir: &Path, relative: &str) -> PathBuf {
    base_dir.join(relative)
}

/// Validate that a file is a valid MDS file.
fn validate_file_type(path: &Path) -> Result<(), MdsError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    match ext {
        "mds" => Ok(()),
        "md" => {
            // Check for `type: mds` in frontmatter
            let source = std::fs::read_to_string(path).map_err(|e| MdsError::Io {
                message: format!("cannot read {}: {e}", path.display()),
            })?;
            if source.starts_with("---") {
                if let Some(end) = source[3..].find("---") {
                    let fm = &source[3..3 + end];
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
    let yaml: serde_yaml::Value = serde_yaml::from_str(raw).map_err(|e| MdsError::YamlError {
        message: e.to_string(),
    })?;

    let mut vars = HashMap::new();
    if let serde_yaml::Value::Mapping(map) = yaml {
        for (key, val) in map {
            let key_str = match key {
                serde_yaml::Value::String(s) => s,
                _ => continue,
            };
            let value = Value::from_yaml(val).map_err(|e| MdsError::YamlError { message: e })?;
            vars.insert(key_str, value);
        }
    }
    Ok(vars)
}
