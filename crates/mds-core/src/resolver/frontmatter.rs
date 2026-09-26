//! Pure helper functions for YAML frontmatter parsing and scope construction.
//!
//! These are free functions extracted from `resolver.rs` that handle deep-merging
//! frontmatter mappings, building variable scopes, and parsing `imports:` declarations
//! from YAML frontmatter.

use std::cell::Cell;
use std::collections::HashMap;
use std::marker::PhantomData;

use serde::de::{self, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor};

use crate::error::MdsError;
use crate::limits::{
    MAX_FRONTMATTER_FLOW_DEPTH, MAX_FRONTMATTER_IMPORTS, MAX_FRONTMATTER_MERGE_DEPTH,
    MAX_FRONTMATTER_NODES, MAX_FRONTMATTER_SIZE,
};
use crate::parser::is_valid_identifier;
use crate::scope::Scope;
use crate::value::Value;

use super::import_path_violation;

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

    // Reserved keys excluded from the merged output at the TOP LEVEL only (depth == 0).
    // These keys are directives, not value data, so they are stripped from the emitted
    // frontmatter to prevent them from appearing in the compiled output.
    //
    // !! IMPORTANT: the guard is `depth == 0` — do NOT filter reserved names in
    // recursive calls (depth > 0).  A nested key like `config.type` is not a
    // top-level directive; filtering it would silently discard user data.
    //
    // SYNC POINT: when this constant changes, also audit `RESERVED_OUTPUT_KEYS` in lib.rs.
    // The two constants serve different purposes and are intentionally not identical:
    // `RESERVED_MERGE_KEYS` prevents reserved keys from propagating as FM variables at merge
    // time; `RESERVED_OUTPUT_KEYS` lists keys removed from raw YAML before re-emitting output.
    // `extends` is in RESERVED_MERGE_KEYS only — it is consumed as a directive during
    // inheritance and is never emitted as an output FM key.
    const RESERVED_MERGE_KEYS: &[&str] = &["imports", "type", "extends"];

    let mut result = serde_yaml_ng::Mapping::new();

    // Phase 1: walk base keys in order.
    // Each base key keeps its position; value is replaced if child also has that key.
    for (base_key, base_val) in base {
        // Skip non-string keys.
        let serde_yaml_ng::Value::String(key_str) = base_key else {
            continue;
        };
        // Reserved keys are stripped at the top-level only (they are directives, not data).
        if depth == 0 && RESERVED_MERGE_KEYS.contains(&key_str.as_str()) {
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
        // Reserved keys are stripped at the top-level only.
        if depth == 0 && RESERVED_MERGE_KEYS.contains(&key_str.as_str()) {
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

/// The single choke point for parsing frontmatter YAML into an untyped `Value` (#162).
///
/// All four frontmatter parse sites route through here so the DoS bounds cannot be
/// bypassed on a parallel path. It returns a `Value` (not a `Mapping`) so each caller
/// keeps its own "not a mapping" handling.
///
/// Three bounds are enforced ahead of / during the parse:
/// 1. A 1 MiB byte cap (`MAX_FRONTMATTER_SIZE`), checked before any YAML work because the
///    `serde_yaml_ng` loader is eager (it drains the whole document into an event vector).
/// 2. A flow-nesting depth guard (`MAX_FRONTMATTER_FLOW_DEPTH`), a single O(n) byte pass
///    before the parser. libyaml's flow scanner is O(depth^2) and runs UPSTREAM of the
///    node budget, so a deep flow-nest under the 1 MiB cap still burns seconds of CPU at
///    trivial RSS; the byte cap alone does not bound it. See [`check_flow_nesting_depth`].
/// 3. A 200 000-node budget (`MAX_FRONTMATTER_NODES`), charged while deserialising, which
///    is what catches an `&anchor` referenced by many `*alias`es — the amplification
///    `serde_yaml_ng`'s own alias-jump limit does not catch.
///
/// Our bounds surface as [`MdsError::resource_limit`]; everything `serde_yaml_ng` itself
/// rejects (syntax errors, its recursion/repetition limits, duplicate keys) surfaces as
/// [`MdsError::yaml_error`] with a message byte-identical to a plain
/// `serde_yaml_ng::from_str::<Value>`, because the same `Deserializer` drives both.
pub(crate) fn parse_frontmatter_yaml(raw: &str) -> Result<serde_yaml_ng::Value, MdsError> {
    parse_frontmatter_yaml_bounded(raw, MAX_FRONTMATTER_SIZE, MAX_FRONTMATTER_NODES)
}

/// Bounds-parameterised core of [`parse_frontmatter_yaml`], so unit tests can pin node
/// accounting on tiny documents without allocating a real attack.
fn parse_frontmatter_yaml_bounded(
    raw: &str,
    max_bytes: usize,
    max_nodes: usize,
) -> Result<serde_yaml_ng::Value, MdsError> {
    // 1. Size cap FIRST — before the eager loader touches the input. Bounds total work
    //    (the flow-nesting depth guard below bounds libyaml's O(depth^2) flow scanner,
    //    which the byte cap alone does not: a deep nest under 1 MiB still hangs).
    if raw.len() > max_bytes {
        return Err(MdsError::resource_limit(format!(
            "frontmatter too large ({} bytes, max {max_bytes} bytes)",
            raw.len()
        )));
    }

    // 1b. Flow-nesting depth guard — a single O(n) byte pass BEFORE the parser, so
    //     libyaml's O(depth^2) flow scanner never runs on a pathological deep nest. This
    //     is a CPU bound the node budget cannot provide: the scanner runs UPSTREAM of
    //     deserialisation (at trivial RSS, few nodes), so a ~1 MiB pure deep flow-nest
    //     hangs for seconds before the budget or any downstream depth limit fires. #162.
    check_flow_nesting_depth(raw, MAX_FRONTMATTER_FLOW_DEPTH)?;

    // 2. Budgeted deserialisation. `from_str::<Value>` is exactly
    //    `Value::deserialize(Deserializer::from_str(raw))`; driving the same deserializer
    //    with the budgeted seed keeps every non-budget error byte-identical.
    let budget = NodeBudget::new(max_nodes);
    let seed = BoundedYaml { budget: &budget };
    match seed.deserialize(serde_yaml_ng::Deserializer::from_str(raw)) {
        Ok(value) => Ok(value),
        // The budget latch is the discriminator — never the message text.
        Err(_) if budget.tripped() => Err(MdsError::resource_limit(format!(
            "frontmatter YAML node count exceeds maximum of {max_nodes} \
             (anchors expanded by aliases count once per expansion)"
        ))),
        Err(e) => Err(MdsError::yaml_error(e.to_string())),
    }
}

/// Reject frontmatter whose running flow-collection nesting depth ever exceeds
/// `max_depth`, in one O(n) pass over the raw bytes (#162).
///
/// This is the pre-parse CPU bound: libyaml's flow scanner is O(depth^2) in flow nesting
/// and runs UPSTREAM of the node budget, so a ~1 MiB pure deep flow-nest hangs for seconds
/// before any downstream depth limit fires. The scan counts NET depth — flow openers
/// (`[`, `{`) increment, closers (`]`, `}`) decrement (saturating at 0) — not a total
/// bracket count, so a wide-but-shallow flow list (`[a, b, c, ...]`, depth 1) stays legal;
/// only nesting DEPTH is bounded. `[`/`]`/`{`/`}` are ASCII (< 0x80) and never occur inside
/// a UTF-8 multibyte sequence, so a byte scan is exact for them.
///
/// The scan is deliberately naive: it does NOT skip brackets inside quoted scalars or
/// comments (that would require a YAML lexer). At a threshold of 1024 — 8x serde_yaml_ng's
/// own 128-frame recursion limit — a false rejection would need 1024+ net-unbalanced flow
/// openers inside scalar/comment content, which no legitimate frontmatter contains: a
/// document serde accepts has structural flow depth <= 64 (`MAX_VALUE_DEPTH`). The high
/// threshold, not a lexer, is the guard against false positives.
fn check_flow_nesting_depth(raw: &str, max_depth: usize) -> Result<(), MdsError> {
    // Bounded by `raw.len()`, which the size cap has already bounded by MAX_FRONTMATTER_SIZE.
    let mut depth: usize = 0;
    for &byte in raw.as_bytes() {
        match byte {
            b'[' | b'{' => {
                depth += 1;
                if depth > max_depth {
                    // Never echo the (adversarial) raw input in the message.
                    return Err(MdsError::resource_limit(format!(
                        "frontmatter YAML flow nesting exceeds maximum depth of {max_depth}"
                    )));
                }
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

/// A saturating-free node budget with a "tripped" latch, shared by reference across the
/// deserialise walk. `Cell` because the seed is `Copy` and threaded by value.
struct NodeBudget {
    remaining: Cell<usize>,
    tripped: Cell<bool>,
}

impl NodeBudget {
    fn new(max: usize) -> Self {
        Self {
            remaining: Cell::new(max),
            tripped: Cell::new(false),
        }
    }

    /// Charge one node. On exhaustion, latch `tripped` and return a custom error so the
    /// classifier in [`parse_frontmatter_yaml_bounded`] can attribute it to our bound.
    /// `checked_sub` (never saturating) so the boundary is exact.
    fn charge<E: de::Error>(&self) -> Result<(), E> {
        match self.remaining.get().checked_sub(1) {
            Some(rest) => {
                self.remaining.set(rest);
                Ok(())
            }
            None => {
                self.tripped.set(true);
                Err(E::custom("frontmatter YAML node budget exhausted"))
            }
        }
    }

    fn tripped(&self) -> bool {
        self.tripped.get()
    }
}

/// A budgeted `DeserializeSeed`/`Visitor` that mirrors `serde_yaml_ng`'s own
/// `impl Deserialize for Value`, charging one node per scalar, sequence, mapping, mapping
/// key and `!tag` wrapper. It never pre-sizes from `size_hint` (an attacker controls it),
/// and it rejects duplicate keys with a message byte-identical to `serde_yaml_ng`'s.
#[derive(Clone, Copy)]
struct BoundedYaml<'b> {
    budget: &'b NodeBudget,
}

impl<'de, 'b> DeserializeSeed<'de> for BoundedYaml<'b> {
    type Value = serde_yaml_ng::Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(self)
    }
}

impl<'de, 'b> Visitor<'de> for BoundedYaml<'b> {
    type Value = serde_yaml_ng::Value;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("any YAML value")
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
        self.budget.charge::<E>()?;
        Ok(serde_yaml_ng::Value::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
        self.budget.charge::<E>()?;
        Ok(serde_yaml_ng::Value::Number(v.into()))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
        self.budget.charge::<E>()?;
        Ok(serde_yaml_ng::Value::Number(v.into()))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
        self.budget.charge::<E>()?;
        Ok(serde_yaml_ng::Value::Number(v.into()))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        self.budget.charge::<E>()?;
        Ok(serde_yaml_ng::Value::String(v.to_owned()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
        self.budget.charge::<E>()?;
        Ok(serde_yaml_ng::Value::String(v))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        self.budget.charge::<E>()?;
        Ok(serde_yaml_ng::Value::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        self.budget.charge::<E>()?;
        Ok(serde_yaml_ng::Value::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(self)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        // Charge the container, then each element as the seed visits it. Do NOT pre-size
        // from `size_hint` — an alias expansion reports a large hint the attacker controls.
        self.budget.charge::<A::Error>()?;
        let mut out = serde_yaml_ng::Sequence::new();
        while let Some(elem) = seq.next_element_seed(self)? {
            out.push(elem);
        }
        Ok(serde_yaml_ng::Value::Sequence(out))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        self.budget.charge::<A::Error>()?;
        let mut out = serde_yaml_ng::Mapping::new();
        // Keys are nodes too. Reject a duplicate BEFORE reading its value, exactly as
        // `serde_yaml_ng`'s `Mapping` visitor does, with a byte-identical message.
        while let Some(key) = map.next_key_seed(self)? {
            match out.entry(key) {
                serde_yaml_ng::mapping::Entry::Occupied(entry) => {
                    return Err(<A::Error as de::Error>::custom(duplicate_key_message(
                        entry.key(),
                    )));
                }
                serde_yaml_ng::mapping::Entry::Vacant(entry) => {
                    let value = map.next_value_seed(self)?;
                    entry.insert(value);
                }
            }
        }
        Ok(serde_yaml_ng::Value::Mapping(out))
    }

    fn visit_enum<A>(self, data: A) -> Result<Self::Value, A::Error>
    where
        A: EnumAccess<'de>,
    {
        // Charge the `!tag` wrapper, then the inner value via the seed.
        self.budget.charge::<A::Error>()?;
        let (tag, contents) = data.variant_seed(PhantomData::<String>)?;
        // `Tag::new` panics on an empty tag, so mirror `TagStringVisitor`'s guard and
        // return the same error instead (never panic — #162 / never-panic contract).
        if tag.is_empty() {
            return Err(<A::Error as de::Error>::custom(
                "empty YAML tag is not allowed",
            ));
        }
        let value = contents.newtype_variant_seed(self)?;
        Ok(serde_yaml_ng::Value::Tagged(Box::new(
            serde_yaml_ng::value::TaggedValue {
                tag: serde_yaml_ng::value::Tag::new(tag),
                value,
            },
        )))
    }
}

/// The `serde_yaml_ng` `DuplicateKeyError` `Display`, reproduced byte-for-byte (its type
/// is private). Mirrors `src/mapping.rs` in the vendored crate.
fn duplicate_key_message(key: &serde_yaml_ng::Value) -> String {
    use serde_yaml_ng::Value;
    let mut message = String::from("duplicate entry ");
    match key {
        Value::Null => message.push_str("with null key"),
        Value::Bool(boolean) => message.push_str(&format!("with key `{boolean}`")),
        Value::Number(number) => message.push_str(&format!("with key {number}")),
        Value::String(string) => message.push_str(&format!("with key {string:?}")),
        Value::Sequence(_) | Value::Mapping(_) | Value::Tagged(_) => {
            message.push_str("in YAML map");
        }
    }
    message
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

    // Validate path via the same rules as body @import, reporting the rule it
    // actually breaks (#265) — the path is escaped, since it may carry the very
    // character being refused.
    if let Some(violation) = import_path_violation(&path) {
        return Err(err(&format!(
            "invalid path \"{}\": {}",
            crate::lint::escape_path_for_message(&path),
            violation.reason()
        )));
    }

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
    let yaml = parse_frontmatter_yaml(raw)?;

    let serde_yaml_ng::Value::Mapping(ref map) = yaml else {
        return Ok(vec![]);
    };

    let Some(imports_val) = map.get("imports") else {
        return Ok(vec![]);
    };

    parse_frontmatter_imports_from_yaml(imports_val)
}

#[cfg(test)]
#[path = "frontmatter_tests.rs"]
mod frontmatter_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping_from_str(yaml: &str) -> serde_yaml_ng::Mapping {
        let val: serde_yaml_ng::Value = serde_yaml_ng::from_str(yaml).unwrap();
        match val {
            serde_yaml_ng::Value::Mapping(m) => m,
            _ => panic!("expected a YAML mapping"),
        }
    }

    fn mapping_get_str<'a>(m: &'a serde_yaml_ng::Mapping, key: &str) -> Option<&'a str> {
        m.get(serde_yaml_ng::Value::String(key.to_owned()))
            .and_then(|v| v.as_str())
    }

    /// Top-level reserved keys (`imports`, `type`, `extends`) must be excluded from
    /// the merged output — they are directive tokens, not value data.
    #[test]
    fn deep_merge_yaml_strips_top_level_reserved_keys() {
        let base = mapping_from_str("type: mds\nmodel: gpt-4\nimports:\n  - path: ./lib.mds\n");
        let child = mapping_from_str("extends: ./base.mds\nrole: system\n");
        let result = deep_merge_yaml(&base, &child, 0).unwrap();
        assert!(
            !result.contains_key(serde_yaml_ng::Value::String("type".into())),
            "top-level 'type' must be stripped"
        );
        assert!(
            !result.contains_key(serde_yaml_ng::Value::String("imports".into())),
            "top-level 'imports' must be stripped"
        );
        assert!(
            !result.contains_key(serde_yaml_ng::Value::String("extends".into())),
            "top-level 'extends' must be stripped"
        );
        assert_eq!(mapping_get_str(&result, "model"), Some("gpt-4"));
        assert_eq!(mapping_get_str(&result, "role"), Some("system"));
    }

    /// Nested keys named `type`, `imports`, or `extends` inside a sub-mapping must
    /// NOT be filtered — the reserved-key guard applies at the top level only (depth == 0).
    ///
    /// Example: `config.type: "mds"` is a user-defined nested key, not a directive.
    /// Filtering it would silently corrupt user data.
    #[test]
    fn deep_merge_yaml_nested_reserved_keys_preserved() {
        let base = mapping_from_str(
            "config:\n  type: mds\n  model: gpt-4\n  imports: strict\nmodel: base-model\n",
        );
        let child = mapping_from_str("config:\n  type: custom\n  extra: added\nrole: system\n");
        let result = deep_merge_yaml(&base, &child, 0).unwrap();

        // `config` key must be present with the merged sub-mapping.
        let config = result
            .get(serde_yaml_ng::Value::String("config".into()))
            .and_then(|v| v.as_mapping())
            .expect("config sub-mapping must survive merge");

        // config.type: child value wins ("custom" overrides "mds").
        assert_eq!(
            config
                .get(serde_yaml_ng::Value::String("type".into()))
                .and_then(|v| v.as_str()),
            Some("custom"),
            "config.type (nested 'type') must NOT be stripped and child value must win"
        );
        // config.imports: base-only nested key, must survive.
        assert_eq!(
            config
                .get(serde_yaml_ng::Value::String("imports".into()))
                .and_then(|v| v.as_str()),
            Some("strict"),
            "config.imports (nested 'imports') must NOT be stripped"
        );
        // config.model: base-only nested key.
        assert_eq!(
            config
                .get(serde_yaml_ng::Value::String("model".into()))
                .and_then(|v| v.as_str()),
            Some("gpt-4"),
            "base-only config.model must survive"
        );
        // config.extra: child-only nested key.
        assert_eq!(
            config
                .get(serde_yaml_ng::Value::String("extra".into()))
                .and_then(|v| v.as_str()),
            Some("added"),
            "child-only config.extra must be present"
        );
        // Top-level keys are correct.
        assert_eq!(mapping_get_str(&result, "model"), Some("base-model"));
        assert_eq!(mapping_get_str(&result, "role"), Some("system"));
    }

    /// Merging two empty mappings must produce an empty result (not an error).
    #[test]
    fn deep_merge_yaml_both_empty() {
        let base = serde_yaml_ng::Mapping::new();
        let child = serde_yaml_ng::Mapping::new();
        let result = deep_merge_yaml(&base, &child, 0).unwrap();
        assert!(result.is_empty());
    }
}
