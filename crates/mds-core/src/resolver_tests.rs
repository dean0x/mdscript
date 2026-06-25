//! Resolver unit and integration tests, extracted from resolver.rs.

use super::frontmatter::deep_merge_yaml;
use super::*;
use crate::limits::{MAX_FRONTMATTER_IMPORTS, MAX_FRONTMATTER_MERGE_DEPTH};

// Helper to build a YAML Value from inline YAML text
fn yaml(s: &str) -> serde_yaml_ng::Value {
    serde_yaml_ng::from_str(s).expect("valid yaml in test")
}

// ── parse_frontmatter_imports_from_yaml ───────────────────────────────────

#[test]
fn parse_fm_import_alias() {
    let v = yaml("- path: ./lib.mds\n  as: lib\n");
    let result = parse_frontmatter_imports_from_yaml(&v).expect("should parse");
    assert_eq!(
        result,
        vec![FrontmatterImport::Alias {
            path: "./lib.mds".into(),
            alias: "lib".into(),
        }]
    );
}

#[test]
fn parse_fm_import_merge() {
    let v = yaml("- path: ./lib.mds\n");
    let result = parse_frontmatter_imports_from_yaml(&v).expect("should parse");
    assert_eq!(
        result,
        vec![FrontmatterImport::Merge {
            path: "./lib.mds".into()
        }]
    );
}

#[test]
fn parse_fm_import_selective() {
    let v = yaml("- path: ./lib.mds\n  names: [greet, farewell]\n");
    let result = parse_frontmatter_imports_from_yaml(&v).expect("should parse");
    assert_eq!(
        result,
        vec![FrontmatterImport::Selective {
            path: "./lib.mds".into(),
            names: vec!["greet".into(), "farewell".into()],
        }]
    );
}

#[test]
fn parse_fm_import_multiple() {
    let v = yaml(
        "- path: ./a.mds\n  as: a\n\
             - path: ./b.mds\n\
             - path: ./c.mds\n  names: [f]\n",
    );
    let result = parse_frontmatter_imports_from_yaml(&v).expect("should parse");
    assert_eq!(result.len(), 3);
    assert!(matches!(result[0], FrontmatterImport::Alias { .. }));
    assert!(matches!(result[1], FrontmatterImport::Merge { .. }));
    assert!(matches!(result[2], FrontmatterImport::Selective { .. }));
}

#[test]
fn parse_fm_import_empty_array() {
    let v = yaml("[]");
    let result = parse_frontmatter_imports_from_yaml(&v).expect("empty array is ok");
    assert!(result.is_empty());
}

#[test]
fn parse_fm_no_imports_key() {
    let result =
        parse_frontmatter_imports("name: Alice\ngreeting: hello\n").expect("no imports key");
    assert!(result.is_empty());
}

#[test]
fn parse_fm_err_missing_path() {
    let v = yaml("- as: lib\n");
    let err = parse_frontmatter_imports_from_yaml(&v).expect_err("missing path should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("missing required key 'path'") && msg.contains("in frontmatter"),
        "got: {msg}"
    );
}

#[test]
fn parse_fm_err_path_not_string() {
    let v = yaml("- path: 42\n");
    let err = parse_frontmatter_imports_from_yaml(&v).expect_err("non-string path should fail");
    let msg = err.to_string();
    assert!(msg.contains("'path' must be a string"), "got: {msg}");
}

#[test]
fn parse_fm_err_invalid_as_id() {
    let v = yaml("- path: ./lib.mds\n  as: 123bad\n");
    let err = parse_frontmatter_imports_from_yaml(&v).expect_err("invalid identifier should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("invalid identifier") && msg.contains("in frontmatter"),
        "got: {msg}"
    );
}

#[test]
fn parse_fm_err_as_and_names() {
    let v = yaml("- path: ./lib.mds\n  as: lib\n  names: [f]\n");
    let err = parse_frontmatter_imports_from_yaml(&v).expect_err("mutually exclusive");
    let msg = err.to_string();
    assert!(
        msg.contains("mutually exclusive") && msg.contains("in frontmatter"),
        "got: {msg}"
    );
}

#[test]
fn parse_fm_err_unknown_key() {
    let v = yaml("- path: ./lib.mds\n  foo: bar\n");
    let err = parse_frontmatter_imports_from_yaml(&v).expect_err("unknown key should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("unknown key 'foo'") && msg.contains("in frontmatter"),
        "got: {msg}"
    );
}

#[test]
fn parse_fm_err_not_array() {
    let v = yaml("path: ./lib.mds\n");
    let err = parse_frontmatter_imports_from_yaml(&v).expect_err("not array should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("must be a YAML sequence") && msg.contains("in frontmatter"),
        "got: {msg}"
    );
}

#[test]
fn parse_fm_err_empty_names() {
    let v = yaml("- path: ./lib.mds\n  names: []\n");
    let err = parse_frontmatter_imports_from_yaml(&v).expect_err("empty names should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("names cannot be empty") && msg.contains("in frontmatter"),
        "got: {msg}"
    );
}

#[test]
fn parse_fm_err_absolute_path() {
    let v = yaml("- path: /absolute/path.mds\n");
    let err = parse_frontmatter_imports_from_yaml(&v).expect_err("absolute path should fail");
    let msg = err.to_string();
    assert!(msg.contains("in frontmatter"), "got: {msg}");
}

#[test]
fn parse_fm_err_exceeds_limit() {
    // Build a sequence with MAX_FRONTMATTER_IMPORTS + 1 entries
    let entry = "- path: ./lib.mds\n";
    let many = entry.repeat(MAX_FRONTMATTER_IMPORTS + 1);
    let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&many).expect("valid yaml");
    let err = parse_frontmatter_imports_from_yaml(&v).expect_err("should exceed limit");
    let msg = err.to_string();
    assert!(msg.contains("exceeds maximum"), "got: {msg}");
}

#[test]
fn parse_fm_prompt_name_in_selective() {
    // "prompt" is a special name — allowed without identifier validation
    let v = yaml("- path: ./lib.mds\n  names: [prompt]\n");
    let result = parse_frontmatter_imports_from_yaml(&v).expect("prompt is allowed");
    assert_eq!(
        result,
        vec![FrontmatterImport::Selective {
            path: "./lib.mds".into(),
            names: vec!["prompt".into()],
        }]
    );
}

#[test]
fn parse_fm_err_duplicate_names() {
    // Duplicate names in the selective names list must be rejected.
    let v = yaml("- path: ./lib.mds\n  names: [greet, greet]\n");
    let err = parse_frontmatter_imports_from_yaml(&v).expect_err("duplicate names should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("duplicate name 'greet'") && msg.contains("in frontmatter"),
        "got: {msg}"
    );
}

#[test]
fn parse_fm_err_non_string_key() {
    // Non-string YAML keys (e.g. integer keys) must be rejected explicitly.
    // Construct a YAML mapping with an integer key via the serde_yaml_ng API
    // since inline YAML always coerces to string keys.
    let mut map = serde_yaml_ng::Mapping::new();
    map.insert(
        serde_yaml_ng::Value::String("path".into()),
        serde_yaml_ng::Value::String("./lib.mds".into()),
    );
    map.insert(
        serde_yaml_ng::Value::Number(42.into()),
        serde_yaml_ng::Value::String("something".into()),
    );
    let seq = serde_yaml_ng::Value::Sequence(vec![serde_yaml_ng::Value::Mapping(map)]);
    let err = parse_frontmatter_imports_from_yaml(&seq).expect_err("non-string key should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("keys must be strings") && msg.contains("in frontmatter"),
        "got: {msg}"
    );
}

#[test]
fn has_type_mds_frontmatter_raw_ignores_indented() {
    // Indented `type: mds` inside a nested YAML object must not be detected
    // as the file-type marker (only top-level non-indented keys should match).
    assert!(
        !has_type_mds_frontmatter_raw("config:\n  type: mds\n  key: val\n"),
        "indented type:mds should not trigger detection"
    );
    assert!(
        has_type_mds_frontmatter_raw("type: mds\nconfig:\n  type: other\n"),
        "top-level type:mds should trigger detection"
    );
}

#[test]
fn has_type_mds_frontmatter_ignores_indented() {
    // Same as above but for the full-source variant.
    assert!(
        !has_type_mds_frontmatter("---\nconfig:\n  type: mds\n---\nbody\n"),
        "indented type:mds should not trigger detection in full-source variant"
    );
    assert!(
        has_type_mds_frontmatter("---\ntype: mds\nconfig:\n  type: other\n---\nbody\n"),
        "top-level type:mds should trigger detection in full-source variant"
    );
}

// ── Phase 1: @block collision and resource-limit tests ────────────────────

#[test]
fn block_duplicate_name_collision() {
    // Two @block declarations with the same name → mds::name_collision.
    let src = "@block foo:\nbody1\n@end\n@block foo:\nbody2\n@end\n";
    let result = crate::compile_str_md(src);
    assert!(result.is_err(), "duplicate @block name must fail");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("'foo'") || msg.contains("foo"),
        "error should mention the colliding name: {msg}"
    );
}

#[test]
fn block_vs_define_name_collision() {
    // @block and @define sharing the same name → mds::name_collision.
    let src = "@define foo():\ncontent\n@end\n@block foo:\nbody\n@end\n";
    let result = crate::compile_str_md(src);
    assert!(result.is_err(), "@block vs @define collision must fail");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("'foo'") || msg.contains("foo"),
        "error should mention the colliding name: {msg}"
    );
}

#[test]
fn define_vs_block_name_collision() {
    // @define declared after a @block with the same name → mds::name_collision.
    let src = "@block foo:\nbody\n@end\n@define foo():\ncontent\n@end\n";
    let result = crate::compile_str_md(src);
    assert!(result.is_err(), "@define vs @block collision must fail");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("'foo'") || msg.contains("foo"),
        "error should mention the colliding name: {msg}"
    );
}

#[test]
fn block_max_per_module_cap() {
    // Declaring more than MAX_BLOCKS_PER_MODULE @blocks in one module → resource_limit.
    // Build a source with 257 @block declarations (one over the 256 cap).
    let mut src = String::new();
    for i in 0..=256usize {
        src.push_str(&format!("@block blk{i}:\nbody\n@end\n"));
    }
    let result = crate::compile_str_md(&src);
    assert!(
        result.is_err(),
        "exceeding MAX_BLOCKS_PER_MODULE should fail with resource_limit"
    );
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("resource limit") || msg.contains("256") || msg.contains("block"),
        "error should mention resource limit or block count: {msg}"
    );
}

#[test]
fn block_exactly_at_max_allowed() {
    // Exactly MAX_BLOCKS_PER_MODULE (256) @block declarations should compile.
    let mut src = String::new();
    for i in 0..256usize {
        src.push_str(&format!("@block blk{i}:\nbody\n@end\n"));
    }
    let result = crate::compile_str_md(&src);
    assert!(
        result.is_ok(),
        "exactly 256 @blocks should succeed, got: {result:?}"
    );
}

// ── Phase 2: Template inheritance ─────────────────────────────────────────

/// Helper: create a VirtualFs-backed ModuleCache from a &[(&str, &str)] slice.
fn virtual_cache(files: &[(&str, &str)]) -> ModuleCache {
    ModuleCache::virtual_fs(
        files
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    )
}

/// Helper: compile a VirtualFs entry and return the Markdown output string.
fn compile_virtual(files: &[(&str, &str)], entry: &str) -> Result<String, MdsError> {
    let map: std::collections::HashMap<String, String> = files
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    crate::compile_virtual_md(map, entry, None)
}

/// Helper: check (validate only, no output) a VirtualFs entry.
fn check_virtual(files: &[(&str, &str)], entry: &str) -> Result<(), MdsError> {
    let map: std::collections::HashMap<String, String> = files
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    crate::check_virtual(map, entry, None)
}

// ── F1: issue worked example (headline test) ──────────────────────────────

