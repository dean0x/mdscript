//! Pure helper functions for YAML frontmatter parsing and scope construction.
//!
//! These are free functions extracted from `resolver.rs` that handle deep-merging
//! frontmatter mappings, building variable scopes, and parsing `imports:` declarations
//! from YAML frontmatter.

use std::collections::HashMap;

use crate::error::MdsError;
use crate::limits::{MAX_FRONTMATTER_IMPORTS, MAX_FRONTMATTER_MERGE_DEPTH};
use crate::parser::is_valid_identifier;
use crate::scope::Scope;
use crate::value::Value;

use super::validate_import_path;

/// A single import declaration from YAML frontmatter.
///
/// Three forms mirror the body `@import` directive:
/// - **Alias**: `{ path: "./lib.mds", as: lib }` — imported under a namespace alias.
/// - **Merge**: `{ path: "./lib.mds" }` — all exports merged into the current scope.
/// - **Selective**: `{ path: "./lib.mds", names: [greet, farewell] }` — named exports only.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FrontmatterImport {
    Alias { path: String, alias: String },
    Merge { path: String },
    Selective { path: String, names: Vec<String> },
}

impl FrontmatterImport {
    pub(crate) fn path(&self) -> &str {
        match self {
            Self::Alias { path, .. } | Self::Merge { path } | Self::Selective { path, .. } => path,
        }
    }
}

/// Deep-merge two YAML `Mapping`s with base-wins-if-absent / child-wins-if-present semantics.
///
/// Semantics (decision #7):
/// - When BOTH values at a key are `Mapping`, recursively merge key-by-key.
/// - Otherwise child wins (scalar over scalar, scalar over map, map over scalar).
/// - Arrays/sequences REPLACE WHOLESALE — no element-level merge.
/// - Key ORDER: base-then-child (determinism A6). Keys present in base keep their
///   original position; their value may be replaced by the merged/child value.
///   Child-only keys are appended in child order after all base keys.
/// - Reserved keys (`imports`, `type`, `extends`) are excluded from the output —
///   they are not value data (decision #7). Callers handle them separately.
/// - Recursion is bounded by `MAX_FRONTMATTER_MERGE_DEPTH`; exceeding it returns
///   `mds::resource_limit` (P4 — no stack overflow).
///
/// The `depth` argument starts at 0 and is incremented on each recursive call.
pub(super) fn deep_merge_yaml(
    base: &serde_yaml_ng::Mapping,
    child: &serde_yaml_ng::Mapping,
    depth: usize,
) -> Result<serde_yaml_ng::Mapping, MdsError> {
    if depth > MAX_FRONTMATTER_MERGE_DEPTH {
        return Err(MdsError::resource_limit(format!(
            "frontmatter merge depth exceeds maximum of {MAX_FRONTMATTER_MERGE_DEPTH}"
        )));
    }

    // Reserved keys excluded from the merged output (must not propagate as FM variables).
    // SYNC POINT: if you add a key here, audit `strip_reserved_keys` in lib.rs —
    // that function strips `type` and `imports` from raw frontmatter output but intentionally
    // omits `extends` (it is a directive token, not an output FM key).
    // The two lists serve different purposes and are NOT identical by design; keep this comment
    // and the strip_reserved_keys comment in sync when either list changes.
    const RESERVED: &[&str] = &["imports", "type", "extends"];

    let mut result = serde_yaml_ng::Mapping::new();

    // Phase 1: walk base keys in order.
    // Each base key keeps its position; value is replaced if child also has that key.
    for (base_key, base_val) in base {
        // Skip reserved keys and non-string keys.
        let serde_yaml_ng::Value::String(key_str) = base_key else {
            continue;
        };
        if RESERVED.contains(&key_str.as_str()) {
            continue;
        }

        let merged_val = if let Some(child_val) = child.get(base_key) {
            // Both have this key: recurse if both are Mapping, else child wins.
            match (base_val, child_val) {
                (serde_yaml_ng::Value::Mapping(bm), serde_yaml_ng::Value::Mapping(cm)) => {
                    let merged_map = deep_merge_yaml(bm, cm, depth + 1)?;
                    serde_yaml_ng::Value::Mapping(merged_map)
                }
                // Child wins for all other combinations (including arrays — replace wholesale).
                (_, other) => other.clone(),
            }
        } else {
            // Base-only key: include as-is.
            base_val.clone()
        };

        result.insert(base_key.clone(), merged_val);
    }

    // Phase 2: append child-only keys in child order.
    for (child_key, child_val) in child {
        let serde_yaml_ng::Value::String(key_str) = child_key else {
            continue;
        };
        if RESERVED.contains(&key_str.as_str()) {
            continue;
        }
        // Skip keys already added from base.
        if result.contains_key(child_key) {
            continue;
        }
        result.insert(child_key.clone(), child_val.clone());
    }

    Ok(result)
}

/// Build a scope from a pre-merged `Mapping` and runtime variable overrides.
///
/// Used by the template inheritance path after `deep_merge_yaml` has already
/// excluded reserved keys (`imports`, `type`, `extends`). The mapping is pure
/// value data — no reserved-key handling needed here.
///
/// Runtime vars are applied LAST so precedence is: base < child < runtime (F7).
pub(super) fn build_scope_from_merged_mapping(
    mapping: &serde_yaml_ng::Mapping,
    runtime_vars: &HashMap<String, Value>,
) -> Result<Scope, MdsError> {
    let mut scope = Scope::new();

    for (key, val) in mapping {
        let serde_yaml_ng::Value::String(key_str) = key else {
            continue;
        };
        let value = Value::from_yaml(val.clone())?;
        scope.set_var(key_str, value);
    }

    // Runtime vars override everything (base < child < runtime, F7, decision #3).
    for (key, value) in runtime_vars {
        scope.set_var(key, value.clone());
    }

    Ok(scope)
}