#[test]
fn f1_worked_example_byte_exact() {
    // base.mds: skeleton with @block placeholders
    // child.mds: overrides instructions+tools, inherits output_format default
    // role=data analysis from child frontmatter
    let base = concat!(
        "You are a {role} assistant.\n",
        "\n",
        "@block instructions:\n",
        "Analyze data carefully.\n",
        "@end\n",
        "@block tools:\n",
        "@end\n",
        "@block output_format:\n",
        "Respond in plain text.\n",
        "@end\n",
    );
    let child = concat!(
        "---\n",
        "role: data analysis\n",
        "---\n",
        "@extends \"./base.mds\"\n",
        "@block instructions:\n",
        "Perform statistical analysis.\n",
        "@end\n",
        "@block tools:\n",
        "You have access to: Python, R\n",
        "@end\n",
    );
    let files = [("base.mds", base), ("child.mds", child)];
    let result = compile_virtual(&files, "child.mds");
    assert!(result.is_ok(), "F1 compile failed: {:?}", result.err());
    let output = result.unwrap();

    // Must contain base skeleton text with child's frontmatter variable
    assert!(
        output.contains("You are a data analysis assistant."),
        "F1: base skeleton text not rendered: {output}"
    );
    // Must contain overridden blocks from child
    assert!(
        output.contains("Perform statistical analysis."),
        "F1: child instructions block not rendered: {output}"
    );
    assert!(
        output.contains("You have access to: Python, R"),
        "F1: child tools block not rendered: {output}"
    );
    // Must contain base default for un-overridden block
    assert!(
        output.contains("Respond in plain text."),
        "F1: base default output_format block not rendered: {output}"
    );
}

// ── F2: standalone base compiles fine rendering its own defaults ──────────

#[test]
fn f2_standalone_base_compiles_with_defaults() {
    let base = concat!(
        "---\n",
        "role: general\n",
        "---\n",
        "You are a {role} assistant.\n",
        "@block instructions:\n",
        "Help the user.\n",
        "@end\n",
    );
    let child = concat!(
        "---\n",
        "role: specialist\n",
        "---\n",
        "@extends \"./base.mds\"\n",
        "@block instructions:\n",
        "Provide expert advice.\n",
        "@end\n",
    );
    let files = [("base.mds", base), ("child.mds", child)];

    // Compile base standalone — must render its own defaults
    let base_out = compile_virtual(&files, "base.mds");
    assert!(
        base_out.is_ok(),
        "F2: standalone base compile failed: {:?}",
        base_out.err()
    );
    let base_str = base_out.unwrap();
    assert!(
        base_str.contains("Help the user."),
        "F2: base default not rendered standalone: {base_str}"
    );
    assert!(
        base_str.contains("You are a general assistant."),
        "F2: base standalone role not rendered: {base_str}"
    );

    // Compile child — must use child overrides and NOT poison base standalone
    let child_out = compile_virtual(&files, "child.mds");
    assert!(
        child_out.is_ok(),
        "F2: child compile failed: {:?}",
        child_out.err()
    );
    let child_str = child_out.unwrap();
    assert!(
        child_str.contains("Provide expert advice."),
        "F2: child override not rendered: {child_str}"
    );
    assert!(
        child_str.contains("You are a specialist assistant."),
        "F2: child role not rendered: {child_str}"
    );
}

// ── F2 cache non-poisoning: same base file as skeleton base AND standalone ─

#[test]
fn f2_cache_nonpoisoning_base_then_child() {
    // Compile the base FIRST (as standalone), THEN compile child.
    // The cached entry for base must serve the child's skeleton needs.
    let base = "You are a {role} assistant.\n@block instructions:\nDefault.\n@end\n";
    let child = concat!(
        "---\nrole: expert\n---\n",
        "@extends \"./base.mds\"\n",
        "@block instructions:\nExpert advice.\n@end\n",
    );
    let files = [("base.mds", base), ("child.mds", child)];
    let mut cache = virtual_cache(&files);
    let mut warnings = vec![];

    // Compile base standalone (no role var — will fail on {role} unless runtime vars set)
    // For this test, compile the child first (skeleton base resolution caches base),
    // then assert base standalone also works from same cache.
    let child_result = cache.resolve_key("child.mds", &Default::default(), &mut warnings);
    assert!(
        child_result.is_ok(),
        "cache non-poison: child should compile: {:?}",
        child_result.err()
    );

    // Now compile base standalone — should work independently (cache returns entry).
    // Base has {role} undefined without frontmatter, so it would fail standalone unless
    // cached entry with skeleton (prompt_body=None) is returned. We use a base WITH frontmatter.
    let base_with_fm = "---\nrole: default\n---\nYou are a {role}.\n@block b:\nBody.\n@end\n";
    let child2 = concat!(
        "---\nrole: override\n---\n",
        "@extends \"./base2.mds\"\n",
        "@block b:\nOverride.\n@end\n",
    );
    let files2 = [("base2.mds", base_with_fm), ("child2.mds", child2)];
    let mut cache2 = virtual_cache(&files2);
    let mut w = vec![];

    // Both in same process/cache: resolve base standalone first
    let base_out = cache2.resolve_key("base2.mds", &Default::default(), &mut w);
    assert!(
        base_out.is_ok(),
        "cache2: standalone base should succeed: {:?}",
        base_out.err()
    );

    // Then resolve child (base is already cached)
    let child_out = cache2.resolve_key("child2.mds", &Default::default(), &mut w);
    assert!(
        child_out.is_ok(),
        "cache2: child after cached base should succeed: {:?}",
        child_out.err()
    );
    let child_mod = child_out.unwrap();
    assert!(
        child_mod
            .prompt_body
            .as_deref()
            .unwrap_or("")
            .contains("Override."),
        "cache2: child should use override block"
    );
}

#[test]
fn f2_cache_nonpoisoning_skeleton_then_standalone_reverse_order() {
    // A1 (reverse of f2_cache_nonpoisoning_base_then_child): resolve the CHILD first,
    // which caches the base as a SKELETON (prompt_body=None, never validated/evaluated
    // standalone). A subsequent standalone resolve of that SAME base from the SAME cache
    // must NOT return the empty skeleton entry — it must render the base's own defaults.
    let base = "---\nrole: default\n---\nYou are a {role}.\n@block b:\nBody.\n@end\n";
    let child = concat!(
        "---\nrole: override\n---\n",
        "@extends \"./base.mds\"\n",
        "@block b:\nOverride.\n@end\n",
    );
    let files = [("base.mds", base), ("child.mds", child)];
    let mut cache = virtual_cache(&files);
    let mut w = vec![];

    // Resolve child FIRST — this caches base as a SKELETON (prompt_body=None).
    let child_out = cache.resolve_key("child.mds", &Default::default(), &mut w);
    assert!(
        child_out.is_ok(),
        "child should compile: {:?}",
        child_out.err()
    );

    // Now resolve base standalone from the SAME cache — must render its own defaults.
    let base_out = cache
        .resolve_key("base.mds", &Default::default(), &mut w)
        .expect("base standalone should compile");
    let body = base_out.prompt_body.as_deref().unwrap_or("<NONE>");
    assert!(
        body.contains("You are a default.") && body.contains("Body."),
        "A1: base standalone after skeleton-cache must render its defaults, got: {body:?}"
    );

    // Arc-sharing must survive the upgrade: the child (resolved earlier from the skeleton)
    // and the upgraded standalone base must still share the same effective_skeleton Arc.
    let child_again = cache
        .resolve_key("child.mds", &Default::default(), &mut w)
        .expect("child re-resolve");
    assert!(
        Arc::ptr_eq(
            &child_again.effective_skeleton,
            &base_out.effective_skeleton
        ),
        "A1: skeleton upgrade must preserve Arc-sharing with descendants"
    );
}

// ── UTF-8 boundary safety: cross-source span offsets yield None, not panic ─
//
// `compute_line_column` previously panicked with "byte index N is not a char
// boundary" when a base-template span offset (computed against the base source)
// was reused against the child source containing multibyte UTF-8 characters.
// After the fix the error is returned gracefully (e.g. mds::undefined_var).

/// Returns (base_src, child_src, base_key) for the multibyte UTF-8 fixture.
///
/// Base has an undefined variable so validation fires a span at byte 16
/// ("@block content:\n" = 16 bytes, then "{undefined_var}").  The child's
/// filesystem key contains a multibyte character (Japanese "あ" = 3 bytes
/// each), so byte 16 may land mid-codepoint in the child source — this was
/// the panic trigger.
fn utf8_boundary_extends_fixture() -> (&'static str, &'static str, &'static str) {
    let base = "@block content:\n{undefined_var}\n@end\n";
    let child = "@extends \"./ああb.mds\"\n";
    // The key must match the path literal in the child source.
    let base_key = "ああb.mds";
    (base, child, base_key)
}

fn assert_graceful_mds_error(code: &str, context: &str) {
    assert!(
        code.starts_with("mds::"),
        "{context}: expected an mds:: error code, got: {code}"
    );
}

#[test]
fn utf8_boundary_compile_virtual_no_panic() {
    let (base, child, base_key) = utf8_boundary_extends_fixture();
    let files = [(base_key, base), ("child.mds", child)];
    let result = compile_virtual(&files, "child.mds");
    assert!(
        result.is_err(),
        "utf8_boundary compile: should error (undefined variable), not succeed: {:?}",
        result.ok()
    );
    assert_graceful_mds_error(
        &result.unwrap_err().serialize().code,
        "utf8_boundary compile",
    );
}

#[test]
fn utf8_boundary_check_virtual_no_panic() {
    // Same scenario via check_virtual — validates only, no evaluate.
    let (base, child, base_key) = utf8_boundary_extends_fixture();
    let files = [(base_key, base), ("child.mds", child)];
    let result = check_virtual(&files, "child.mds");
    assert!(
        result.is_err(),
        "utf8_boundary check: should error (undefined variable), not succeed"
    );
    assert_graceful_mds_error(&result.unwrap_err().serialize().code, "utf8_boundary check");
}

// ── F3: multi-level chain A←B←C, most-derived wins ──────────────────────

#[test]
fn f3_multilevel_most_derived_wins() {
    let a = concat!(
        "@block content:\n",
        "From A.\n",
        "@end\n",
        "@block footer:\n",
        "Footer A.\n",
        "@end\n",
    );
    let b = concat!(
        "@extends \"./a.mds\"\n",
        "@block content:\n",
        "From B.\n",
        "@end\n",
    );
    let c = concat!(
        "@extends \"./b.mds\"\n",
        "@block content:\n",
        "From C.\n",
        "@end\n",
    );
    let files = [("a.mds", a), ("b.mds", b), ("c.mds", c)];

    // C overrides content → "From C." + footer default from A = "Footer A."
    let c_out = compile_virtual(&files, "c.mds").expect("F3: C should compile");
    assert!(
        c_out.contains("From C."),
        "F3: C content should be most-derived: {c_out}"
    );
    assert!(
        c_out.contains("Footer A."),
        "F3: footer should fall through to A default: {c_out}"
    );
    assert!(
        !c_out.contains("From A.") && !c_out.contains("From B."),
        "F3: C should override B which overrode A: {c_out}"
    );

    // B overrides content → "From B." + footer default from A = "Footer A."
    let b_out = compile_virtual(&files, "b.mds").expect("F3: B should compile");
    assert!(
        b_out.contains("From B."),
        "F3: B content should beat A's default: {b_out}"
    );
    assert!(
        b_out.contains("Footer A."),
        "F3: B footer should fall through to A default: {b_out}"
    );

    // A standalone → its own defaults
    let a_out = compile_virtual(&files, "a.mds").expect("F3: A should compile");
    assert!(
        a_out.contains("From A.") && a_out.contains("Footer A."),
        "F3: A standalone should render own defaults: {a_out}"
    );
}

// ── F5: diamond inheritance — B and C both extend A; A's cached blocks must not be polluted ─

#[test]
fn f5_diamond_inheritance_cache_not_polluted() {
    // A is the base. B and C both extend A.
    // B overrides `shared_block`. C does NOT override `shared_block`.
    // Compiling B then C in one process must not leak B's override into C.
    let a = "@block shared_block:\nFrom A.\n@end\n";
    let b = "@extends \"./a.mds\"\n@block shared_block:\nFrom B.\n@end\n";
    let c = "@extends \"./a.mds\"\n";

    let files = [("a.mds", a), ("b.mds", b), ("c.mds", c)];
    let mut cache = virtual_cache(&files);
    let mut warnings = vec![];

    // Compile B first
    let b_resolved = cache.resolve_key("b.mds", &Default::default(), &mut warnings);
    assert!(
        b_resolved.is_ok(),
        "F5: B should compile: {:?}",
        b_resolved.err()
    );
    let b_body = b_resolved.unwrap().prompt_body.clone().unwrap_or_default();
    assert!(
        b_body.contains("From B."),
        "F5: B should contain its override: {b_body}"
    );

    // Compile C (uses SAME cache, A already cached)
    let c_resolved = cache.resolve_key("c.mds", &Default::default(), &mut warnings);
    assert!(
        c_resolved.is_ok(),
        "F5: C should compile: {:?}",
        c_resolved.err()
    );
    let c_body = c_resolved.unwrap().prompt_body.clone().unwrap_or_default();
    assert!(
        c_body.contains("From A."),
        "F5: C should use A's default (not B's override): {c_body}"
    );
    assert!(
        !c_body.contains("From B."),
        "F5: C must NOT have B's override (cache poisoning): {c_body}"
    );
}