/// Parse the `imports` key from an already-parsed YAML value.
///
/// `imports_val` must be a YAML Sequence; each element must be a Mapping with
/// a required `path` string key and at most one of `as` (alias) or `names` (selective).
pub(crate) fn parse_frontmatter_imports_from_yaml(
    imports_val: &serde_yaml_ng::Value,
) -> Result<Vec<FrontmatterImport>, MdsError> {
    let serde_yaml_ng::Value::Sequence(seq) = imports_val else {
        return Err(MdsError::import_error(
            "imports must be a YAML sequence (in frontmatter)",
        ));
    };

    if seq.len() > MAX_FRONTMATTER_IMPORTS {
        return Err(MdsError::resource_limit(format!(
            "imports exceeds maximum of {MAX_FRONTMATTER_IMPORTS} entries (in frontmatter)"
        )));
    }

    seq.iter()
        .enumerate()
        .map(|(index, entry)| parse_single_import_entry(entry, index))
        .collect()
}

/// Parse one entry from the `imports` YAML sequence.
///
/// `index` is used solely for error messages.
fn parse_single_import_entry(
    entry: &serde_yaml_ng::Value,
    index: usize,
) -> Result<FrontmatterImport, MdsError> {
    let err =
        |msg: &str| MdsError::import_error(format!("imports[{index}]: {msg} (in frontmatter)"));

    let serde_yaml_ng::Value::Mapping(map) = entry else {
        return Err(err("each entry must be a mapping"));
    };

    // Validate all keys first: reject non-string keys and unknown field names.
    for (k, _) in map {
        let serde_yaml_ng::Value::String(key_str) = k else {
            return Err(err("keys must be strings"));
        };
        match key_str.as_str() {
            "path" | "as" | "names" => {}
            other => return Err(err(&format!("unknown key '{other}'"))),
        }
    }

    // Extract path (required)
    let path_val = map
        .get("path")
        .ok_or_else(|| err("missing required key 'path'"))?;
    let serde_yaml_ng::Value::String(path) = path_val else {
        return Err(err("'path' must be a string"));
    };
    let path = path.clone();

    // Validate path via the same rules as body @import
    validate_import_path(&path).map_err(|_| {
        err(&format!(
            "invalid path \"{path}\": must start with './' or '../'"
        ))
    })?;

    match (map.get("as"), map.get("names")) {
        (Some(_), Some(_)) => Err(err("'as' and 'names' are mutually exclusive")),
        (Some(as_v), None) => parse_alias_entry(as_v, path, &err),
        (None, Some(names_v)) => parse_selective_entry(names_v, path, &err),
        (None, None) => Ok(FrontmatterImport::Merge { path }),
    }
}

/// Parse the alias (`as`) form of a frontmatter import entry.
fn parse_alias_entry(
    as_v: &serde_yaml_ng::Value,
    path: String,
    err: &impl Fn(&str) -> MdsError,
) -> Result<FrontmatterImport, MdsError> {
    let serde_yaml_ng::Value::String(alias) = as_v else {
        return Err(err("'as' must be a string"));
    };
    if !is_valid_identifier(alias) {
        return Err(err(&format!(
            "invalid identifier '{alias}' for 'as': must start with a letter or '_' \
             and contain only alphanumeric characters or '_'"
        )));
    }
    Ok(FrontmatterImport::Alias {
        path,
        alias: alias.clone(),
    })
}

/// Parse the selective (`names`) form of a frontmatter import entry.
fn parse_selective_entry(
    names_v: &serde_yaml_ng::Value,
    path: String,
    err: &impl Fn(&str) -> MdsError,
) -> Result<FrontmatterImport, MdsError> {
    use std::collections::HashSet;

    let serde_yaml_ng::Value::Sequence(names_seq) = names_v else {
        return Err(err("'names' must be a sequence"));
    };
    if names_seq.is_empty() {
        return Err(err("names cannot be empty"));
    }
    let mut names = Vec::with_capacity(names_seq.len());
    let mut seen = HashSet::with_capacity(names_seq.len());
    for name_val in names_seq {
        let serde_yaml_ng::Value::String(name) = name_val else {
            return Err(err("each name in 'names' must be a string"));
        };
        // "prompt" is a special export name — allowed without identifier validation
        if name != "prompt" && !is_valid_identifier(name) {
            return Err(err(&format!(
                "invalid identifier '{name}' in 'names': must start with a letter or \
                 '_' and contain only alphanumeric characters or '_'"
            )));
        }
        if !seen.insert(name.as_str()) {
            return Err(err(&format!("duplicate name '{name}' in 'names'")));
        }
        names.push(name.clone());
    }
    Ok(FrontmatterImport::Selective { path, names })
}

/// Parse frontmatter imports from a raw YAML string.
///
/// Returns an empty `Vec` if the `imports` key is absent. Propagates any
/// parse or validation error from [`parse_frontmatter_imports_from_yaml`].
pub(crate) fn parse_frontmatter_imports(raw: &str) -> Result<Vec<FrontmatterImport>, MdsError> {
    let yaml: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(raw).map_err(|e| MdsError::yaml_error(e.to_string()))?;

    let serde_yaml_ng::Value::Mapping(ref map) = yaml else {
        return Ok(vec![]);
    };

    let Some(imports_val) = map.get("imports") else {
        return Ok(vec![]);
    };

    parse_frontmatter_imports_from_yaml(imports_val)
}