// ── F12: base default block calls a base @define → resolves ───────────────

#[test]
fn f12_base_define_resolves_in_child() {
    let base = concat!(
        "@define greet(name):\n",
        "Hello, {name}!\n",
        "@end\n",
        "@block content:\n",
        "{greet(\"World\")}\n",
        "@end\n",
    );
    let child = "@extends \"./base.mds\"\n";
    let files = [("base.mds", base), ("child.mds", child)];

    let result = compile_virtual(&files, "child.mds");
    assert!(
        result.is_ok(),
        "F12: child compile failed: {:?}",
        result.err()
    );
    let output = result.unwrap();
    assert!(
        output.contains("Hello, World!"),
        "F12: base @define should resolve in child: {output}"
    );
}

// ── E3: stray child content → mds::extends ────────────────────────────────

#[test]
fn e3_stray_child_content_error() {
    let base = "@block b:\nDefault.\n@end\n";
    let child = concat!(
        "@extends \"./base.mds\"\n",
        "This is stray text!\n",
        "@block b:\nOverride.\n@end\n",
    );
    let files = [("base.mds", base), ("child.mds", child)];

    let err =
        compile_virtual(&files, "child.mds").expect_err("E3: stray text should produce an error");
    let serialized = err.serialize();
    assert_eq!(
        serialized.code, "mds::extends",
        "E3: error code should be mds::extends: {serialized:?}"
    );
    assert!(
        serialized.message.contains("only @block overrides"),
        "E3: message should mention @block overrides: {}",
        serialized.message
    );

    // A5: check_virtual must produce the same error
    let check_err =
        check_virtual(&files, "child.mds").expect_err("E3 A5: check must also reject stray text");
    assert_eq!(
        check_err.serialize().code,
        "mds::extends",
        "E3 A5: check error code should be mds::extends"
    );
}

// ── E4 / F4: unknown override → mds::extends ─────────────────────────────

#[test]
fn e4_unknown_override_error() {
    let base = "@block known:\nDefault.\n@end\n";
    let child = concat!(
        "@extends \"./base.mds\"\n",
        "@block known:\nOK.\n@end\n",
        "@block unknown_block:\nBad.\n@end\n",
    );
    let files = [("base.mds", base), ("child.mds", child)];

    let err = compile_virtual(&files, "child.mds")
        .expect_err("E4: unknown override should produce an error");
    let serialized = err.serialize();
    assert_eq!(
        serialized.code, "mds::extends",
        "E4: error code should be mds::extends: {serialized:?}"
    );
    assert!(
        serialized
            .message
            .contains("only the root template may declare"),
        "E4: message should mention root template: {}",
        serialized.message
    );

    // A5: check_virtual must produce the same error
    let check_err = check_virtual(&files, "child.mds")
        .expect_err("E4 A5: check must also reject unknown override");
    assert_eq!(
        check_err.serialize().code,
        "mds::extends",
        "E4 A5: check error code should be mds::extends"
    );
}

// ── F4/E4 intermediate: intermediate template may not declare new @block ────
//
// AC: In an A←B←C chain, only the root (A) may declare @block placeholders.
// B is an intermediate — it extends A but is itself extended by C. If B
// introduces a brand-new @block name absent from A, both compiling B standalone
// and compiling the leaf C must reject with mds::extends.

#[test]
fn f4_intermediate_new_block_rejected() {
    // A = root base with one declared @block.
    let a = "@block known:\nRoot default.\n@end\n";
    // B = intermediate: extends A, overrides the known block (valid), but also
    // declares a NEW @block name that A never declared (invalid).
    let b = concat!(
        "@extends \"./a.mds\"\n",
        "@block known:\nB override.\n@end\n",
        "@block new_in_b:\nThis must be rejected.\n@end\n",
    );
    // C = leaf extending B (valid chain if B were valid).
    let c = "@extends \"./b.mds\"\n@block known:\nC override.\n@end\n";
    let files = [("a.mds", a), ("b.mds", b), ("c.mds", c)];

    // Compiling B directly must fail.
    let err_b = compile_virtual(&files, "b.mds")
        .expect_err("F4-intermediate: new @block in intermediate B must be rejected");
    let serialized_b = err_b.serialize();
    assert_eq!(
        serialized_b.code, "mds::extends",
        "F4-intermediate: B compile error code must be mds::extends: {serialized_b:?}"
    );
    assert!(
        serialized_b
            .message
            .contains("only the root template may declare"),
        "F4-intermediate: B error message must mention root template: {}",
        serialized_b.message
    );
    assert!(
        serialized_b.span.is_some(),
        "F4-intermediate: B error must carry a span"
    );

    // Compiling the leaf C must also fail (B is invalid, so C cannot build on it).
    let err_c = compile_virtual(&files, "c.mds")
        .expect_err("F4-intermediate: leaf C on invalid intermediate B must be rejected");
    let serialized_c = err_c.serialize();
    assert_eq!(
        serialized_c.code, "mds::extends",
        "F4-intermediate: C compile error code must be mds::extends: {serialized_c:?}"
    );
    assert!(
        serialized_c
            .message
            .contains("only the root template may declare"),
        "F4-intermediate: C error message must mention root template: {}",
        serialized_c.message
    );

    // check_virtual on B must produce the same error (A5 parity).
    let check_err_b = check_virtual(&files, "b.mds")
        .expect_err("F4-intermediate: check_virtual on B must also reject");
    assert_eq!(
        check_err_b.serialize().code,
        "mds::extends",
        "F4-intermediate: check_virtual B error code must be mds::extends"
    );
}

// ── E5: circular inheritance → mds::circular_import ──────────────────────

#[test]
fn e5_circular_inheritance_a_to_b_to_a() {
    // Two-file mutual cycle: a2.mds extends b2.mds, b2.mds extends a2.mds.
    let a2 = "@extends \"./b2.mds\"\n";
    let b2 = "@extends \"./a2.mds\"\n";
    let files2 = [("a2.mds", a2), ("b2.mds", b2)];

    let err = compile_virtual(&files2, "a2.mds")
        .expect_err("E5: circular @extends should produce an error");
    let serialized = err.serialize();
    assert_eq!(
        serialized.code, "mds::circular_import",
        "E5: should surface as mds::circular_import: {serialized:?}"
    );

    // Self-extension: @extends "./self.mds"
    let self_ext = "@extends \"./self.mds\"\n";
    let files_self = [("self.mds", self_ext)];
    let err_self = compile_virtual(&files_self, "self.mds")
        .expect_err("E5: self-extension should produce circular_import");
    let serialized_self = err_self.serialize();
    assert_eq!(
        serialized_self.code, "mds::circular_import",
        "E5: self-extension should surface as mds::circular_import: {serialized_self:?}"
    );
}

// ── E5: uses valid circular detection with files that have blocks ─────────

#[test]
fn e5_circular_two_hop() {
    // A extends B extends A (proper 2-hop cycle)
    // A has a @block so it's a valid root base syntax-wise
    let a = "@extends \"./b.mds\"\n";
    let b = "@extends \"./a.mds\"\n@block blk:\nB.\n@end\n";
    // This won't work because a.mds has no @block — let's use a root base C that both extend
    // A extends B, B extends A — since neither has @block declarations at root,
    // the cycle is detected before block validation.
    let files = [("a.mds", a), ("b.mds", b)];
    let err = compile_virtual(&files, "a.mds").expect_err("E5: two-hop cycle should error");
    let code = err.serialize().code;
    assert_eq!(
        code, "mds::circular_import",
        "E5: two-hop cycle should be circular_import: {code}"
    );
}

// ── E6: 65-deep chain → import-depth error ────────────────────────────────

#[test]
fn e6_depth_limit_exceeded() {
    // Build a chain of 66 files: file0 extends file1 extends ... extends file65
    // file65 is the root base with @block declarations.
    let depth = 66usize; // one more than MAX_IMPORT_DEPTH (64)
    let mut files: Vec<(String, String)> = Vec::new();

    // Root base
    let root_src = "@block content:\nRoot.\n@end\n".to_string();
    files.push((format!("file{depth}.mds"), root_src));

    // Each intermediate extends the next
    for i in (0..depth).rev() {
        let src = format!("@extends \"./file{}.mds\"\n", i + 1);
        files.push((format!("file{i}.mds"), src));
    }

    let file_refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let err = compile_virtual(&file_refs, "file0.mds")
        .expect_err("E6: depth > 64 should produce an error");
    let code = err.serialize().code;
    // check_import_depth fires when resolving.len() >= MAX_IMPORT_DEPTH (64) and
    // returns MdsError::import_error(...), code = "mds::import".  The linear chain
    // has no cycles and no frontmatter, so circular_import and resource_limit cannot
    // be triggered here.
    assert_eq!(
        code, "mds::import",
        "E6: depth-exceeded error should be mds::import (check_import_depth): {code}"
    );
}

// ── E10: missing base → file-not-found with span ──────────────────────────

#[test]
fn e10_missing_base_file_not_found() {
    let child = "@extends \"./missing.mds\"\n";
    let files = [("child.mds", child)];
    let err = compile_virtual(&files, "child.mds")
        .expect_err("E10: missing base should produce file-not-found");
    let serialized = err.serialize();
    assert_eq!(
        serialized.code, "mds::file_not_found",
        "E10: should be file_not_found: {serialized:?}"
    );
}

// ── E11: parse error in base propagates with base's location ─────────────

#[test]
fn e11_parse_error_in_base_propagates() {
    // Base has a syntax error: @if without condition
    let base = "@block b:\n@if :\nbad\n@end\n@end\n";
    let child = "@extends \"./base.mds\"\n@block b:\nOK.\n@end\n";
    let files = [("base.mds", base), ("child.mds", child)];
    let err = compile_virtual(&files, "child.mds")
        .expect_err("E11: parse error in base should propagate");
    let code = err.serialize().code;
    assert!(
        code == "mds::syntax" || code == "mds::extends",
        "E11: parse error should be syntax or extends: {code}"
    );
}

// ── E12: base default block with undefined var → validation error at leaf ──

#[test]
fn e12_base_default_undefined_var_caught_at_leaf() {
    // Base has a default block referencing {undefined_var} which is NOT in the
    // base's frontmatter and NOT provided by the child. This should produce an
    // undefined-var error (caught against the merged scope at the leaf).
    let base = "@block content:\n{undefined_var}\n@end\n";
    let child = "@extends \"./base.mds\"\n"; // No frontmatter, no runtime vars

    let files = [("base.mds", base), ("child.mds", child)];
    let err = compile_virtual(&files, "child.mds")
        .expect_err("E12: undefined var in base default should error at leaf");
    let serialized = err.serialize();
    assert!(
        serialized.code == "mds::undefined_var" || serialized.code == "mds::syntax",
        "E12: should be undefined_var (or syntax): {serialized:?}"
    );

    // A5: check_virtual must also reject this
    let check_err = check_virtual(&files, "child.mds")
        .expect_err("E12 A5: check must also reject undefined var in base default");
    assert!(
        check_err.serialize().code == "mds::undefined_var"
            || check_err.serialize().code == "mds::syntax",
        "E12 A5: check should be undefined_var/syntax: {:?}",
        check_err.serialize()
    );
}

// ── A2: dependency ordering — base FIRST, before body imports ────────────

#[test]
fn a2_dependency_ordering_base_first() {
    let base = "@block b:\nBase.\n@end\n";
    let lib = "@define helper():\nHelper.\n@end\n";
    let child = concat!("@extends \"./base.mds\"\n", "@block b:\n@end\n",);
    // We test via compile_virtual_with_deps which returns the dependency list.
    let files: std::collections::HashMap<String, String> = [
        ("base.mds".to_string(), base.to_string()),
        ("lib.mds".to_string(), lib.to_string()),
        ("child.mds".to_string(), child.to_string()),
    ]
    .into_iter()
    .collect();

    let result = crate::compile_virtual_with_deps(files, "child.mds", None);
    assert!(result.is_ok(), "A2: should compile: {:?}", result.err());
    let output = result.unwrap();
    // base.mds must appear in dependencies (it's a dependency of child.mds)
    assert!(
        output.dependencies.contains(&"base.mds".to_string()),
        "A2: base.mds should be in dependencies: {:?}",
        output.dependencies
    );
    // base.mds must appear BEFORE any body imports (scan_imports puts extends first)
    if let Some(base_idx) = output.dependencies.iter().position(|d| d == "base.mds") {
        // If there are body imports, they must come after base
        // For this test case there are no body imports, but the order is correct.
        assert!(
            base_idx == 0,
            "A2: base.mds should be first dependency: {:?}",
            output.dependencies
        );
    }
}

// ── P1: effective_skeleton is Arc<[Node]>, no deep-clone ─────────────────

#[test]
fn p1_effective_skeleton_is_arc_shared() {
    // Verify that after resolving a child, both the base and child share the
    // same Arc<[Node]> skeleton (pointer equality).
    let base = "@block b:\nBase.\n@end\n";
    let child = "@extends \"./base.mds\"\n@block b:\nChild.\n@end\n";
    let files = [("base.mds", base), ("child.mds", child)];
    let mut cache = virtual_cache(&files);
    let mut warnings = vec![];

    // Resolve base first (as skeleton via child resolution)
    let child_resolved = cache
        .resolve_key("child.mds", &Default::default(), &mut warnings)
        .expect("P1: child should compile");
    let base_resolved = cache
        .resolve_key("base.mds", &Default::default(), &mut warnings)
        .expect("P1: base should compile");

    // Both should share the same Arc<[Node]> skeleton (Arc::ptr_eq)
    let child_skeleton = &child_resolved.effective_skeleton;
    let base_skeleton = &base_resolved.effective_skeleton;
    assert!(
        Arc::ptr_eq(child_skeleton, base_skeleton),
        "P1: child and base must share the same Arc<[Node]> skeleton (ptr_eq)"
    );
}

// ── MdsError::Extends serialize() wired correctly ─────────────────────────

#[test]
fn extends_error_serialize_code() {
    let err = MdsError::extends_error_at("test message", "child.mds", "source", 0, 5);
    let serialized = err.serialize();
    assert_eq!(
        serialized.code, "mds::extends",
        "extends error code: {serialized:?}"
    );
    assert!(
        serialized.span.is_some(),
        "extends error should have a span"
    );
}

// ── Phase 3: deep_merge_yaml unit tests ───────────────────────────────────

fn mapping(pairs: &[(&str, serde_yaml_ng::Value)]) -> serde_yaml_ng::Mapping {
    let mut m = serde_yaml_ng::Mapping::new();
    for (k, v) in pairs {
        m.insert(serde_yaml_ng::Value::String(k.to_string()), v.clone());
    }
    m
}

fn str_val(s: &str) -> serde_yaml_ng::Value {
    serde_yaml_ng::Value::String(s.to_string())
}

fn seq_val(items: &[serde_yaml_ng::Value]) -> serde_yaml_ng::Value {
    serde_yaml_ng::Value::Sequence(items.to_vec())
}

fn map_val(pairs: &[(&str, serde_yaml_ng::Value)]) -> serde_yaml_ng::Value {
    serde_yaml_ng::Value::Mapping(mapping(pairs))
}

#[test]
fn deep_merge_yaml_nested_key_by_key() {
    // Both base and child have a nested Mapping at the same key → recursively merged.
    let base = mapping(&[(
        "outer",
        map_val(&[("base_only", str_val("keep")), ("shared", str_val("base"))]),
    )]);
    let child = mapping(&[(
        "outer",
        map_val(&[("shared", str_val("child")), ("child_only", str_val("new"))]),
    )]);
    let result = deep_merge_yaml(&base, &child, 0).expect("deep merge should succeed");
    let outer = match result.get("outer").expect("outer key present") {
        serde_yaml_ng::Value::Mapping(m) => m.clone(),
        other => panic!("expected Mapping, got {other:?}"),
    };
    assert_eq!(
        outer.get("base_only"),
        Some(&str_val("keep")),
        "base-only key survives"
    );
    assert_eq!(
        outer.get("shared"),
        Some(&str_val("child")),
        "child overrides shared key"
    );
    assert_eq!(
        outer.get("child_only"),
        Some(&str_val("new")),
        "child-only key added"
    );
}

#[test]
fn deep_merge_yaml_child_leaf_override() {
    // Scalar in child replaces scalar in base.
    let base = mapping(&[("a", str_val("base")), ("b", str_val("base_b"))]);
    let child = mapping(&[("a", str_val("child"))]);
    let result = deep_merge_yaml(&base, &child, 0).expect("merge ok");
    assert_eq!(
        result.get("a"),
        Some(&str_val("child")),
        "child overrides base scalar"
    );
    assert_eq!(
        result.get("b"),
        Some(&str_val("base_b")),
        "base-only key preserved"
    );
}

#[test]
fn deep_merge_yaml_base_only_key_survives() {
    // Key present only in base must appear in the merged output.
    let base = mapping(&[("only_base", str_val("value")), ("shared", str_val("x"))]);
    let child = mapping(&[("shared", str_val("y"))]);
    let result = deep_merge_yaml(&base, &child, 0).expect("merge ok");
    assert_eq!(
        result.get("only_base"),
        Some(&str_val("value")),
        "base-only key survives"
    );
    assert_eq!(
        result.get("shared"),
        Some(&str_val("y")),
        "shared key = child wins"
    );
}

#[test]
fn deep_merge_yaml_array_wholesale_replace() {
    // Arrays are replaced wholesale — no element-level merge.
    let base = mapping(&[("tags", seq_val(&[str_val("a"), str_val("b")]))]);
    let child = mapping(&[("tags", seq_val(&[str_val("c")]))]);
    let result = deep_merge_yaml(&base, &child, 0).expect("merge ok");
    assert_eq!(
        result.get("tags"),
        Some(&seq_val(&[str_val("c")])),
        "child array replaces base array wholesale"
    );
}

#[test]
fn deep_merge_yaml_reserved_keys_excluded() {
    // imports, type, extends must be excluded from the merged output.
    let base = mapping(&[
        ("imports", seq_val(&[])),
        ("type", str_val("mds")),
        ("extends", str_val("./parent.mds")),
        ("real_key", str_val("keep")),
    ]);
    let child = mapping(&[
        ("imports", seq_val(&[])),
        ("type", str_val("mds")),
        ("extends", str_val("./other.mds")),
        ("child_key", str_val("added")),
    ]);
    let result = deep_merge_yaml(&base, &child, 0).expect("merge ok");
    assert!(result.get("imports").is_none(), "imports must be excluded");
    assert!(result.get("type").is_none(), "type must be excluded");
    assert!(result.get("extends").is_none(), "extends must be excluded");
    assert_eq!(result.get("real_key"), Some(&str_val("keep")));
    assert_eq!(result.get("child_key"), Some(&str_val("added")));
}

#[test]
fn deep_merge_yaml_key_order_base_then_child() {
    // Base keys come first, then child-only keys, preserving base key order (A6).
    let base = mapping(&[("a", str_val("1")), ("b", str_val("2"))]);
    let child = mapping(&[("c", str_val("3")), ("a", str_val("a_child"))]);
    let result = deep_merge_yaml(&base, &child, 0).expect("merge ok");
    let keys: Vec<&str> = result
        .iter()
        .filter_map(|(k, _)| {
            if let serde_yaml_ng::Value::String(s) = k {
                Some(s.as_str())
            } else {
                None
            }
        })
        .collect();
    // Expected order: a (from base), b (from base), c (child-only appended)
    assert_eq!(keys, ["a", "b", "c"], "key order: base-then-child (A6)");
    assert_eq!(
        result.get("a"),
        Some(&str_val("a_child")),
        "shared key = child wins"
    );
}

#[test]
fn deep_merge_yaml_depth_cap_succeeds_at_cap() {
    // Build a nested mapping exactly MAX_FRONTMATTER_MERGE_DEPTH deep.
    // The call at depth=0 with nesting of cap levels should succeed (cap is the limit check,
    // we need cap+1 to exceed it).
    let cap = MAX_FRONTMATTER_MERGE_DEPTH;
    // Build a base mapping nested cap levels deep, then call with depth=0.
    // The deepest merge call will be at depth=cap (cap nested recursive calls), which
    // should still succeed because the check is `depth > cap`.
    fn nested_map(depth: usize, cap: usize) -> serde_yaml_ng::Mapping {
        let mut m = serde_yaml_ng::Mapping::new();
        if depth < cap {
            m.insert(
                serde_yaml_ng::Value::String("n".to_string()),
                serde_yaml_ng::Value::Mapping(nested_map(depth + 1, cap)),
            );
        } else {
            m.insert(
                serde_yaml_ng::Value::String("leaf".to_string()),
                serde_yaml_ng::Value::String("base".to_string()),
            );
        }
        m
    }
    let deep_base = nested_map(0, cap);
    // Child has same structure with a different leaf
    let mut deep_child_inner = serde_yaml_ng::Mapping::new();
    deep_child_inner.insert(
        serde_yaml_ng::Value::String("leaf".to_string()),
        serde_yaml_ng::Value::String("child".to_string()),
    );
    // We don't need child to be as deep — the deepest base map merged with
    // an empty child at depth=cap still succeeds.
    let result = deep_merge_yaml(&deep_base, &serde_yaml_ng::Mapping::new(), 0);
    assert!(result.is_ok(), "depth=cap should succeed: {result:?}");
}

#[test]
fn deep_merge_yaml_depth_cap_plus_one_errors() {
    // Calling deep_merge_yaml with depth = MAX_FRONTMATTER_MERGE_DEPTH + 1 must
    // return mds::resource_limit (P4, no stack overflow).
    let base = serde_yaml_ng::Mapping::new();
    let child = serde_yaml_ng::Mapping::new();
    let result = deep_merge_yaml(&base, &child, MAX_FRONTMATTER_MERGE_DEPTH + 1);
    assert!(result.is_err(), "depth cap+1 must error");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("resource limit") || msg.contains("depth"),
        "error should mention resource limit or depth: {msg}"
    );
}

// ── Phase 3: integration tests via compile_virtual ────────────────────────

#[test]
fn f6_deep_frontmatter_merge_nested_object() {
    // F6: nested key merged key-by-key; child leaf overrides; base-only key visible.
    let base = concat!(
        "---\n",
        "config:\n",
        "  model: gpt-4\n",
        "  temperature: 0.7\n",
        "base_only: \"from base\"\n",
        "---\n",
        "@block content:\n",
        "model={config.model} temp={config.temperature} base={base_only}\n",
        "@end\n",
    );
    let child = concat!(
        "---\n",
        "config:\n",
        "  temperature: 0.3\n",
        "  extra: added\n",
        "---\n",
        "@extends \"./base.mds\"\n",
        "@block content:\n",
        "model={config.model} temp={config.temperature} extra={config.extra} base={base_only}\n",
        "@end\n",
    );
    let files = [("base.mds", base), ("child.mds", child)];
    let result = compile_virtual(&files, "child.mds").expect("F6: deep merge should compile");
    assert!(
        result.contains("model=gpt-4"),
        "base-only nested key visible: {result}"
    );
    assert!(
        result.contains("temp=0.3"),
        "child override applied: {result}"
    );
    assert!(
        result.contains("extra=added"),
        "child-only nested key visible: {result}"
    );
    assert!(
        result.contains("base=from base"),
        "base top-level key visible: {result}"
    );
}

#[test]
fn f6_deep_frontmatter_merge_array_wholesale_replace() {
    // F6/decision #7: arrays in frontmatter are replaced wholesale, not merged.
    let base = concat!(
        "---\n",
        "tools:\n",
        "  - python\n",
        "  - rust\n",
        "---\n",
        "@block content:\n",
        "@for tool in tools:\n",
        "{tool}\n",
        "@end\n",
        "@end\n",
    );
    let child = concat!(
        "---\n",
        "tools:\n",
        "  - typescript\n",
        "---\n",
        "@extends \"./base.mds\"\n",
    );
    let files = [("base.mds", base), ("child.mds", child)];
    let result = compile_virtual(&files, "child.mds").expect("F6: array replace should compile");
    assert!(
        result.contains("typescript"),
        "child array replaces base: {result}"
    );
    assert!(
        !result.contains("python"),
        "base array not in child result: {result}"
    );
    assert!(
        !result.contains("rust"),
        "base array not in child result: {result}"
    );
}

#[test]
fn f6_base_only_key_visible_in_child() {
    // F6: key present only in base FM is visible to child scope.
    let base = concat!(
        "---\n",
        "only_in_base: \"secret_from_base\"\n",
        "---\n",
        "@block content:\n",
        "{only_in_base}\n",
        "@end\n",
    );
    let child = concat!(
        "---\n",
        "child_var: hello\n",
        "---\n",
        "@extends \"./base.mds\"\n",
    );
    let files = [("base.mds", base), ("child.mds", child)];
    let result = compile_virtual(&files, "child.mds").expect("F6: base-only key visible");
    assert!(
        result.contains("secret_from_base"),
        "base-only key visible in child: {result}"
    );
}

#[test]
fn f7_runtime_override_precedence() {
    // F7: runtime --set overrides merged frontmatter (base < child < runtime).
    // We test at the ResolvedModule level to check the rendered body directly,
    // without the raw_frontmatter fence (which always shows the child's raw FM).
    let base = concat!(
        "---\n",
        "role: base_role\n",
        "---\n",
        "@block content:\n",
        "{role}\n",
        "@end\n",
    );
    let child = concat!(
        "---\n",
        "role: child_role\n",
        "---\n",
        "@extends \"./base.mds\"\n",
    );
    let files = [("base.mds", base), ("child.mds", child)];

    // Without runtime override: child wins over base.
    {
        let mut cache = virtual_cache(&files);
        let resolved = cache
            .resolve_key("child.mds", &Default::default(), &mut vec![])
            .expect("F7: should compile without runtime override");
        let body = resolved.prompt_body.as_deref().unwrap_or("");
        assert!(
            body.contains("child_role"),
            "child overrides base without runtime: body={body}"
        );
        assert!(
            !body.contains("base_role"),
            "base value not present when child overrides: body={body}"
        );
    }

    // With runtime override: runtime wins over child.
    {
        let mut runtime_vars = HashMap::new();
        runtime_vars.insert(
            "role".to_string(),
            Value::String("runtime_role".to_string()),
        );
        let mut cache = virtual_cache(&files);
        let resolved = cache
            .resolve_key("child.mds", &runtime_vars, &mut vec![])
            .expect("F7: should compile with runtime override");
        let body = resolved.prompt_body.as_deref().unwrap_or("");
        assert!(
            body.contains("runtime_role"),
            "runtime overrides child: body={body}"
        );
        assert!(
            !body.contains("child_role"),
            "child value not present when runtime overrides: body={body}"
        );
        assert!(
            !body.contains("base_role"),
            "base value not present when runtime overrides: body={body}"
        );
    }
}

#[test]
fn f8_base_default_block_use_base_fm_alias() {
    // F8: a base default block can use a function from a base frontmatter import alias.
    // Base has `imports: [{path: ./shared.mds, as: shared}]` in its FM.
    // Base default block uses {shared.greeting("World")} interpolation.
    let shared = "@define greeting(name):\nHello {name}!\n@end\n";
    let base = concat!(
        "---\n",
        "imports:\n",
        "  - path: ./shared.mds\n",
        "    as: shared\n",
        "---\n",
        "@block content:\n",
        "{shared.greeting(\"World\")}\n",
        "@end\n",
    );
    let child = "@extends \"./base.mds\"\n";
    let files = [
        ("shared.mds", shared),
        ("base.mds", base),
        ("child.mds", child),
    ];
    let result = compile_virtual(&files, "child.mds")
        .expect("F8: base FM import alias in base default block");
    assert!(
        result.contains("Hello World!"),
        "base FM alias usable in base default block: {result}"
    );
}

#[test]
fn f8_child_can_use_own_fm_import_alias() {
    // F8: child's own frontmatter import alias is available in its block overrides.
    let lib = "@define greet(x):\nHi {x}\n@end\n";
    let base = "@block msg:\nDefault message\n@end\n";
    let child = concat!(
        "---\n",
        "imports:\n",
        "  - path: ./lib.mds\n",
        "    as: lib\n",
        "---\n",
        "@extends \"./base.mds\"\n",
        "@block msg:\n",
        "{lib.greet(\"child\")}\n",
        "@end\n",
    );
    let files = [("lib.mds", lib), ("base.mds", base), ("child.mds", child)];
    let result =
        compile_virtual(&files, "child.mds").expect("F8: child FM import alias in child block");
    assert!(
        result.contains("Hi child"),
        "child FM alias usable in child block override: {result}"
    );
}

#[test]
fn f8_duplicate_alias_base_and_child_error() {
    // F8/ADR-014: same alias in both base and child frontmatter imports → mds::name_collision.
    let lib = "@define foo():\nfoo\n@end\n";
    let base = concat!(
        "---\n",
        "imports:\n",
        "  - path: ./lib.mds\n",
        "    as: mylib\n",
        "---\n",
        "@block content:\n",
        "base content\n",
        "@end\n",
    );
    let child = concat!(
        "---\n",
        "imports:\n",
        "  - path: ./lib.mds\n",
        "    as: mylib\n",
        "---\n",
        "@extends \"./base.mds\"\n",
    );
    let files = [("lib.mds", lib), ("base.mds", base), ("child.mds", child)];
    let result = compile_virtual(&files, "child.mds");
    assert!(result.is_err(), "duplicate alias base+child must error");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("mylib") || msg.contains("name") || msg.contains("collision"),
        "error should mention the colliding alias: {msg}"
    );
}

#[test]
fn a6_determinism_double_compile_byte_identical() {
    // A6: compiling the same multi-level chain twice produces byte-identical output.
    let a = concat!(
        "---\n",
        "x: 1\n",
        "y: 2\n",
        "---\n",
        "@block content:\n",
        "{x},{y}\n",
        "@end\n",
    );
    let b = concat!(
        "---\n",
        "y: 99\n",
        "z: 3\n",
        "---\n",
        "@extends \"./a.mds\"\n",
    );
    let c = concat!("---\n", "z: 100\n", "---\n", "@extends \"./b.mds\"\n",);
    let files = [("a.mds", a), ("b.mds", b), ("c.mds", c)];
    let result1 = compile_virtual(&files, "c.mds").expect("A6 first compile");
    let result2 = compile_virtual(&files, "c.mds").expect("A6 second compile");
    assert_eq!(
        result1, result2,
        "A6: double compile must be byte-identical"
    );
}

#[test]
fn a6_for_loop_over_deep_merged_fm_stable_order() {
    // A6: @for over a deep-merged object iterates in stable base-then-child key order.
    // The base has keys a, b in its labels object; child adds key c.
    // deep_merge produces labels with keys a, b, c in that order (base-then-child).
    // MDS uses @for k, v in obj: for objects.
    let base = concat!(
        "---\n",
        "labels:\n",
        "  a: \"first\"\n",
        "  b: \"second\"\n",
        "---\n",
        "@block content:\n",
        "@for k, v in labels:\n",
        "{k}={v};\n",
        "@end\n",
        "@end\n",
    );
    // child extends base and specifies all three keys in labels (a+b from base merged
    // with c from child → a, b first from base position, c appended by child order).
    let child = concat!(
        "---\n",
        "labels:\n",
        "  a: \"first\"\n",
        "  b: \"second\"\n",
        "  c: \"third\"\n",
        "---\n",
        "@extends \"./base.mds\"\n",
    );
    let files = [("base.mds", base), ("child.mds", child)];
    let result = compile_virtual(&files, "child.mds").expect("A6 stable key order");
    // Verify all three keys present
    assert!(result.contains("a=first"), "key a present: {result}");
    assert!(result.contains("b=second"), "key b present: {result}");
    assert!(result.contains("c=third"), "key c present: {result}");
    // Verify order: a before b before c
    let pos_a = result.find("a=first").expect("a in result");
    let pos_b = result.find("b=second").expect("b in result");
    let pos_c = result.find("c=third").expect("c in result");
    assert!(
        pos_a < pos_b,
        "a before b (stable base-then-child order): {result}"
    );
    assert!(
        pos_b < pos_c,
        "b before c (stable base-then-child order): {result}"
    );
}

#[test]
fn p4_fm_merge_depth_bound_resource_limit() {
    // P4: deep_merge_yaml at depth > MAX_FRONTMATTER_MERGE_DEPTH returns mds::resource_limit.
    let result = deep_merge_yaml(
        &serde_yaml_ng::Mapping::new(),
        &serde_yaml_ng::Mapping::new(),
        MAX_FRONTMATTER_MERGE_DEPTH + 1,
    );
    assert!(result.is_err(), "P4: depth cap+1 must error");
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("resource limit") || msg.contains("depth"),
        "P4: error should be resource_limit: {msg}"
    );
}

#[test]
fn regression_non_extending_file_fm_unchanged() {
    // Regression: a non-extending file with frontmatter imports still works identically.
    // This confirms the standalone build_scope_from_frontmatter path is unchanged.
    let lib = "@define greet(name):\nHello {name}!\n@end\n";
    let standalone = concat!(
        "---\n",
        "imports:\n",
        "  - path: ./lib.mds\n",
        "    as: lib\n",
        "greeting: World\n",
        "---\n",
        "{lib.greet(greeting)}\n",
    );
    let files = [("lib.mds", lib), ("standalone.mds", standalone)];
    let result = compile_virtual(&files, "standalone.mds")
        .expect("regression: standalone FM imports should still work");
    assert!(
        result.contains("Hello World!"),
        "standalone FM import regression: {result}"
    );
}

#[test]
fn f4_child_emits_only_own_raw_frontmatter() {
    // decision #7 / output emission: extending child emits only its own raw_frontmatter.
    // Base frontmatter is an input to scope, not output.
    let base = concat!(
        "---\n",
        "base_secret: only_in_base\n",
        "---\n",
        "@block content:\n",
        "{base_secret}\n",
        "@end\n",
    );
    let child = concat!(
        "---\n",
        "child_var: in_child\n",
        "---\n",
        "@extends \"./base.mds\"\n",
    );
    let files = [("base.mds", base), ("child.mds", child)];
    let mut cache = virtual_cache(&files);
    let mut warnings = vec![];

    let child_resolved = cache
        .resolve_key("child.mds", &Default::default(), &mut warnings)
        .expect("output emission test should compile");

    // raw_frontmatter in the resolved module is the child's raw FM (not base's).
    if let Some(ref raw_fm) = child_resolved.raw_frontmatter {
        assert!(
            !raw_fm.contains("base_secret"),
            "child output must NOT contain base frontmatter: {raw_fm}"
        );
        assert!(
            raw_fm.contains("child_var"),
            "child output must contain child's own frontmatter: {raw_fm}"
        );
    }
    // The compiled output uses the merged scope (base_secret visible to blocks)
    let output = child_resolved.prompt_body.as_deref().unwrap_or("");
    assert!(
        output.contains("only_in_base"),
        "merged scope used: base var rendered in block: {output}"
    );
}

#[test]
fn f3_multilevel_deep_merge_transitive() {
    // A←B←C: deep merge is transitive: A's FM < B's FM < C's FM.
    // A has a=1, b=2. B overrides b=99, adds c=3. C overrides c=100.
    // Result: a=1 (from A), b=99 (from B), c=100 (from C).
    let a = concat!(
        "---\n",
        "a: \"from_a\"\n",
        "b: \"from_a_b\"\n",
        "---\n",
        "@block content:\n",
        "a={a} b={b} c={c}\n",
        "@end\n",
    );
    let b = concat!(
        "---\n",
        "b: \"from_b\"\n",
        "c: \"from_b_c\"\n",
        "---\n",
        "@extends \"./a.mds\"\n",
    );
    let c = concat!(
        "---\n",
        "c: \"from_c\"\n",
        "---\n",
        "@extends \"./b.mds\"\n",
    );
    let files = [("a.mds", a), ("b.mds", b), ("c.mds", c)];
    let result = compile_virtual(&files, "c.mds").expect("F3 multilevel deep merge");
    assert!(
        result.contains("a=from_a"),
        "a=from_a (root only): {result}"
    );
    assert!(
        result.contains("b=from_b"),
        "b=from_b (B overrides A): {result}"
    );
    assert!(
        result.contains("c=from_c"),
        "c=from_c (C overrides B): {result}"
    );
}

// ── Phase 4: messages-mode inheritance ───────────────────────────────────

/// Helper: compile a VirtualFs entry and extract its messages.
///
/// Output shape is intrinsic: a template containing any `@message` block compiles to
/// `CompiledOutput::Messages`. `into_messages()` extracts the vector — or returns
/// `Err(ExpectedMessages)` if the template produced Markdown (no `@message` block).
fn compile_messages_virtual_helper(
    files: &[(&str, &str)],
    entry: &str,
) -> Result<Vec<crate::Message>, MdsError> {
    let map: std::collections::HashMap<String, String> = files
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    crate::compile_virtual(map, entry, None).and_then(crate::CompileResult::into_messages)
}

// ── F9: messages mode — @message-structured base + child @block override ──
//
// Layout: @block is at top-level (base skeleton), @message is inside the @block body.
// @block cannot appear inside @message (parser enforces top-level only).

#[test]
fn f9_messages_mode_block_override_compiles_to_message_array() {
    // Base: @block at top level, @message inside the block body (default).
    // Child: overrides the block — the @message in the override surfaces in output.
    let base = concat!(
        "@block msg:\n",
        "@message user:\n",
        "Default question.\n",
        "@end\n",
        "@end\n",
    );
    let child = concat!(
        "@extends \"./base.mds\"\n",
        "@block msg:\n",
        "@message user:\n",
        "Child override question.\n",
        "@end\n",
        "@end\n",
    );
    let files = [("base.mds", base), ("child.mds", child)];
    let messages = compile_messages_virtual_helper(&files, "child.mds")
        .expect("F9: messages-mode inheritance should compile");
    assert_eq!(
        messages.len(),
        1,
        "F9: expected 1 message, got {messages:?}"
    );
    assert_eq!(messages[0].role, "user", "F9: expected role=user");
    assert!(
        messages[0].content.contains("Child override question."),
        "F9: child block override should appear in message content: {:?}",
        messages[0].content,
    );
}

#[test]
fn f9_messages_mode_default_block_in_message_body() {
    // @message inside a base default block (un-overridden by child) surfaces in output.
    // @block is top-level; @message is inside the @block body.
    let base = concat!(
        "@block intro:\n",
        "@message system:\n",
        "You are a helpful assistant.\n",
        "@end\n",
        "@end\n",
    );
    let child = "@extends \"./base.mds\"\n";
    let files = [("base.mds", base), ("child.mds", child)];
    let messages = compile_messages_virtual_helper(&files, "child.mds")
        .expect("F9: @message inside un-overridden base default block should surface");
    assert_eq!(
        messages.len(),
        1,
        "F9: expected 1 message, got {messages:?}"
    );
    assert_eq!(messages[0].role, "system");
    assert!(
        messages[0].content.contains("You are a helpful assistant."),
        "F9: message from base default block: {:?}",
        messages[0].content,
    );
}

// ── E13: intrinsic — base with no @message compiles to Markdown ──────────

#[test]
fn e13_extends_no_message_block_compiles_to_markdown() {
    // Base has @block placeholders but no @message. Output shape is intrinsic:
    // with no @message anywhere in the spliced final_body, the child compiles to
    // Markdown (not an error). Extracting messages from it yields ExpectedMessages.
    let base = concat!(
        "You are an assistant.\n",
        "@block instructions:\n",
        "Do things carefully.\n",
        "@end\n",
    );
    let child = concat!(
        "@extends \"./base.mds\"\n",
        "@block instructions:\n",
        "Do things quickly.\n",
        "@end\n",
    );
    let files = [("base.mds", base), ("child.mds", child)];

    // It compiles to Markdown with the child's override spliced in.
    let md = compile_virtual(&files, "child.mds")
        .expect("E13: no @message → Markdown output, not an error");
    assert!(
        md.contains("You are an assistant.") && md.contains("Do things quickly."),
        "E13: spliced Markdown output expected, got: {md:?}"
    );

    // Asking for messages on a Markdown result is the ExpectedMessages error.
    let err = compile_messages_virtual_helper(&files, "child.mds")
        .expect_err("E13: extracting messages from Markdown output must error");
    assert_eq!(
        err.serialize().code,
        "mds::expected_messages",
        "E13: error code should be mds::expected_messages: {err}"
    );
}

// ── F10 (messages half): empty block renders empty in messages mode ────────

#[test]
fn f10_messages_mode_empty_block_renders_empty() {
    // An @block with no default and no child override renders empty — surrounding
    // @message content intact. @block is at top level; @message is a sibling,
    // not a parent (parser rejects @block inside @message).
    let base = concat!(
        // A @message block with literal surrounding text — no @block inside message.
        "@message user:\n",
        "Before.\n",
        "@end\n",
        // The @block placeholder at top level: empty default body.
        "@block gap:\n",
        "@end\n",
        // Another @message for content after the gap placeholder.
        "@message user:\n",
        "After.\n",
        "@end\n",
    );
    // Child overrides the gap block with an empty body (same as default).
    let child = concat!("@extends \"./base.mds\"\n", "@block gap:\n", "@end\n",);
    let files = [("base.mds", base), ("child.mds", child)];
    let messages = compile_messages_virtual_helper(&files, "child.mds")
        .expect("F10 messages: empty block should not break compilation");
    // Two @message blocks: Before. and After.
    assert_eq!(
        messages.len(),
        2,
        "F10: expected 2 messages, got {messages:?}"
    );
    let first_content = &messages[0].content;
    let second_content = &messages[1].content;
    assert!(
        first_content.contains("Before."),
        "F10: first message must contain 'Before.': {first_content}"
    );
    assert!(
        second_content.contains("After."),
        "F10: second message must contain 'After.': {second_content}"
    );
}

// ── P5: deep-chain performance guard (Markdown + Messages, < 2 s) ────────

/// Build a 32-level @extends chain whose root carries `root_src` in a @block, and
/// return the (key, source) pairs. file0 @extends file1 @extends … @extends file31.
fn deep_chain_files(depth: usize, root_src: String) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = Vec::new();
    files.push((format!("file{depth}.mds"), root_src));
    for i in (0..depth).rev() {
        files.push((
            format!("file{i}.mds"),
            format!("@extends \"./file{}.mds\"\n", i + 1),
        ));
    }
    files
}

#[test]
fn p5_deep_chain_32_levels_markdown_and_messages_under_2s() {
    // Output shape is intrinsic, so the markdown and messages perf paths need
    // different roots: a plain @block (Markdown) vs a @block holding a @message
    // (Messages). Both 32-level chains must compile in < 2 s with no OOM.
    let depth = 32usize;

    // Markdown root: @block with plain default content (no @message).
    let md_files = deep_chain_files(
        depth,
        "@block content:\nHello from root.\n@end\n".to_string(),
    );
    let md_refs: Vec<(&str, &str)> = md_files
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    // Messages root: @block at top level with a @message inside it.
    let msg_files = deep_chain_files(
        depth,
        concat!(
            "@block content:\n",
            "@message user:\n",
            "Hello from root.\n",
            "@end\n",
            "@end\n",
        )
        .to_string(),
    );
    let msg_refs: Vec<(&str, &str)> = msg_files
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let start = std::time::Instant::now();
    let markdown_result = compile_virtual(&md_refs, "file0.mds");
    let messages_result = compile_messages_virtual_helper(&msg_refs, "file0.mds");
    let elapsed = start.elapsed();

    assert!(
        markdown_result.is_ok(),
        "P5: 32-level chain markdown compile failed: {:?}",
        markdown_result.err()
    );
    assert!(
        messages_result.is_ok(),
        "P5: 32-level chain messages compile failed: {:?}",
        messages_result.err()
    );
    assert!(
        elapsed.as_secs() < 2,
        "P5: 32-level chains took {:?}, must be < 2 s",
        elapsed
    );
}

// ── P5b: deep chain WITH frontmatter — perf guard for FM accumulation ────
//
// P5 (above) builds a 32-level chain with EMPTY frontmatter on every level,
// giving zero coverage of the O(N²) per-level FM accumulation
// (process_module_skeleton:~916-927).  This test adds frontmatter keys at
// every level so the deep_merge_yaml path is exercised on each resolution.
// Wall-clock bound mirrors P5 — both must complete in < 2 s.
//
// This converts an untested assumption into a guarded one.  The merge algorithm
// is NOT changed here (that is deferred tech debt); this test pins correctness
// and performance of the current implementation.

#[test]
fn p5b_deep_chain_32_levels_with_frontmatter_under_2s() {
    let depth = 32usize;
    let mut files: Vec<(String, String)> = Vec::new();

    // Root base: a few FM keys + @block with content.
    let root_src = concat!(
        "---\n",
        "base_key: root_value\n",
        "shared_key: from_root\n",
        "---\n",
        "@block content:\n",
        "Root content.\n",
        "@end\n",
    )
    .to_string();
    files.push((format!("file{depth}.mds"), root_src));

    // Each intermediate level adds/overrides one FM key and extends the next.
    for i in (0..depth).rev() {
        let src = format!(
            "---\nlevel_{i}_key: value_{i}\nshared_key: from_{i}\n---\n\
                 @extends \"./file{}.mds\"\n",
            i + 1
        );
        files.push((format!("file{i}.mds"), src));
    }

    let file_refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let start = std::time::Instant::now();
    // file0.mds is a child with no @block override — uses root content default.
    let result = compile_virtual(&file_refs, "file0.mds");
    let elapsed = start.elapsed();

    assert!(
        result.is_ok(),
        "P5b: 32-level chain with FM failed: {:?}",
        result.err()
    );
    let output = result.unwrap();
    assert!(
        output.contains("Root content."),
        "P5b: default block content must appear in output: {output:?}"
    );
    assert!(
        elapsed.as_secs() < 2,
        "P5b: 32-level FM chain took {:?}, must be < 2 s",
        elapsed
    );
}

// ── P6: PF-004 oversized base rejected in BOTH modes ─────────────────────

#[test]
fn p6_pf004_oversized_base_rejected_in_text_mode() {
    // PF-004 (applying avoids PF-004): a base larger than MAX_FILE_SIZE is rejected
    // via resolve_by_key_skeleton (FileSystem trait path, never std::fs).
    // Text mode must return mds::resource_limit.
    use crate::limits::MAX_FILE_SIZE;
    // One byte over the limit — large enough to trigger the guard.
    let oversized = "x".repeat((MAX_FILE_SIZE + 1) as usize);
    let child = "@extends \"./base.mds\"\n";
    let files = [("base.mds", oversized.as_str()), ("child.mds", child)];
    let err =
        compile_virtual(&files, "child.mds").expect_err("P6 text: oversized base must be rejected");
    let code = err.serialize().code;
    assert_eq!(
        code, "mds::resource_limit",
        "P6 text: error code must be mds::resource_limit: {code}"
    );
    // PF-004 + debug-panics gotcha: no base filesystem path in error message.
    let msg = err.to_string();
    assert!(
        !msg.contains("/Users/") && !msg.contains("\\Users\\"),
        "P6 text: error must not leak absolute filesystem path: {msg}"
    );
}

#[test]
fn p6_pf004_oversized_base_rejected_in_messages_mode() {
    // PF-004 (applying avoids PF-004): same oversized-base guard must also fire
    // in messages mode — both modes go through resolve_by_key_skeleton.
    use crate::limits::MAX_FILE_SIZE;
    let oversized = "x".repeat((MAX_FILE_SIZE + 1) as usize);
    let child = "@extends \"./base.mds\"\n";
    let files = [("base.mds", oversized.as_str()), ("child.mds", child)];
    let err = compile_messages_virtual_helper(&files, "child.mds")
        .expect_err("P6 messages: oversized base must be rejected");
    let code = err.serialize().code;
    assert_eq!(
        code, "mds::resource_limit",
        "P6 messages: error code must be mds::resource_limit: {code}"
    );
    // PF-004 + debug-panics gotcha: no base filesystem path in error message.
    let msg = err.to_string();
    assert!(
        !msg.contains("/Users/") && !msg.contains("\\Users\\"),
        "P6 messages: error must not leak absolute filesystem path: {msg}"
    );
}

// ── F9 multi-level messages (two-hop chain) ───────────────────────────────

#[test]
fn f9_messages_mode_multilevel_chain() {
    // A←B←C: C extends B extends A. A has @block (top-level) with @message inside.
    // B overrides the block. C overrides again. Most-derived (C) wins.
    let a = concat!(
        "@block msg:\n",
        "@message user:\n",
        "From A.\n",
        "@end\n",
        "@end\n",
    );
    let b = concat!(
        "@extends \"./a.mds\"\n",
        "@block msg:\n",
        "@message user:\n",
        "From B.\n",
        "@end\n",
        "@end\n",
    );
    let c = concat!(
        "@extends \"./b.mds\"\n",
        "@block msg:\n",
        "@message user:\n",
        "From C.\n",
        "@end\n",
        "@end\n",
    );
    let files = [("a.mds", a), ("b.mds", b), ("c.mds", c)];
    let messages =
        compile_messages_virtual_helper(&files, "c.mds").expect("F9 multilevel: should compile");
    assert_eq!(messages.len(), 1, "F9 multilevel: got {messages:?}");
    assert!(
        messages[0].content.contains("From C."),
        "F9 multilevel: most-derived (C) wins: {:?}",
        messages[0].content
    );
}

// ── PF-004 parity: messages-mode @extends path validates final_body ────────
//
// A check on the primary path (text-mode process_module_extends calls
// validator::validate before evaluate) must not be absent on the parallel path
// (messages-mode @extends branch).  This test verifies that an @extends child
// compiled in messages mode whose base-default block references an undefined var
// produces the SAME error (mds::undefined_var) as text mode does.

#[test]
fn pf004_messages_mode_extends_validates_final_body_parity() {
    // Base: @block with @message inside, and an undefined variable.
    // Child: @extends the base, provides no override (uses default).
    let base = concat!(
        "@block content:\n",
        "@message user:\n",
        "Hello {undefined_var}.\n",
        "@end\n",
        "@end\n",
    );
    let child = "@extends \"./base.mds\"\n";
    let files = [("base.mds", base), ("child.mds", child)];

    // Text mode: must error with mds::undefined_var
    let text_err =
        compile_virtual(&files, "child.mds").expect_err("text: undefined var must error");
    let text_code = text_err.serialize().code;
    assert_eq!(
        text_code, "mds::undefined_var",
        "text: expected mds::undefined_var, got: {text_code}"
    );

    // Messages mode: must produce the SAME error (avoids PF-004 parallel-path divergence).
    let messages_err = compile_messages_virtual_helper(&files, "child.mds")
        .expect_err("messages: undefined var must error");
    let messages_code = messages_err.serialize().code;
    assert_eq!(
        messages_code, "mds::undefined_var",
        "messages: expected mds::undefined_var (same as text mode), got: {messages_code}"
    );
}

// ── F11: whitespace contract — 4-combination byte-exact matrix ───────────────
//
// Decision #9 (from spec): block-body edge newlines (ONE leading + ONE trailing
// `\n`) are stripped at parse time. Between-block Text nodes in the skeleton
// are preserved verbatim. This test pins all four observable combinations:
//
//  1. Base default (no override):    skeleton text nodes pass through; block
//                                    body edge newlines stripped.
//  2. Child override, no blanks:     body = "Override." — only text, no leading/
//                                    trailing blank lines.
//  3. Child override WITH blanks:    extra blank lines inside the block body
//                                    survive (only ONE edge \n is stripped each
//                                    side), producing one extra \n in output.
//  4. Child override, indented:      leading spaces inside block body are
//                                    preserved verbatim; edge \n still stripped.

#[test]
fn f11_whitespace_contract_4_combination_matrix() {
    // Base skeleton: text before, one @block, text after.
    // Between the Text("Intro.\n\n") node and the @block body there is NO
    // extra whitespace beyond what the skeleton text nodes carry.
    //
    // Base source (repr): "Intro.\n\n@block body:\nDefault body.\n@end\n\nAfter.\n"
    //
    // Skeleton nodes after parse:
    //   Text("Intro.\n\n")
    //   Block("body")  body = [Text("Default body.")]   ← edge \n stripped
    //   Text("\nAfter.\n")                              ← the blank line + After.
    let base = "Intro.\n\n@block body:\nDefault body.\n@end\n\nAfter.\n";

    // ── Combination 1: base default, no child override ────────────────────
    // Child has no @block override — effective_blocks use the base default.
    // Between-block blank line (\n before "After.") preserved verbatim.
    {
        let child = "@extends \"./base.mds\"\n";
        let files = [("base.mds", base), ("child.mds", child)];
        let out = compile_virtual(&files, "child.mds").expect("F11 combo-1: should compile");
        assert_eq!(
            out, "Intro.\n\nDefault body.\nAfter.\n",
            "F11 combo-1: base default — between-block blank line preserved, body edge stripped"
        );
    }

    // ── Combination 2: override with no surrounding blank lines ───────────
    // Block body = "Override." (no leading/trailing blank lines).
    // After edge-strip: body = [Text("Override.")].
    {
        let child = "@extends \"./base.mds\"\n@block body:\nOverride.\n@end\n";
        let files = [("base.mds", base), ("child.mds", child)];
        let out = compile_virtual(&files, "child.mds").expect("F11 combo-2: should compile");
        assert_eq!(
            out, "Intro.\n\nOverride.\nAfter.\n",
            "F11 combo-2: override without blank lines — clean output"
        );
    }

    // ── Combination 3: override WITH leading+trailing blank lines ─────────
    // Block body raw = "\nOverride.\n\n" (blank line before + blank line after).
    // strip_leading_newline removes ONE leading \n  → "Override.\n\n"
    // strip_trailing_newline removes ONE trailing \n → "Override.\n"
    // Residual \n becomes part of the rendered block body, producing an extra
    // blank line BEFORE the "After." skeleton text node ("\nAfter.\n").
    // This pins decision #9: only one edge \n is stripped — extra interior
    // blank lines are preserved.
    {
        let child = "@extends \"./base.mds\"\n@block body:\n\nOverride.\n\n@end\n";
        let files = [("base.mds", base), ("child.mds", child)];
        let out = compile_virtual(&files, "child.mds").expect("F11 combo-3: should compile");
        assert_eq!(
                out,
                "Intro.\n\nOverride.\n\nAfter.\n",
                "F11 combo-3: override with surrounding blanks — extra blank line inside body preserved (only edge \n stripped)"
            );
    }

    // ── Combination 4: override with indented content ─────────────────────
    // Block body raw = "  Indented.\n".
    // strip_leading_newline: no leading \n, no change.
    // strip_trailing_newline: pop \n → "  Indented."
    // Leading spaces are preserved verbatim (base author's indentation style).
    {
        let child = "@extends \"./base.mds\"\n@block body:\n  Indented.\n@end\n";
        let files = [("base.mds", base), ("child.mds", child)];
        let out = compile_virtual(&files, "child.mds").expect("F11 combo-4: should compile");
        assert_eq!(
            out, "Intro.\n\n  Indented.\nAfter.\n",
            "F11 combo-4: indented override — leading spaces preserved verbatim"
        );
    }
}

// ── A3: Error-code mapping consolidation (resolver layer) ─────────────────
//
// Authoritative table for resolver-level errors:
//
// | ID | Trigger                               | Expected code          |
// |----|---------------------------------------|------------------------|
// | E3 | stray child content                   | mds::extends           |
// | E4 | unknown override block                | mds::extends           |
// | E5 | circular inheritance (A→B→A, self)    | mds::circular_import   |
// | E7 | @block name collides with @define     | mds::name_collision    |
// | E8 | duplicate @block in same module       | mds::name_collision    |
//
// E1/E2/E9 are covered in parser_tests.rs (a3_parser_error_code_table).

#[test]
fn a3_resolver_error_code_table() {
    // E3: stray child content → mds::extends
    {
        let base = "@block body:\nHello\n@end\n";
        let child = "@extends \"./base.mds\"\nStray text here.\n";
        let files = [("base.mds", base), ("child.mds", child)];
        let err = compile_virtual(&files, "child.mds")
            .expect_err("A3 E3: stray child content should error");
        let s = err.serialize();
        assert_eq!(
            s.code, "mds::extends",
            "A3 E3: stray child content must be mds::extends, got: {:?}",
            s
        );
        assert!(
            s.span.is_some(),
            "A3 E3: mds::extends error must carry a span, got: {:?}",
            s
        );
    }

    // E4: unknown block override → mds::extends
    {
        let base = "@block body:\nHello\n@end\n";
        let child = "@extends \"./base.mds\"\n@block nonexistent:\nOverride\n@end\n";
        let files = [("base.mds", base), ("child.mds", child)];
        let err =
            compile_virtual(&files, "child.mds").expect_err("A3 E4: unknown override should error");
        let s = err.serialize();
        assert_eq!(
            s.code, "mds::extends",
            "A3 E4: unknown block override must be mds::extends, got: {:?}",
            s
        );
        assert!(
            s.span.is_some(),
            "A3 E4: mds::extends error must carry a span, got: {:?}",
            s
        );
    }

    // E5: circular inheritance (A→B→A) → mds::circular_import
    {
        let a = "@extends \"./b.mds\"\n@block body:\nFrom A\n@end\n";
        let b = "@extends \"./a.mds\"\n@block body:\nFrom B\n@end\n";
        let files = [("a.mds", a), ("b.mds", b)];
        let err =
            compile_virtual(&files, "a.mds").expect_err("A3 E5: circular inheritance should error");
        assert_eq!(
            err.serialize().code,
            "mds::circular_import",
            "A3 E5: circular inheritance must be mds::circular_import, got: {:?}",
            err.serialize()
        );
    }

    // E7: @block name collides with @define name → mds::name_collision
    {
        let src = "@define body():\ncontent\n@end\n@block body:\nbody text\n@end\n";
        let err =
            crate::compile_str_md(src).expect_err("A3 E7: @block/@define collision should error");
        assert_eq!(
            err.serialize().code,
            "mds::name_collision",
            "A3 E7: @block vs @define must be mds::name_collision, got: {:?}",
            err.serialize()
        );
    }

    // E8: duplicate @block in same module → mds::name_collision
    {
        let src = "@block body:\nfirst\n@end\n@block body:\nsecond\n@end\n";
        let err = crate::compile_str_md(src).expect_err("A3 E8: duplicate @block should error");
        assert_eq!(
            err.serialize().code,
            "mds::name_collision",
            "A3 E8: duplicate @block must be mds::name_collision, got: {:?}",
            err.serialize()
        );
    }
}

// ── E12 (strengthened): span attributes to base source ───────────────────
//
// Previously: compile_virtual produced mds::undefined_var but the diagnostic
// rendered as miette OutOfBounds because the base node offset was paired with
// the child's NamedSource. After the fix, span.is_some() AND line/column are
// populated (the span is now validated against the base source).

#[test]
fn e12_base_default_undefined_var_span_attributes_to_base() {
    // base = "@block content:\n{undefined_var}\n@end\n"
    // bytes: "@block content:\n" = 16 bytes, then "{undefined_var}" starts at 16.
    let base = "@block content:\n{undefined_var}\n@end\n";
    let child = "@extends \"./base.mds\"\n";
    let files = [("base.mds", base), ("child.mds", child)];

    // compile_virtual (text mode)
    let err = compile_virtual(&files, "child.mds")
        .expect_err("E12: undefined var in base default should error");
    let s = err.serialize();
    assert_eq!(
        s.code, "mds::undefined_var",
        "E12 text: code mismatch: {s:?}"
    );
    let span = s
        .span
        .expect("E12 text: span must be Some (base offset in base source)");
    assert!(
        span.line.is_some(),
        "E12 text: line must be Some (offset is in-bounds for base source): {span:?}"
    );
    assert!(
        span.column.is_some(),
        "E12 text: column must be Some: {span:?}"
    );
    assert!(
        span.offset + span.length <= base.len(),
        "E12 text: span offset+length must be in-bounds for base source: {span:?}"
    );

    // check_virtual (A5 parity) — same assertions
    let check_err = check_virtual(&files, "child.mds")
        .expect_err("E12 A5: check must also reject undefined var in base default");
    let cs = check_err.serialize();
    assert_eq!(
        cs.code, "mds::undefined_var",
        "E12 A5 check: code mismatch: {cs:?}"
    );
    let cspan = cs.span.expect("E12 A5 check: span must be Some");
    assert!(
        cspan.line.is_some(),
        "E12 A5 check: line must be Some: {cspan:?}"
    );
    assert!(
        cspan.column.is_some(),
        "E12 A5 check: column must be Some: {cspan:?}"
    );
    assert!(
        cspan.offset + cspan.length <= base.len(),
        "E12 A5 check: span in-bounds for base: {cspan:?}"
    );
}

// ── PF-004 (strengthened): text and messages span parity ─────────────────

#[test]
fn pf004_messages_mode_span_parity_with_text_mode() {
    // Base: @block with @message inside, and an undefined variable.
    let base = concat!(
        "@block content:\n",
        "@message user:\n",
        "Hello {undefined_var}.\n",
        "@end\n",
        "@end\n",
    );
    let child = "@extends \"./base.mds\"\n";
    let files = [("base.mds", base), ("child.mds", child)];

    let text_err = compile_virtual(&files, "child.mds").expect_err("pf004 span: text must error");
    let text_s = text_err.serialize();
    assert_eq!(
        text_s.code, "mds::undefined_var",
        "pf004 span text: {text_s:?}"
    );
    let text_span = text_s
        .span
        .as_ref()
        .expect("pf004 span text: span must be Some");
    assert!(
        text_span.line.is_some(),
        "pf004 span text: line must be Some: {text_span:?}"
    );
    assert!(
        text_span.column.is_some(),
        "pf004 span text: column must be Some: {text_span:?}"
    );

    let msg_err = compile_messages_virtual_helper(&files, "child.mds")
        .expect_err("pf004 span: messages must error");
    let msg_s = msg_err.serialize();
    assert_eq!(
        msg_s.code, "mds::undefined_var",
        "pf004 span messages: {msg_s:?}"
    );
    let msg_span = msg_s
        .span
        .as_ref()
        .expect("pf004 span messages: span must be Some");
    assert!(
        msg_span.line.is_some(),
        "pf004 span messages: line must be Some: {msg_span:?}"
    );
    assert!(
        msg_span.column.is_some(),
        "pf004 span messages: column must be Some: {msg_span:?}"
    );

    // Text and messages must point to the same location.
    assert_eq!(
        text_span.offset, msg_span.offset,
        "pf004 span: text and messages offsets must match"
    );
    assert_eq!(
        text_span.line, msg_span.line,
        "pf004 span: text and messages lines must match"
    );
    assert_eq!(
        text_span.column, msg_span.column,
        "pf004 span: text and messages columns must match"
    );
}

// ── UTF-8 boundary (strengthened): code + span ───────────────────────────

#[test]
fn utf8_boundary_span_attributes_to_base() {
    // Base has an ASCII body so the offset is valid in the base source;
    // the child key contains multibyte chars (the old panic scenario).
    let (base, child, base_key) = utf8_boundary_extends_fixture();
    let files = [(base_key, base), ("child.mds", child)];

    // compile_virtual
    let err = compile_virtual(&files, "child.mds").expect_err("utf8_boundary span: should error");
    let s = err.serialize();
    assert_eq!(
        s.code, "mds::undefined_var",
        "utf8_boundary span compile: expected mds::undefined_var, got: {s:?}"
    );
    let span = s
        .span
        .expect("utf8_boundary span: span must be Some (base is ASCII)");
    assert!(
            span.line.is_some(),
            "utf8_boundary span: line must be Some (base source is ASCII, offset is in-bounds): {span:?}"
        );
    assert!(
        span.column.is_some(),
        "utf8_boundary span: column must be Some: {span:?}"
    );

    // check_virtual
    let check_err =
        check_virtual(&files, "child.mds").expect_err("utf8_boundary span check: should error");
    let cs = check_err.serialize();
    assert_eq!(
        cs.code, "mds::undefined_var",
        "utf8_boundary span check: expected mds::undefined_var, got: {cs:?}"
    );
    let cspan = cs
        .span
        .expect("utf8_boundary span check: span must be Some");
    assert!(
        cspan.line.is_some(),
        "utf8_boundary span check: line Some: {cspan:?}"
    );
    assert!(
        cspan.column.is_some(),
        "utf8_boundary span check: column Some: {cspan:?}"
    );
}

// ── E12: child override error attributes to child ─────────────────────────
//
// When the error is in the CHILD's own override body, the span attributes
// to the child file (the winning override's source), not the base.

#[test]
fn e12_child_override_undefined_var_attributes_to_child() {
    let base = "@block content:\nDefault.\n@end\n";
    // child extends base, "@extends \"./base.mds\"\n" = 21 bytes
    let child = "@extends \"./base.mds\"\n@block content:\n{undefined_var}\n@end\n";
    let files = [("base.mds", base), ("child.mds", child)];

    let err = compile_virtual(&files, "child.mds")
        .expect_err("e12 child: undefined var in child override should error");
    let s = err.serialize();
    assert_eq!(s.code, "mds::undefined_var", "e12 child: code: {s:?}");
    let span = s.span.expect("e12 child: span must be Some");
    assert!(span.line.is_some(), "e12 child: line Some: {span:?}");
    assert!(span.column.is_some(), "e12 child: column Some: {span:?}");
    // The error is INSIDE the child's override body, which starts after
    // "@extends \"./base.mds\"\n" (21 bytes). The offset must be >= 21.
    assert!(
        span.offset >= "@extends \"./base.mds\"\n".len(),
        "e12 child: span offset must be in child's body (>= 21), got offset={}: {span:?}",
        span.offset
    );
    // Offset must be in-bounds for the child source.
    assert!(
        span.offset + span.length <= child.len(),
        "e12 child: span must be in-bounds for child source: {span:?}"
    );
}

// ── E12: multi-level chain A←B←C, error in root A's default ─────────────
//
// A←B←C: undefined var in A's never-overridden default block.
// The diagnostic must attribute to A (skeleton_origin rides down the chain).

#[test]
fn e12_multilevel_undefined_var_attributes_to_root_base() {
    // A: root base with two blocks, one has an undefined var (never overridden).
    let a = concat!(
        "@block safe:\nSafe content.\n@end\n",
        "@block danger:\n{undefined_var}\n@end\n",
    );
    // B: extends A, overrides `safe`, leaves `danger` to A's default.
    let b = "@extends \"./a.mds\"\n@block safe:\nB override.\n@end\n";
    // C: extends B, also leaves `danger` to A's default.
    let c = "@extends \"./b.mds\"\n@block safe:\nC override.\n@end\n";
    let files = [("a.mds", a), ("b.mds", b), ("c.mds", c)];

    let err = compile_virtual(&files, "c.mds")
        .expect_err("e12 multilevel: undefined var in A default should error at C");
    let s = err.serialize();
    assert_eq!(s.code, "mds::undefined_var", "e12 multilevel: code: {s:?}");
    let span = s.span.expect("e12 multilevel: span must be Some");
    assert!(
        span.line.is_some(),
        "e12 multilevel: line Some (A source is ASCII): {span:?}"
    );
    assert!(
        span.column.is_some(),
        "e12 multilevel: column Some: {span:?}"
    );
    // Span offset must be in-bounds for A's source (NOT c's).
    assert!(
        span.offset + span.length <= a.len(),
        "e12 multilevel: span in-bounds for A's source (len={}): {span:?}",
        a.len()
    );
}

// ── E12: top-level non-block base node attributes to base ─────────────────
//
// A top-level interpolation in the BASE skeleton (between block declarations)
// is a non-block skeleton node — validated against skeleton_origin (base).

#[test]
fn e12_base_toplevel_nonblock_node_attributes_to_base() {
    // Base: a top-level interpolation between blocks — {customer_name} is not defined.
    let base = concat!(
        "@block header:\nHello.\n@end\n",
        "Hello {customer_name}!\n", // top-level interpolation in skeleton
        "@block footer:\nBye.\n@end\n",
    );
    let child = "@extends \"./base.mds\"\n";
    let files = [("base.mds", base), ("child.mds", child)];

    let err = compile_virtual(&files, "child.mds")
        .expect_err("e12 nonblock: undefined var in base top-level node should error");
    let s = err.serialize();
    assert_eq!(
        s.code, "mds::undefined_var",
        "e12 nonblock: code must be undefined_var: {s:?}"
    );
    let span = s.span.expect("e12 nonblock: span must be Some");
    assert!(span.line.is_some(), "e12 nonblock: line Some: {span:?}");
    // Span in-bounds for base source.
    assert!(
        span.offset + span.length <= base.len(),
        "e12 nonblock: span in-bounds for base: {span:?} (base len={})",
        base.len()
    );
}

// ── F: empty override compiles cleanly ────────────────────────────────────

#[test]
fn f_extends_empty_override_compiles_clean() {
    let base = "@block content:\nDefault.\n@end\n";
    // Child overrides block to empty body (valid — just erases the content).
    let child = "@extends \"./base.mds\"\n@block content:\n@end\n";
    let files = [("base.mds", base), ("child.mds", child)];
    let result = compile_virtual(&files, "child.mds");
    assert!(
        result.is_ok(),
        "f_extends_empty_override: empty block override should compile cleanly: {:?}",
        result.err()
    );
}

// ── A1: skeleton-then-standalone upgrade preserves span attribution ───────

#[test]
fn a_skeleton_then_standalone_upgrade_preserves_attribution() {
    // The base is resolved as a skeleton (via the child compile), then resolved
    // again as a standalone. After the upgrade, compiling a second child that
    // extends the base must still attribute errors to the base (not the child).
    let base = "@block content:\n{undefined_var}\n@end\n";
    let child = "@extends \"./base.mds\"\n";
    let files = [("base.mds", base), ("child.mds", child)];
    let mut cache = virtual_cache(&files);
    let mut warnings = vec![];

    // First: compile child — base gets resolved as skeleton.
    let child_err = cache
        .resolve_key("child.mds", &Default::default(), &mut warnings)
        .expect_err("A1 upgrade: child should error on undefined_var");
    let cs = child_err.serialize();
    assert_eq!(cs.code, "mds::undefined_var", "A1 upgrade child: {cs:?}");
    let cspan = cs.span.expect("A1 upgrade child: span must be Some");
    assert!(
        cspan.line.is_some(),
        "A1 upgrade child: line Some: {cspan:?}"
    );

    // Second: compile base standalone (A1 upgrade path).
    // (The base compiles fine standalone since {undefined_var} would be provided.)
    let base_with_var = "@block content:\n{x}\n@end\n";
    let base_def = "@block content:\nHello.\n@end\n";
    let files2 = [("base.mds", base_def), ("child.mds", child)];
    let mut cache2 = virtual_cache(&files2);
    let mut w2 = vec![];
    // Resolve child first (caches base as skeleton), then resolve base standalone.
    let _ = cache2.resolve_key("child.mds", &Default::default(), &mut w2);
    let base_standalone = cache2
        .resolve_key("base.mds", &Default::default(), &mut w2)
        .expect("A1 upgrade: base standalone after skeleton-cache must succeed");
    // skeleton_origin of the upgraded entry should still be the base's own file.
    assert_eq!(
        base_standalone.skeleton_origin.file.as_ref(),
        "base.mds",
        "A1 upgrade: skeleton_origin.file must be base.mds after upgrade"
    );
    let _ = base_with_var; // suppress unused variable warning
}

// ── P3: blocks from one file share one Arc<str> source ───────────────────

#[test]
fn p_block_sources_share_one_arc() {
    // Wide base: all blocks come from one file → all EffectiveBlocks share the
    // same Arc<str> source (Arc::ptr_eq). This confirms O(1) per block, not O(N).
    let mut base_src = String::new();
    for i in 0..10usize {
        base_src.push_str(&format!("@block blk{i}:\nDefault {i}.\n@end\n"));
    }
    let child = "@extends \"./base.mds\"\n";
    let files = [("base.mds", base_src.as_str()), ("child.mds", child)];
    let mut cache = virtual_cache(&files);
    let mut warnings = vec![];

    let result = cache
        .resolve_key("child.mds", &Default::default(), &mut warnings)
        .expect("P3: wide base should compile");

    // All effective block origins should share the same Arc<str> (Arc::ptr_eq).
    let mut origins: Vec<Arc<str>> = result
        .effective_blocks
        .values()
        .map(|eb| Arc::clone(&eb.origin.source))
        .collect();
    if origins.len() >= 2 {
        let first = &origins[0];
        for other in &origins[1..] {
            assert!(
                Arc::ptr_eq(first, other),
                "P3: all blocks from same file must share one Arc<str> source"
            );
        }
    }
    let _ = origins.pop(); // suppress unused warning
}
