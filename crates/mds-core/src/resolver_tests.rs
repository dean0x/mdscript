//! Resolver unit and integration tests, extracted from resolver.rs.

use super::frontmatter::deep_merge_yaml;
use super::*;
use crate::evaluator::evaluate;
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

// ── Module count: the entry plus 256 distinct modules ────────────────────

fn module_count_graph(total: usize, nested: bool) -> HashMap<String, String> {
    let imports = |start, end| {
        (start..end)
            .map(|i| format!("@import \"./m{i}.mds\" as m{i}\n"))
            .collect::<String>()
    };
    let mut files: HashMap<_, _> = (0..total)
        .map(|i| (format!("m{i}.mds"), "Body\n".to_string()))
        .collect();
    let entry = if nested {
        files.insert("m1.mds".into(), imports(2, total));
        imports(1, 2)
    } else {
        imports(1, total)
    };
    files.insert("m0.mds".into(), format!("{entry}OK\n"));
    files
}

fn assert_module_count_outcome<T: std::fmt::Debug>(result: Result<T, MdsError>, total: usize) {
    if total <= 257 {
        result.expect("the entry plus at most 256 modules must compile");
    } else {
        assert!(result.is_err(), "a graph of {total} modules must fail");
        let err = result.unwrap_err();
        assert_eq!(err.serialize().code, "mds::resource_limit");
        assert_eq!(
            err.to_string(),
            "resource limit exceeded: module count exceeds maximum of 256 modules besides the entry"
        );
    }
}

#[test]
fn module_count_legacy_graph_boundaries() {
    for nested in [true, false] {
        for total in 255..=259 {
            let mut cache = ModuleCache::virtual_fs(module_count_graph(total, nested));
            assert_module_count_outcome(
                cache.resolve_key("m0.mds", &HashMap::new(), &mut Vec::new()),
                total,
            );
        }
    }
}

#[test]
fn module_count_intrinsic_graph_boundaries() {
    for nested in [true, false] {
        for total in 255..=259 {
            for with_opts in [false, true] {
                let mut cache = ModuleCache::virtual_fs(module_count_graph(total, nested));
                let result = if with_opts {
                    cache
                        .resolve_virtual_intrinsic_opts(
                            "m0.mds",
                            &HashMap::new(),
                            &Default::default(),
                            &mut Vec::new(),
                        )
                        .map(|(output, _)| output)
                } else {
                    cache.resolve_virtual_intrinsic("m0.mds", &HashMap::new(), &mut Vec::new())
                };
                assert_module_count_outcome(result, total);
            }
        }
    }
}

#[test]
fn module_count_native_graph_boundaries() {
    for nested in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        for total in 255..=259 {
            for (key, source) in module_count_graph(total, nested) {
                std::fs::write(dir.path().join(key), source).unwrap();
            }
            assert_module_count_outcome(crate::compile(dir.path().join("m0.mds"), None), total);
        }
    }
}

#[test]
fn module_count_string_entry_counts_root() {
    for total in 257..=258 {
        for mode in 0..3 {
            let mut files = module_count_graph(total, true);
            let source = files.remove("m0.mds").unwrap();
            let mut cache = ModuleCache::virtual_fs(files);
            let result = match mode {
                0 => cache
                    .resolve_source(&source, "", &HashMap::new(), &mut Vec::new())
                    .map(|_| ()),
                1 => cache
                    .resolve_source_intrinsic(&source, "", &HashMap::new(), &mut Vec::new())
                    .map(|_| ()),
                _ => cache
                    .resolve_source_intrinsic_opts(
                        &source,
                        "",
                        &HashMap::new(),
                        &Default::default(),
                        &mut Vec::new(),
                    )
                    .map(|_| ()),
            };
            assert_module_count_outcome(result, total);
        }
    }
}

#[test]
fn module_count_skeleton_graph_boundaries() {
    for total in 257..=258 {
        let mut files = module_count_graph(total, true);
        files.insert("m0.mds".into(), "@extends \"./m1.mds\"\n".into());
        assert_module_count_outcome(crate::compile_virtual(files, "m0.mds", None), total);
    }
}

#[test]
fn module_count_reused_cache_checks_each_entry_path() {
    for mode in 0..5 {
        let mut files = module_count_graph(257, false);
        files.insert("fresh.mds".into(), "Fresh\n".into());
        let mut cache = ModuleCache::virtual_fs(files);
        cache
            .resolve_key("m0.mds", &HashMap::new(), &mut Vec::new())
            .expect("the cache can hold the entry plus 256 modules");
        let result = match mode {
            0 => cache
                .resolve_virtual_intrinsic("fresh.mds", &HashMap::new(), &mut Vec::new())
                .map(|_| ()),
            1 => cache
                .resolve_virtual_intrinsic_opts(
                    "fresh.mds",
                    &HashMap::new(),
                    &Default::default(),
                    &mut Vec::new(),
                )
                .map(|_| ()),
            2 => cache
                .resolve_source("Fresh\n", "", &HashMap::new(), &mut Vec::new())
                .map(|_| ()),
            3 => cache
                .resolve_source_intrinsic("Fresh\n", "", &HashMap::new(), &mut Vec::new())
                .map(|_| ()),
            _ => cache
                .resolve_source_intrinsic_opts(
                    "Fresh\n",
                    "",
                    &HashMap::new(),
                    &Default::default(),
                    &mut Vec::new(),
                )
                .map(|_| ()),
        };
        assert_module_count_outcome(result, 258);
    }
}

#[test]
fn module_count_cached_skeleton_upgrade_counts_once() {
    let mut files = module_count_graph(257, false);
    files.insert("m1.mds".into(), "@extends \"./m2.mds\"\n".into());
    let entry = files.get_mut("m0.mds").unwrap();
    *entry = entry.replace("@import \"./m2.mds\" as m2\n", "");
    entry.push_str("@import \"./m2.mds\" as m2\n@import \"./m3.mds\" as again\n");
    let result = crate::compile_virtual(files, "m0.mds", None)
        .expect("cached modules and a skeleton upgrade must not consume another slot");
    assert_eq!(result.dependencies.len(), 256);
    assert_eq!(result.into_markdown().unwrap(), "OK\n");
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

fn define_source(count: usize, prefix: &str) -> String {
    use std::fmt::Write as _;

    let mut source = String::new();
    for i in 1..=count {
        writeln!(source, "@define {prefix}{i}():\nbody\n@end").unwrap();
    }
    source
}

fn assert_define_limit(source: &str) {
    let error = crate::compile_str_md(source).unwrap_err().serialize();
    assert_eq!(error.code, "mds::resource_limit");
    assert_eq!(
        error.message,
        "resource limit exceeded: module has more than 256 @define declarations"
    );
}

#[test]
fn define_cap_accepts_256_declarations() {
    let mut source = define_source(256, "f");
    source.push_str("{{f256()}}");
    assert_eq!(crate::compile_str_md(&source).unwrap(), "body\n");
}

#[test]
fn define_cap_refuses_257_declarations() {
    assert_define_limit(&define_source(257, "f"));
}

#[test]
fn define_cap_counts_declarations_in_nested_bodies() {
    for (open, outer_defines) in [
        ("@if enabled:", 0),
        ("@if disabled:\n@elseif enabled:", 0),
        ("@if enabled:\n@else:", 0),
        ("@for item in items:", 0),
        ("@block section:", 0),
        ("@message user:", 0),
        ("@define outer():", 1),
    ] {
        let prefix = define_source(255 - outer_defines, "f");
        let source = format!(
            "---\nenabled: true\ndisabled: false\nitems: []\n---\n{prefix}{open}\n@define inner():\nbody\n@end\ntext\n@end\n"
        );
        crate::compile_str(&source).unwrap();
        assert_define_limit(&format!("{source}@define last():\nbody\n@end"));
    }
}

#[test]
fn define_cap_error_precedes_a_name_collision() {
    let mut source = define_source(256, "f");
    source.push_str("@define f1():\nother\n@end");
    assert_define_limit(&source);
}

#[test]
fn define_cap_does_not_count_reexports() {
    let library = define_source(10, "lib");
    let mut source = String::from("@export * from \"./lib.mds\"\n");
    source.push_str(&define_source(256, "f"));
    source.push_str("@export prompt\n{{f256()}}");
    let module = virtual_cache(&[("lib.mds", &library)])
        .resolve_source(&source, "", &HashMap::new(), &mut Vec::new())
        .unwrap();
    assert_eq!(module.prompt_body.as_deref(), Some("body"));
    assert!(module.explicit_exports.contains("lib10"));
}

#[test]
fn define_capture_holds_one_function_node_per_define() {
    use std::fmt::Write as _;

    let defines = [1, 2, 3, 12, 13];
    let counts = defines.map(|k| {
        let mut source = String::new();
        for i in 1..=k {
            writeln!(source, "@define f{i}(x):\nv{{{{x}}}}\n@end").unwrap();
        }
        let name = format!("f{k}");
        write!(source, "{{{{{name}(\"a\")}}}}").unwrap();

        let module = virtual_cache(&[("main.mds", &source)])
            .resolve_key("main.mds", &HashMap::new(), &mut Vec::new())
            .unwrap();
        assert_eq!(module.prompt_body.as_deref(), Some("va"), "K={k}");
        crate::scope::captured_function_nodes(&module.functions[&name])
    });
    let delta = counts[4].checked_sub(counts[3]);
    assert_eq!(
        (&counts[..4], delta),
        (&[1, 2, 3, 12][..], Some(1)),
        "FunctionDef counts for K={defines:?}: {counts:?}; change from K=12 to K=13: {delta:?}"
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

#[test]
fn function_call_limit_counts_each_kind_across_modules() {
    use crate::limits::MAX_TOTAL_FUNCTION_CALLS;

    let lib = "@define word():\nb\n@end\n{{upper(\"a\")}}";
    let entry = concat!(
        "@import \"./lib.mds\" as lib\n",
        "@define local():\nc\n@end\n"
    );
    for (body, source_map) in [
        ("@include lib\n{{lib.word()}}{{local()}}", false),
        ("@include lib\n{{lib.word()}}{{local()}}", true),
        (
            "@message user:\n@include lib\n{{lib.word()}}{{local()}}\n@end",
            false,
        ),
    ] {
        let source = format!("{entry}{body}");
        let compile = |calls_left| {
            let mut cache = virtual_cache(&[("entry.mds", &source), ("lib.mds", lib)]);
            cache.function_call_seed = MAX_TOTAL_FUNCTION_CALLS - calls_left;
            let opts = crate::CompileOptions {
                source_map,
                ..Default::default()
            };
            cache.resolve_virtual_intrinsic_opts(
                "entry.mds",
                &HashMap::new(),
                &opts,
                &mut Vec::new(),
            )
        };
        let (output, _) = compile(3).unwrap();
        let text = match output {
            crate::CompiledOutput::Markdown(text) => text,
            crate::CompiledOutput::Messages(messages) => messages[0].content.clone(),
        };
        assert_eq!(text.lines().collect::<String>(), "Abc");
        assert_eq!(
            compile(2).unwrap_err().serialize().code,
            "mds::resource_limit"
        );
    }
}

#[test]
fn function_call_limit_counts_across_extends_regions() {
    use crate::limits::MAX_TOTAL_FUNCTION_CALLS;

    let compile = |calls_left| {
        let mut cache = virtual_cache(&[
            ("base.mds", "{{upper(\"a\")}}\n@block content:\nx\n@end"),
            (
                "entry.mds",
                "@extends \"./base.mds\"\n@block content:\n{{upper(\"b\")}}\n@end",
            ),
        ]);
        cache.function_call_seed = MAX_TOTAL_FUNCTION_CALLS - calls_left;
        cache.resolve_virtual_intrinsic("entry.mds", &HashMap::new(), &mut Vec::new())
    };
    let crate::CompiledOutput::Markdown(text) = compile(2).unwrap() else {
        panic!("expected Markdown output");
    };
    assert_eq!(text.lines().collect::<String>(), "AB");
    assert_eq!(
        compile(1).unwrap_err().serialize().code,
        "mds::resource_limit"
    );
}

#[test]
fn scope_layer_precedence_keeps_runtime_above_frontmatter() {
    let mut cache = virtual_cache(&[
        (
            "base.mds",
            "---\nx: base\ny: base\n---\n@block content:\n{{x}}/{{y}}\n@end",
        ),
        ("standalone.mds", "---\nx: local\n---\n{{x}}"),
        ("child.mds", "---\ny: child\n---\n@extends \"./base.mds\"\n"),
    ]);
    let vars = HashMap::from([("x".into(), Value::String("runtime".into()))]);
    for (key, expected) in [
        ("standalone.mds", "runtime"),
        ("child.mds", "runtime/child"),
    ] {
        let module = cache.resolve_key(key, &vars, &mut Vec::new()).unwrap();
        assert_eq!(module.prompt_body.as_deref().map(str::trim), Some(expected));
    }
}

#[test]
fn scope_layer_precedence_keeps_imported_prompt_above_both_layers() {
    let vars = HashMap::from([("prompt".into(), Value::String("runtime".into()))]);
    for source in [
        "---\nprompt: local\n---\n@import \"./lib.mds\"\n{{prompt}}",
        "---\nprompt: local\n---\n@import { prompt } from \"./lib.mds\"\n{{prompt}}",
        "---\nprompt: local\nimports:\n  - path: ./lib.mds\n---\n{{prompt}}",
    ] {
        let module = virtual_cache(&[("entry.mds", source), ("lib.mds", "imported")])
            .resolve_key("entry.mds", &vars, &mut Vec::new())
            .unwrap();
        assert_eq!(
            module.prompt_body.as_deref().map(str::trim),
            Some("imported")
        );
    }
}

#[test]
fn scope_layer_precedence_keeps_imports_reserved_only_for_mds() {
    let vars = HashMap::from([("imports".into(), Value::String("runtime".into()))]);
    let mut cache = virtual_cache(&[
        ("entry.mds", "{{imports}}"),
        ("invalid.mds", "---\nimports: invalid\n---\n{{imports}}"),
    ]);
    let err = cache
        .resolve_key("entry.mds", &vars, &mut Vec::new())
        .unwrap_err();
    assert_eq!(
        miette::Diagnostic::code(&err).unwrap().to_string(),
        "mds::import"
    );
    assert!(err.to_string().ends_with(
        "'imports' is a reserved frontmatter key for MDS files and cannot be set via --set"
    ));
    let source = "{{imports}}";
    let tokens = tokenize(source, "plain.md").unwrap();
    let module = parse_with_ctx(&tokens, "plain.md", source).unwrap();
    let (mut plain, _) = build_scope_from_frontmatter(
        None,
        None,
        0,
        true,
        &runtime_layer(&vars),
        &mut MemBudget::default(),
    )
    .unwrap();
    assert_eq!(
        evaluate(
            &module.body,
            &mut plain,
            &mut Vec::new(),
            "plain.md",
            source
        )
        .unwrap(),
        "runtime"
    );
    let invalid = cache
        .resolve_key("invalid.mds", &vars, &mut Vec::new())
        .unwrap_err();
    let no_runtime = cache
        .resolve_key("invalid.mds", &HashMap::new(), &mut Vec::new())
        .unwrap_err();
    assert_eq!(invalid.to_string(), no_runtime.to_string());
    assert_ne!(invalid.to_string(), err.to_string());
}

#[test]
fn indented_directives_never_enters_scope_from_frontmatter() {
    let frontmatter = crate::ast::Frontmatter {
        raw: "indented-directives: true\nname: World\n".to_owned(),
    };
    let mut mem = MemBudget::default();
    let (mapping, spent) = parse_frontmatter_mapping(Some(&frontmatter), &mut mem).unwrap();
    for is_md in [false, true] {
        let (scope, _) = build_scope_from_frontmatter(
            Some(&frontmatter),
            mapping.as_ref(),
            spent,
            is_md,
            &runtime_layer(&HashMap::new()),
            &mut mem,
        )
        .unwrap();
        assert!(scope.get_slot("name").is_some());
        assert!(scope.get_slot("indented-directives").is_none());
    }
}

#[test]
fn runtime_layer_is_shared_by_module_scopes() {
    let vars = HashMap::from([("x".into(), Value::String("runtime".into()))]);
    let vars = runtime_layer(&vars);
    let (first, _) =
        build_scope_from_frontmatter(None, None, 0, false, &vars, &mut MemBudget::default())
            .unwrap();
    let inherited = build_scope_from_merged_mapping(&serde_yaml_ng::Mapping::new(), &vars).unwrap();
    assert!(Arc::ptr_eq(
        first.get_slot("x").unwrap().root(),
        inherited.get_slot("x").unwrap().root()
    ));
}

#[test]
fn runtime_layer_cache_reuse_preserves_import_values() {
    let mut cache = virtual_cache(&[("lib.mds", "@define from_lib():\n{{x}}\n@end")]);
    let source = "@import \"./lib.mds\"\n{{from_lib()}}/{{x}}";
    for (runtime, expected) in [("first", "first/first"), ("second", "first/second")] {
        let vars = HashMap::from([("x".into(), Value::String(runtime.into()))]);
        let module = cache
            .resolve_source(source, "", &vars, &mut Vec::new())
            .unwrap();
        assert_eq!(module.prompt_body.as_deref().map(str::trim), Some(expected));
    }
}

#[test]
fn captures_keep_each_imported_prompt_write() {
    let source = concat!(
        "@import \"./first.mds\"\n@define before():\n{{prompt}}\n@end\n",
        "@import \"./second.mds\"\n@define after():\n{{prompt}}\n@end\n",
        "{{before()}}/{{after()}}",
    );
    let module = virtual_cache(&[("first.mds", "one"), ("second.mds", "two")])
        .resolve_source(source, "", &HashMap::new(), &mut Vec::new())
        .unwrap();
    assert_eq!(module.prompt_body.as_deref(), Some("one/two"));
}

#[test]
fn prompt_imports_and_captures_share_the_module_text() {
    let mut cache = virtual_cache(&[("lib.mds", "library")]);
    let mut shared = Vec::new();
    for names in ["", "    names: [prompt]\n"] {
        let source = format!(
            "---\nimports:\n  - path: ./lib.mds\n{names}---\n{}",
            concat!(
                "@define fm():\n{{prompt}}\n@end\n",
                "@import \"./lib.mds\"\n@define first():\n{{prompt}}\n@end\n",
                "@import \"./lib.mds\"\n@define second():\n{{prompt}}\n@end\n",
                "@import { prompt } from \"./lib.mds\"\n@define pick():\n{{prompt}}\n@end\n",
                "{{fm()}}/{{first()}}/{{second()}}/{{pick()}}"
            )
        );
        let module = cache
            .resolve_source(&source, "", &HashMap::new(), &mut Vec::new())
            .unwrap();
        assert_eq!(
            module.prompt_body.as_deref(),
            Some("library/library/library/library")
        );
        let library = &cache.modules["lib.mds"];
        let body = library.prompt_body.as_deref().unwrap();
        let value = library.shared_prompt_value().unwrap();
        let mut scope = Scope::new();
        for name in ["fm", "first", "second", "pick"] {
            scope.push_call(&module.functions[name]);
            let slot = scope.get_slot("prompt").unwrap();
            let Value::String(text) = slot.get().unwrap() else {
                panic!("{name} must capture prompt text");
            };
            assert!(
                Arc::ptr_eq(slot.root(), &value),
                "{name} must share the prompt value"
            );
            shared.push((name, text.as_ptr() == body.as_ptr()));
            scope.pop().unwrap();
        }
    }
    assert_eq!(
        shared,
        [
            ("fm", true),
            ("first", true),
            ("second", true),
            ("pick", true)
        ]
        .repeat(2)
    );
}

#[test]
fn alias_imports_share_the_module_namespace_and_text() {
    let source = concat!(
        "---\nimports:\n  - path: ./lib.mds\n    as: front\n---\n",
        "@import \"./lib.mds\" as body\n@define f():\n{{front.g()}}/{{body.g()}}\n@end\n{{f()}}"
    );
    let mut cache = virtual_cache(&[("lib.mds", "@define g():\nresult\n@end\nlibrary")]);
    let module = cache
        .resolve_source(source, "", &HashMap::new(), &mut Vec::new())
        .unwrap();
    assert_eq!(module.prompt_body.as_deref(), Some("result/result"));
    let capture = &module.functions["f"].captured;
    let front = capture.weak_namespace("front").unwrap();
    let body = capture.weak_namespace("body").unwrap();
    let library = &cache.modules["lib.mds"];
    let text = library.prompt_body.as_deref().unwrap();
    let namespace = front.upgrade().unwrap();
    let namespace_text = namespace.prompt_body.as_deref().unwrap();
    assert_eq!(
        (
            front.ptr_eq(&body),
            Arc::ptr_eq(&namespace, &library.shared_namespace()),
            text.as_ptr() == namespace_text.as_ptr(),
        ),
        (true, true, true)
    );
}

#[test]
fn captured_import_graph_drops_after_the_last_owner() {
    let (keep, imported, link, namespace, prompt) = {
        let mut cache = virtual_cache(&[
            ("merge.mds", "@define merged():\nmerge\n@end"),
            ("alias.mds", "@define aliased():\nalias\n@end"),
            ("prompt.mds", "prompt"),
        ]);
        let source = concat!(
            "@import \"./merge.mds\"\n@import \"./alias.mds\" as lib\n",
            "@import { prompt } from \"./prompt.mds\"\n",
            "@define local():\n{{merged()}}/{{lib.aliased()}}/{{prompt}}\n@end\n{{local()}}",
        );
        let module = cache
            .resolve_source(source, "", &HashMap::new(), &mut Vec::new())
            .unwrap();
        assert_eq!(module.prompt_body.as_deref(), Some("merge/alias/prompt"));
        let keep = Arc::clone(&module.functions["local"]);
        let imported = Arc::downgrade(&cache.modules["merge.mds"].functions["merged"]);
        let link = keep.captured.weak_head().unwrap();
        let namespace = keep.captured.weak_namespace("lib").unwrap();
        let prompt = Arc::downgrade(&cache.modules["prompt.mds"].shared_prompt_value().unwrap());
        (keep, imported, link, namespace, prompt)
    };
    let local = Arc::downgrade(&keep);
    assert!(local.upgrade().is_some());
    assert!(imported.upgrade().is_some());
    assert!(link.upgrade().is_some());
    assert!(namespace.upgrade().is_some());
    assert!(prompt.upgrade().is_some());

    drop(keep);
    assert!(local.upgrade().is_none());
    assert!(imported.upgrade().is_none());
    assert!(link.upgrade().is_none());
    assert!(namespace.upgrade().is_none());
    assert!(prompt.upgrade().is_none());
}

mod prompt_source_scan {
    #[test]
    fn internal_prompt_producers_use_shared_accessors() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut dirs = vec![src];
        let mut files = 0;
        let mut hits = Vec::new();
        for _ in 0..1024 {
            let Some(dir) = dirs.pop() else { break };
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let kind = entry.file_type().unwrap();
                if kind.is_dir() {
                    dirs.push(path);
                } else if kind.is_file()
                    && path.extension().is_some_and(|ext| ext == "rs")
                    && !entry.file_name().to_string_lossy().ends_with("_tests.rs")
                {
                    files += 1;
                    let count = forbidden_producers(&std::fs::read_to_string(&path).unwrap());
                    if count != 0 {
                        hits.push((path, count));
                    }
                }
            }
        }
        assert!(dirs.is_empty(), "must visit every source directory");
        assert!(
            files >= 10,
            "expected at least ten source files, saw {files}"
        );
        assert!(
            hits.is_empty(),
            "use the shared accessors instead: {hits:?}"
        );
    }

    #[test]
    fn prompt_source_scan_detects_each_planted_producer() {
        let ignored = r##"
            pub fn get_prompt_value(&self) -> Option<Value> { None }
            // m.get_prompt_value();
            /* outer /* nested */ m.to_namespace(); */
            const TEXT: &str = r#"m.to_namespace();"#;
            #[cfg(test)] fn sample() { m.get_prompt_value(); }
            #[cfg(test)] mod extra;
        "##;
        assert_eq!(forbidden_producers(ignored), 0);
        for planted in ["m.get_prompt_value();", "m.to_namespace();"] {
            assert_eq!(forbidden_producers(&format!("{ignored}{planted}")), 1);
        }

        let field = r"
            struct Sample { #[cfg(test)] test_field: bool }
            fn production(m: ResolvedModule) { m.to_namespace(); }
        ";
        let initializer = r"
            fn production(m: ResolvedModule) {
                Sample {
                    #[cfg(test)] test_field: false,
                    namespace: { m.to_namespace() },
                };
            }
        ";
        assert_eq!(
            [field, initializer].map(forbidden_producers),
            [1, 1],
            "scan must count each producer after a test-only field"
        );
    }

    fn forbidden_producers(source: &str) -> usize {
        let tokens = source_tokens(source);
        let mut rest = tokens.as_slice();
        let mut prev = "";
        let mut hits = 0;
        for _ in 0..tokens.len() {
            if let Some(tail) = rest.strip_prefix(&["#", "[", "cfg", "(", "test", ")", "]"]) {
                // Scan the tail if the attribute has no clear item boundary.
                rest = after_test_item(tail).unwrap_or(tail);
                prev = "";
            } else if let Some((&token, tail)) = rest.split_first() {
                hits += usize::from(
                    token == "to_namespace" || (token == "get_prompt_value" && prev != "fn"),
                );
                rest = tail;
                prev = token;
            } else {
                break;
            }
        }
        hits
    }

    fn after_test_item<'a>(item: &'a [&'a str]) -> Option<&'a [&'a str]> {
        let mut nested = 0_usize;
        for (i, &token) in item.iter().enumerate() {
            match token {
                "[" | "(" | "{" => nested += 1,
                "]" | ")" | "}" => {
                    nested = nested.checked_sub(1)?;
                    if nested == 0 && token == "}" {
                        return Some(&item[i + 1..]);
                    }
                }
                ";" if nested == 0 => return Some(&item[i + 1..]),
                // A comma can end a field or arm, but does not prove an item end.
                "," if nested == 0 => return None,
                _ => {}
            }
        }
        panic!("expected an end to the #[cfg(test)] item");
    }

    // Ignore comments and literals before the scan considers names or test attributes.
    fn source_tokens(mut rest: &str) -> Vec<&str> {
        let mut tokens = Vec::new();
        for _ in 0..rest.len() {
            rest = rest.trim_start();
            let Some(first) = rest.chars().next() else {
                break;
            };
            if let Some(line) = rest.strip_prefix("//") {
                rest = line.split_once('\n').map_or("", |(_, tail)| tail);
            } else if let Some(block) = rest.strip_prefix("/*") {
                rest = after_block_comment(block);
            } else if let Some(tail) = after_literal(rest) {
                rest = tail;
            } else {
                let len = if first.is_alphanumeric() || first == '_' {
                    rest.find(|c: char| !c.is_alphanumeric() && c != '_')
                        .unwrap_or(rest.len())
                } else {
                    first.len_utf8()
                };
                let (token, tail) = rest.split_at(len);
                tokens.push(token);
                rest = tail;
            }
        }
        tokens
    }

    fn after_block_comment(mut rest: &str) -> &str {
        let mut depth = 1;
        for _ in 0..rest.len() {
            let at = rest.find(['/', '*']).expect("must close block comment");
            rest = &rest[at..];
            if rest.starts_with("/*") {
                depth += 1;
                rest = &rest[2..];
            } else if rest.starts_with("*/") {
                depth -= 1;
                rest = &rest[2..];
                if depth == 0 {
                    return rest;
                }
            } else {
                rest = &rest[1..];
            }
        }
        panic!("must close block comment");
    }

    fn after_literal(source: &str) -> Option<&str> {
        let source = source
            .strip_prefix('b')
            .or_else(|| source.strip_prefix('c'))
            .unwrap_or(source);
        if let Some(raw) = source.strip_prefix('r') {
            let quote = raw.find(|c| c != '#')?;
            let body = raw[quote..].strip_prefix('"')?;
            let end = format!("\"{}", &raw[..quote]);
            return body.split_once(&end).map(|(_, tail)| tail);
        }
        if let Some(body) = source.strip_prefix('"') {
            let mut escaped = false;
            for (i, c) in body.char_indices() {
                if !escaped && c == '"' {
                    return Some(&body[i + 1..]);
                }
                escaped = !escaped && c == '\\';
            }
            return None;
        }
        let body = source.strip_prefix('\'')?;
        let len = if let Some(escape) = body.strip_prefix('\\') {
            match escape.as_bytes().first()? {
                b'x' => 4,
                b'u' => escape.find('}')? + 2,
                _ => 2,
            }
        } else {
            body.chars().next()?.len_utf8()
        };
        body.get(len..)?.strip_prefix('\'')
    }
}

#[test]
fn imported_defines_share_the_runtime_layer_of_their_call() {
    let mut cache = virtual_cache(&[
        ("first.mds", "@define first():\n{{x}}\n@end"),
        ("second.mds", "@define second():\n{{x}}\n@end"),
    ]);
    let source = concat!(
        "@import \"./first.mds\"\n@import \"./second.mds\"\n",
        "@define local():\n{{x}}\n@end\n{{first()}}/{{second()}}/{{local()}}",
    );
    let first_vars = HashMap::from([("x".into(), Value::String("one".into()))]);
    let second_vars = HashMap::from([("x".into(), Value::String("two".into()))]);
    let first = cache
        .resolve_source(source, "", &first_vars, &mut Vec::new())
        .unwrap();
    let second = cache
        .resolve_source(source, "", &second_vars, &mut Vec::new())
        .unwrap();
    assert_eq!(first.prompt_body.as_deref(), Some("one/one/one"));
    assert_eq!(second.prompt_body.as_deref(), Some("one/one/two"));
    fn runtime<'a>(module: &'a ResolvedModule, function: &str) -> &'a Arc<VarLayer> {
        module.functions[function]
            .captured
            .runtime
            .as_ref()
            .unwrap()
    }
    let imported = runtime(&cache.modules["first.mds"], "first");
    assert!(Arc::ptr_eq(
        imported,
        runtime(&cache.modules["second.mds"], "second")
    ));
    assert!(Arc::ptr_eq(imported, runtime(&first, "local")));
    assert!(!Arc::ptr_eq(
        runtime(&first, "local"),
        runtime(&second, "local")
    ));
}

#[test]
fn selective_import_shadows_a_define_in_the_next_capture() {
    let source = concat!(
        "@define same():\nlocal\n@end\n@define earlier():\n{{same()}}\n@end\n",
        "@import { same } from \"./lib.mds\"\n",
        "@define captured():\n{{same()}}\n@end\n{{earlier()}}/{{same()}}/{{captured()}}",
    );
    let module = virtual_cache(&[("lib.mds", "@define same():\nimported\n@end")])
        .resolve_source(source, "", &HashMap::new(), &mut Vec::new())
        .unwrap();
    assert_eq!(
        module.prompt_body.as_deref(),
        Some("local/imported/imported")
    );
}

#[test]
fn define_shadows_an_earlier_merge_import_in_the_next_capture() {
    let source = concat!(
        "@import \"./lib.mds\"\n@define same():\nlocal\n@end\n",
        "@define captured():\n{{same()}}\n@end\n{{same()}}/{{captured()}}",
    );
    let module = virtual_cache(&[("lib.mds", "@define same():\nimported\n@end")])
        .resolve_source(source, "", &HashMap::new(), &mut Vec::new())
        .unwrap();
    assert_eq!(module.prompt_body.as_deref(), Some("local/local"));
}

#[test]
fn merge_import_refuses_a_name_that_a_define_already_uses() {
    let source = "@define same():\nlocal\n@end\n@import \"./lib.mds\"";
    let err = virtual_cache(&[("lib.mds", "@define same():\nimported\n@end")])
        .resolve_source(source, "", &HashMap::new(), &mut Vec::new())
        .unwrap_err();
    assert_eq!(
        miette::Diagnostic::code(&err).unwrap().to_string(),
        "mds::name_collision"
    );
}

#[test]
fn interleaved_imports_and_defines_keep_one_capture_entry_per_write() {
    use std::fmt::Write as _;

    let mut library = String::new();
    let mut source = String::new();
    for i in 1..=128 {
        writeln!(library, "@define i{i}():\nimported\n@end").unwrap();
        writeln!(
            source,
            "@import {{ i{i} }} from \"./lib.mds\"\n@define f{i}():\n{{{{i{i}()}}}}\n@end"
        )
        .unwrap();
    }
    source.push_str("{{f128()}}");
    let tokens = tokenize(&source, "entry.mds").unwrap();
    let module = parse_with_ctx(&tokens, "entry.mds", &source).unwrap();
    let ctx = ModuleCtx {
        key: "entry.mds",
        file_str: "entry.mds",
        source: &source,
        base_dir: "",
        runtime_vars: &Arc::default(),
        origin: OnceCell::new(),
    };
    let mut scope = Scope::new();
    let defs = virtual_cache(&[("lib.mds", &library)])
        .collect_definitions_and_imports(&module.body, &mut scope, &ctx, &mut Vec::new())
        .unwrap();
    assert_eq!(defs.functions.len(), 128);
    assert_eq!(
        evaluate(
            &module.body,
            &mut scope,
            &mut Vec::new(),
            ctx.file_str,
            &source
        )
        .unwrap(),
        "imported"
    );
    assert_eq!(crate::scope::captured_entry_count(&scope), 256);
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

#[test]
fn extends_lexes_the_base_and_child_with_their_own_file_keys() {
    let base_body = "  @if 1 == 0:\nbase text\n  @end\n";
    let child_body = "  @if 1 == 0:\nchild text\n  @end\n";
    for base_on in [true, false] {
        let base = format!("---\ntitle: Base\nindented-directives: {base_on}\n---\n{base_body}@block section:\ndefault\n@end\n");
        let child = format!("---\nindented-directives: {}\n---\n@extends \"./base.mds\"\n@block section:\n{child_body}@end\n", !base_on);
        let output =
            compile_virtual(&[("base.mds", &base), ("child.mds", &child)], "child.mds").unwrap();
        let literal = if base_on { child_body } else { base_body };
        assert_eq!(output, format!("---\ntitle: Base\n---\n{literal}"));
    }
}

const INDENTED_DIRECTIVES_FORM: &str =
    "expected indented-directives: true or indented-directives: false";

#[test]
fn indented_directives_yaml_agreement_covers_all_module_kinds() {
    let entry_routes = [
        "@import { word } from \"./dep.mds\"\n{{word()}}\n",
        "@import \"./dep.mds\" as dep\n@include dep\n",
        "@extends \"./dep.mds\"\n",
    ];
    let body = "@define word():\nword\n@end\nbody\n";
    let compile_error = |files: &[(&str, &str)]| {
        let err = compile_virtual(files, "entry.mds").unwrap_err();
        assert_eq!(err.serialize().code, "mds::yaml");
        assert!(
            matches!(err, MdsError::YamlError { message } if message == INDENTED_DIRECTIVES_FORM)
        );
    };
    for raw in [
        "\"indented-directives\": true",
        "{indented-directives: true}",
        "? indented-directives\n: true",
        "# comment\u{2028}indented-directives: true",
        "{indented-directives: 'true'}",
    ] {
        let source = format!("---\n{raw}\n---\n{body}");
        compile_error(&[("entry.mds", &source)]);
        for entry in entry_routes {
            compile_error(&[("entry.mds", entry), ("dep.mds", &source)]);
        }
    }
}

#[test]
fn indented_directives_yaml_agreement_rejects_a_matching_value_from_another_line() {
    let src = "---\n\"indented-directives\": true\ntext: \"start\nindented-directives: true\nend\"\n---\nbody\n";
    let err = compile_virtual(&[("entry.mds", src)], "entry.mds").unwrap_err();
    assert!(matches!(err, MdsError::YamlError { message } if message == INDENTED_DIRECTIVES_FORM));
}

#[test]
fn indented_directives_yaml_agreement_accepts_a_key_after_other_values() {
    let body = "  @if enabled:\nyes\n  @end\n";
    for (key, expected) in [("true", "yes\n"), ("false", body)] {
        let metadata = "title: 'one\ntwo'\nenabled: true\n";
        let source =
            format!("---\n{metadata}indented-directives: \t{key} \t # option\n---\n{body}");
        let output = compile_virtual(&[("entry.mds", &source)], "entry.mds").unwrap();
        assert_eq!(output, format!("---\n{metadata}---\n{expected}"));
    }
}

#[test]
fn indented_directives_preserves_metadata_after_a_yaml_line_break() {
    for eol in ['\u{85}', '\u{2028}', '\u{2029}'] {
        let source = format!(
            "---\nindented-directives: true # flag{eol}title: Sample\n---\nTitle: {{{{title}}}}\n"
        );
        assert_eq!(
            compile_virtual(&[("entry.mds", &source)], "entry.mds").unwrap(),
            "---\ntitle: Sample\n---\nTitle: Sample\n"
        );
    }
}

#[test]
fn indented_directives_yaml_agreement_rejects_a_probe_hit_in_a_scalar() {
    let src = "---\n\"text\nindented-directives: true\nend\"\n---\nbody\n";
    let err = compile_virtual(&[("entry.mds", src)], "entry.mds").unwrap_err();
    assert!(matches!(err, MdsError::YamlError { message } if message == INDENTED_DIRECTIVES_FORM));
}

#[test]
fn indented_directives_yaml_agreement_rejects_a_value_changed_by_folding() {
    let src = "---\nindented-directives: true\n  more\n---\nbody\n";
    let err = compile_virtual(&[("entry.mds", src)], "entry.mds").unwrap_err();
    assert!(matches!(err, MdsError::YamlError { message } if message == INDENTED_DIRECTIVES_FORM));
}

// ── F1: issue worked example (headline test) ──────────────────────────────

#[test]
fn f1_worked_example_byte_exact() {
    // base.mds: skeleton with @block placeholders
    // child.mds: overrides instructions+tools, inherits output_format default
    // role=data analysis from child frontmatter
    let base = concat!(
        "You are a {{role}} assistant.\n",
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
        "You are a {{role}} assistant.\n",
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
    let base = "You are a {{role}} assistant.\n@block instructions:\nDefault.\n@end\n";
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
    let base_with_fm = "---\nrole: default\n---\nYou are a {{role}}.\n@block b:\nBody.\n@end\n";
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
    let base = "---\nrole: default\n---\nYou are a {{role}}.\n@block b:\nBody.\n@end\n";
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
    let base = "@block content:\n{{undefined_var}}\n@end\n";
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
    let b_resolved = b_resolved.unwrap();
    let b_body = b_resolved.prompt_body.as_deref().unwrap_or_default();
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
    let c_resolved = c_resolved.unwrap();
    let c_body = c_resolved.prompt_body.as_deref().unwrap_or_default();
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
        "Hello, {{name}}!\n",
        "@end\n",
        "@block content:\n",
        "{{greet(\"World\")}}\n",
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
    // R6 audit: VirtualFs now emits mds::module_not_found (not mds::file_not_found)
    // when a key is absent from the in-memory module map.  mds::file_not_found is
    // reserved for NativeFs.  The @extends resolver attaches a span via
    // attach_import_span, so the final error may keep the module_not_found code
    // with a span (attach_import_span path) or be wrapped as ImportError for the
    // frontmatter path — either is correct; what must NOT occur is file_not_found.
    let child = "@extends \"./missing.mds\"\n";
    let files = [("child.mds", child)];
    let err = compile_virtual(&files, "child.mds")
        .expect_err("E10: missing base should produce module-not-found");
    let serialized = err.serialize();
    assert!(
        serialized.code == "mds::module_not_found" || serialized.code == "mds::import",
        "E10: expected module_not_found or import, got: {serialized:?}"
    );
    assert_ne!(
        serialized.code, "mds::file_not_found",
        "E10: VirtualFs missing base must not produce file_not_found: {serialized:?}"
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
    let base = "@block content:\n{{undefined_var}}\n@end\n";
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
        "model={{config.model}} temp={{config.temperature}} base={{base_only}}\n",
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
        "model={{config.model}} temp={{config.temperature}} extra={{config.extra}} base={{base_only}}\n",
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
        "{{tool}}\n",
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
        "{{only_in_base}}\n",
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
fn f6_nested_reserved_key_config_type_survives_merge() {
    // F6 / depth-gating fix: a nested key named `type` inside a sub-mapping (e.g.
    // `config.type: mds`) must survive the deep merge and appear in the compiled output.
    // Previously deep_merge_yaml filtered all keys named "type" at every recursion depth,
    // silently dropping user data inside nested objects.
    let base = concat!(
        "---\n",
        "config:\n",
        "  type: mds\n",
        "  model: gpt-4\n",
        "model: base-model\n",
        "---\n",
        "@block content:\n",
        "{{config.model}}\n",
        "@end\n",
    );
    let child = concat!(
        "---\n",
        "role: system\n",
        "---\n",
        "@extends \"./base.mds\"\n",
    );
    let files = [("base.mds", base), ("child.mds", child)];
    let mut cache = virtual_cache(&files);
    let mut warnings = vec![];
    let resolved = cache
        .resolve_key("child.mds", &Default::default(), &mut warnings)
        .expect("F6 nested reserved key: should compile");

    // Compiled output (body) uses config.model from merged scope.
    let body = resolved.prompt_body.as_deref().unwrap_or("");
    assert!(
        body.contains("gpt-4"),
        "config.model must be in scope: {body}"
    );

    // Emitted frontmatter must contain `config.type: mds` — the nested `type` key
    // must NOT be stripped by the reserved-key filter.
    let fm = resolved.raw_frontmatter.as_deref().unwrap_or("");
    assert!(
        fm.contains("type: mds") || fm.contains("type: 'mds'") || fm.contains("\"type\": mds"),
        "config.type nested key must survive into emitted frontmatter; got: {fm}"
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
        "{{role}}\n",
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
    let shared = "@define greeting(name):\nHello {{name}}!\n@end\n";
    let base = concat!(
        "---\n",
        "imports:\n",
        "  - path: ./shared.mds\n",
        "    as: shared\n",
        "---\n",
        "@block content:\n",
        "{{shared.greeting(\"World\")}}\n",
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
    let lib = "@define greet(x):\nHi {{x}}\n@end\n";
    let base = "@block msg:\nDefault message\n@end\n";
    let child = concat!(
        "---\n",
        "imports:\n",
        "  - path: ./lib.mds\n",
        "    as: lib\n",
        "---\n",
        "@extends \"./base.mds\"\n",
        "@block msg:\n",
        "{{lib.greet(\"child\")}}\n",
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
    // F8 (frontmatter-first import ordering): same alias in both base and child frontmatter imports → mds::name_collision.
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
        "{{x}},{{y}}\n",
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
        "{{k}}={{v}};\n",
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
    let lib = "@define greet(name):\nHello {{name}}!\n@end\n";
    let standalone = concat!(
        "---\n",
        "imports:\n",
        "  - path: ./lib.mds\n",
        "    as: lib\n",
        "greeting: World\n",
        "---\n",
        "{{lib.greet(greeting)}}\n",
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
fn f4_extends_emits_deep_merged_frontmatter() {
    // #154 — @extends must emit the deep-merged frontmatter (base < child),
    // not just the child's raw FM. Reserved keys (imports, type, extends) are
    // excluded; non-reserved keys from both sides appear in the output.
    let base = concat!(
        "---\n",
        "base_key: from_base\n",
        "shared_key: base_wins_without_child_override\n",
        "---\n",
        "@block content:\n",
        "{{base_key}}\n",
        "@end\n",
    );
    let child = concat!(
        "---\n",
        "child_key: from_child\n",
        "shared_key: child_overrides_base\n",
        "---\n",
        "@extends \"./base.mds\"\n",
    );
    let files = [("base.mds", base), ("child.mds", child)];
    let mut cache = virtual_cache(&files);
    let mut warnings = vec![];

    let child_resolved = cache
        .resolve_key("child.mds", &Default::default(), &mut warnings)
        .expect("output emission test should compile");

    // raw_frontmatter in the resolved module is the deep-merged FM (base < child).
    let raw_fm = child_resolved
        .raw_frontmatter
        .as_deref()
        .expect("merged output must have frontmatter");
    assert!(
        raw_fm.contains("base_key"),
        "merged output must contain base-only key; got: {raw_fm}"
    );
    assert!(
        raw_fm.contains("child_key"),
        "merged output must contain child-only key; got: {raw_fm}"
    );
    assert!(
        raw_fm.contains("child_overrides_base"),
        "child value must win for shared_key; got: {raw_fm}"
    );
    assert!(
        !raw_fm.contains("base_wins_without_child_override"),
        "base value for shared_key must be overridden; got: {raw_fm}"
    );

    // The compiled body still uses the merged scope (base_key visible to blocks).
    let output = child_resolved.prompt_body.as_deref().unwrap_or("");
    assert!(
        output.contains("from_base"),
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
        "a={{a}} b={{b}} c={{c}}\n",
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
fn compile_virtual_messages_helper(
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
    let messages = compile_virtual_messages_helper(&files, "child.mds")
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
    let messages = compile_virtual_messages_helper(&files, "child.mds")
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
    // with no @message anywhere in the spliced regions, the child compiles to
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
    let err = compile_virtual_messages_helper(&files, "child.mds")
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
    let messages = compile_virtual_messages_helper(&files, "child.mds")
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
    let messages_result = compile_virtual_messages_helper(&msg_refs, "file0.mds");
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
    let err = compile_virtual_messages_helper(&files, "child.mds")
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
        compile_virtual_messages_helper(&files, "c.mds").expect("F9 multilevel: should compile");
    assert_eq!(messages.len(), 1, "F9 multilevel: got {messages:?}");
    assert!(
        messages[0].content.contains("From C."),
        "F9 multilevel: most-derived (C) wins: {:?}",
        messages[0].content
    );
}

// ── PF-004 parity: messages-mode @extends path validates the spliced regions ─
//
// A check on the primary path (text-mode process_module_extends calls
// validator::validate before evaluate) must not be absent on the parallel path
// (messages-mode @extends branch).  This test verifies that an @extends child
// compiled in messages mode whose base-default block references an undefined var
// produces the SAME error (mds::undefined_var) as text mode does.

#[test]
fn pf004_messages_mode_extends_validates_spliced_regions_parity() {
    // Base: @block with @message inside, and an undefined variable.
    // Child: @extends the base, provides no override (uses default).
    let base = concat!(
        "@block content:\n",
        "@message user:\n",
        "Hello {{undefined_var}}.\n",
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
    let messages_err = compile_virtual_messages_helper(&files, "child.mds")
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
    // Interior-verbatim contract (#150/#151): block body bytes pass through
    // exactly as authored; only the directive lines (@block … :, @end) are
    // consumed. The trailing \n before @end is now part of the body.
    //
    // Base source (repr): "Intro.\n\n@block body:\nDefault body.\n@end\n\nAfter.\n"
    //
    // Skeleton nodes after parse:
    //   Text("Intro.\n\n")
    //   Block("body")  body = [Text("Default body.\n")]  ← verbatim, no edge strip
    //   Text("\nAfter.\n")                               ← the blank line + After.
    let base = "Intro.\n\n@block body:\nDefault body.\n@end\n\nAfter.\n";

    // ── Combination 1: base default, no child override ────────────────────
    // Child has no @block override — effective_blocks use the base default.
    // Body text "Default body.\n" + skeleton "\nAfter.\n" → blank line preserved.
    {
        let child = "@extends \"./base.mds\"\n";
        let files = [("base.mds", base), ("child.mds", child)];
        let out = compile_virtual(&files, "child.mds").expect("F11 combo-1: should compile");
        assert_eq!(
            out, "Intro.\n\nDefault body.\n\nAfter.\n",
            "F11 combo-1: base default — body verbatim, trailing \\n + skeleton \\n = blank line"
        );
    }

    // ── Combination 2: override with no surrounding blank lines ───────────
    // Block body bytes (verbatim): "Override.\n"
    // Body text "Override.\n" + skeleton "\nAfter.\n" → blank line.
    {
        let child = "@extends \"./base.mds\"\n@block body:\nOverride.\n@end\n";
        let files = [("base.mds", base), ("child.mds", child)];
        let out = compile_virtual(&files, "child.mds").expect("F11 combo-2: should compile");
        assert_eq!(
            out, "Intro.\n\nOverride.\n\nAfter.\n",
            "F11 combo-2: override without blank lines — trailing \\n + skeleton \\n = blank line"
        );
    }

    // ── Combination 3: override WITH leading+trailing blank lines ─────────
    // Block body bytes (verbatim): "\nOverride.\n\n"
    // Full verbatim run: "Intro.\n\n" + "\nOverride.\n\n" + "\nAfter.\n"
    {
        let child = "@extends \"./base.mds\"\n@block body:\n\nOverride.\n\n@end\n";
        let files = [("base.mds", base), ("child.mds", child)];
        let out = compile_virtual(&files, "child.mds").expect("F11 combo-3: should compile");
        assert_eq!(
            out, "Intro.\n\n\nOverride.\n\n\nAfter.\n",
            "F11 combo-3: override with surrounding blanks — full verbatim run preserved"
        );
    }

    // ── Combination 4: override with indented content ─────────────────────
    // Block body bytes (verbatim): "  Indented.\n"
    // Body "  Indented.\n" + skeleton "\nAfter.\n" → blank line between.
    {
        let child = "@extends \"./base.mds\"\n@block body:\n  Indented.\n@end\n";
        let files = [("base.mds", base), ("child.mds", child)];
        let out = compile_virtual(&files, "child.mds").expect("F11 combo-4: should compile");
        assert_eq!(
            out, "Intro.\n\n  Indented.\n\nAfter.\n",
            "F11 combo-4: indented override — verbatim, trailing \\n + skeleton \\n = blank line"
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
    // base = "@block content:\n{{undefined_var}}\n@end\n"
    // bytes: "@block content:\n" = 16 bytes, then "{undefined_var}" starts at 16.
    let base = "@block content:\n{{undefined_var}}\n@end\n";
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
        "Hello {{undefined_var}}.\n",
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

    let msg_err = compile_virtual_messages_helper(&files, "child.mds")
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
    let child = "@extends \"./base.mds\"\n@block content:\n{{undefined_var}}\n@end\n";
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
        "@block danger:\n{{undefined_var}}\n@end\n",
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
        "Hello {{customer_name}}!\n", // top-level interpolation in skeleton
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
    let base = "@block content:\n{{undefined_var}}\n@end\n";
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
    let base_with_var = "@block content:\n{{x}}\n@end\n";
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

#[test]
fn definitions_and_blocks_share_one_module_source() {
    let source = "@define greet():\nHello\n@end\n@block content:\n{{greet()}}\n@end\n";
    let child = "@extends \"./base.mds\"\n";
    let files = [("base.mds", source), ("child.mds", child)];
    for entry in ["base.mds", "child.mds"] {
        let mut cache = virtual_cache(&files);
        let vars = Default::default();
        let mut warnings = Vec::new();
        cache.resolve_key(entry, &vars, &mut warnings).unwrap();
        let base = Arc::clone(&cache.modules["base.mds"]);
        let module_origin = &base.skeleton_origin;
        assert_eq!(module_origin.source.as_ref(), source);
        assert_eq!(module_origin.display.as_ref(), "base.mds");
        let origins = [
            &base.effective_blocks["content"].origin,
            base.functions["greet"].origin.as_ref().unwrap(),
        ];
        let shared = origins.map(|origin| Arc::ptr_eq(&module_origin.source, &origin.source));
        assert_eq!(shared, [true, true], "entry: {entry}");
        let upgraded = cache.resolve_key("base.mds", &vars, &mut warnings).unwrap();
        assert!(Arc::ptr_eq(
            &module_origin.source,
            &upgraded.skeleton_origin.source
        ));
        assert!(Arc::ptr_eq(
            &base.effective_skeleton,
            &upgraded.effective_skeleton
        ));
    }
}

// ── PF-003 / #133: Windows verbatim-path parent_dir invariant ────────────────

// Guards that `NativeFs::parent_dir` uses `Path::new(key).parent()` — the same
// OS-native path decomposition used everywhere in this codebase — rather than a
// string split on `/`. On Windows, `std::fs::canonicalize` returns a `\\?\`
// verbatim extended-length path where `/` is a LITERAL character, not a separator;
// a string split on `/` would silently return the wrong directory. The Windows
// assertion is gated to `#[cfg(windows)]`; a POSIX assertion runs on every CI
// leg to keep the code path compiled and tested on all platforms.
#[test]
fn pf003_parent_dir_strips_on_windows_verbatim_path() {
    use crate::fs::NativeFs;
    let fs = NativeFs::new();

    // On Windows: verbatim extended-length paths use `\` as the separator;
    // `Path::parent()` correctly strips the filename back to the directory.
    #[cfg(windows)]
    {
        let dir = fs.parent_dir(r"\\?\C:\a\b\c.mds");
        assert_eq!(
            dir, r"\\?\C:\a\b",
            "parent_dir on Windows verbatim path must strip the filename (PF-003 guard)"
        );
    }

    // Cross-platform (POSIX): regular OS-native path decomposition; keeps the
    // code path compiled and exercised on every CI leg (Linux, macOS).
    #[cfg(unix)]
    {
        let dir = fs.parent_dir("/abs/dir/file.mds");
        assert_eq!(
            dir, "/abs/dir",
            "parent_dir on POSIX path must strip the filename"
        );
    }
}

// ── SOURCE_LABEL display-label lock ──────────────────────────────────────────

// Ensures that string-source compile errors include `<source>` as the file
// label in miette diagnostics. SOURCE_LABEL is the `ctx.file_str` for
// string-source modules — it must appear in the rendered diagnostic.
//
// The source must trigger a SPAN-CARRYING error so that `MdsError::at()` sets
// `src: Some(NamedSource::new("<source>", ...))`. Post-parse semantic errors
// without a span (e.g. "cannot use literal in @if condition") have `src: None`
// and will not surface `<source>` in the debug representation.
// A plain `{undefined_var}` interpolation triggers `MdsError::undefined_var_at`
// with a span pointing to the interpolation site — that IS a span-carrying error.
#[test]
fn source_label_appears_in_string_source_diagnostics() {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();

    // An undefined variable interpolation produces MdsError::UndefinedVariable
    // with src: Some(NamedSource { name: "<source>", ... }) and a span pointing
    // to the interpolation site — exactly the span-carrying error we need.
    let bad_source = "{{undefined_var}}\n";
    let result = crate::compile_str_with(bad_source, Some(dir.path()), None);

    let err = result.unwrap_err();
    // The Debug representation of MdsError::UndefinedVariable surfaces
    // `NamedSource { name: "<source>", ... }` when src is Some.
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("<source>"),
        "string-source diagnostic must contain '<source>' as the file label, got:\n{rendered}"
    );
}

// ── Adversarial YAML-injection: core-level twin (#154) ───────────────────────
//
// These tests exercise the resolver's `raw_frontmatter` serialization path
// (serialize_merged_frontmatter / serde_yaml_ng::to_writer) to verify that values
// containing fence-like sequences, YAML document-end markers, or block-scalar
// `type: mds` lines cannot escape the emitted frontmatter block and smuggle
// additional top-level keys into the compiled output.
//
// The CLI-level mirror lives in crates/mds-cli/tests/frontmatter.rs.

#[test]
fn adversarial_injection_fence_sequence_in_fm_value() {
    // A frontmatter value containing a literal `\n---\n` YAML document separator
    // must be serialized as a quoted/escaped YAML string and must NOT break the
    // outer `---`/`---` fence structure.
    let base = concat!(
        "---\n",
        "prompt: \"start\\n---\\ninjected: evil\\n---\\nend\"\n",
        "---\n",
        "@block body:\ndefault\n@end\n",
    );
    let child = concat!("---\nrole: system\n---\n", "@extends \"./base.mds\"\n");
    let files = [("base.mds", base), ("child.mds", child)];
    let mut cache = virtual_cache(&files);
    let resolved = cache
        .resolve_key("child.mds", &Default::default(), &mut vec![])
        .expect("should compile despite adversarial FM value");

    let raw_fm = resolved
        .raw_frontmatter
        .as_deref()
        .expect("merged output must have raw_frontmatter");

    // Extract ONLY the frontmatter section (between the first --- and second ---).
    // Using the full raw_frontmatter string directly since it's already the inner YAML.
    let has_injected = raw_fm.lines().any(|l| l == "injected: evil");
    assert!(
        !has_injected,
        "adversarial 'injected: evil' must not appear as a top-level FM key; raw_fm={raw_fm}"
    );
    assert!(
        raw_fm.contains("role: system"),
        "role key must survive; raw_fm={raw_fm}"
    );
    assert!(
        raw_fm.contains("prompt:"),
        "prompt key must survive; raw_fm={raw_fm}"
    );
}

#[test]
fn adversarial_injection_yaml_end_marker_in_fm_value() {
    // A frontmatter value containing `\n...\n` (YAML stream-end marker) must
    // be serialized safely and must not close the document prematurely.
    let base = concat!(
        "---\n",
        "note: \"before\\n...\\nafter\"\n",
        "---\n",
        "@block body:\ndefault\n@end\n",
    );
    let child = concat!("---\nrole: user\n---\n", "@extends \"./base.mds\"\n");
    let files = [("base.mds", base), ("child.mds", child)];
    let mut cache = virtual_cache(&files);
    let resolved = cache
        .resolve_key("child.mds", &Default::default(), &mut vec![])
        .expect("should compile despite YAML end-marker in value");

    let raw_fm = resolved
        .raw_frontmatter
        .as_deref()
        .expect("merged output must have raw_frontmatter");

    // The end-marker must not split the frontmatter YAML; both keys must appear.
    assert!(
        raw_fm.contains("role: user"),
        "role key must be in raw_fm; got: {raw_fm}"
    );
    assert!(
        raw_fm.contains("note:"),
        "note key must be in raw_fm; got: {raw_fm}"
    );
}

#[test]
fn adversarial_injection_block_scalar_type_mds_not_stripped() {
    // A block-scalar FM value whose lines include `type: mds` must not be
    // confused with the top-level `type: mds` directive — only an unindented
    // top-level `type: mds` key should be stripped from the emitted output.
    let base = concat!(
        "---\n",
        "description: |\n",
        "  type: mds\n",
        "  this is a block scalar\n",
        "model: gpt-4\n",
        "---\n",
        "@block body:\ndefault\n@end\n",
    );
    let child = "@extends \"./base.mds\"\n";
    let files = [("base.mds", base), ("child.mds", child)];
    let mut cache = virtual_cache(&files);
    let resolved = cache
        .resolve_key("child.mds", &Default::default(), &mut vec![])
        .expect("should compile");

    let raw_fm = resolved
        .raw_frontmatter
        .as_deref()
        .expect("merged output must have raw_frontmatter");

    // No unindented `type: mds` key in the raw frontmatter.
    let has_top_level_type = raw_fm.lines().any(|l| l == "type: mds");
    assert!(
        !has_top_level_type,
        "unindented 'type: mds' must not appear as a top-level key; raw_fm={raw_fm}"
    );
    assert!(
        raw_fm.contains("description:"),
        "description key must be present; raw_fm={raw_fm}"
    );
    assert!(
        raw_fm.contains("model:"),
        "model key must be present; raw_fm={raw_fm}"
    );
}

// ── f7 extension: emitted FM ignores --set (#154) ────────────────────────────

#[test]
fn f7_emitted_frontmatter_ignores_runtime_set() {
    // Extending an extends test to pass a runtime var whose key collides with a
    // frontmatter key: the emitted raw_frontmatter must NOT contain the runtime
    // value. Runtime vars override the SCOPE (what the body renders) but must
    // never alter the emitted raw frontmatter serialization.
    let base = concat!(
        "---\n",
        "model: gpt-4\n",
        "---\n",
        "@block content:\n",
        "Model: {{model}}\n",
        "@end\n",
    );
    let child = concat!(
        "---\n",
        "role: assistant\n",
        "---\n",
        "@extends \"./base.mds\"\n",
    );
    let files = [("base.mds", base), ("child.mds", child)];
    let mut runtime_vars = std::collections::HashMap::new();
    runtime_vars.insert(
        "model".to_string(),
        Value::String("gpt-3.5-turbo".to_string()),
    );
    let mut cache = virtual_cache(&files);
    let resolved = cache
        .resolve_key("child.mds", &runtime_vars, &mut vec![])
        .expect("F7 extension: should compile with runtime override");

    // Body renders the runtime value (scope wins).
    let body = resolved.prompt_body.as_deref().unwrap_or("");
    assert!(
        body.contains("gpt-3.5-turbo"),
        "body must use runtime var value; body={body}"
    );

    // Emitted raw_frontmatter must show the MERGED FRONTMATTER value, not the runtime one.
    let raw_fm = resolved
        .raw_frontmatter
        .as_deref()
        .expect("merged output must have raw_frontmatter");
    assert!(
        !raw_fm.contains("gpt-3.5-turbo"),
        "runtime value must NOT appear in emitted frontmatter; raw_fm={raw_fm}"
    );
    assert!(
        raw_fm.contains("gpt-4"),
        "frontmatter must show merged FM value (gpt-4); raw_fm={raw_fm}"
    );
}

// ── PF-003 / #146: parent_dir cross-platform integration test ─────────────────
//
// Verifies that `FileSystem::parent_dir` is correctly called and consumed during
// real compilation on every CI leg (Linux, macOS). When an imported file (sub/lib.mds)
// itself contains a relative `@import`, the resolver calls `parent_dir` on sub/lib.mds's
// canonical key to locate sub/ before resolving the nested import. Without a correct
// `parent_dir`, the nested `./helper.mds` import would resolve against the wrong
// directory and fail with mds::file_not_found. Auto-covers Windows once #147 lands.
#[test]
fn parent_dir_drives_nested_file_import_resolution() {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir_all(&sub).unwrap();

    // sub/helper.mds — exists on disk so the import resolves; contributes nothing to scope.
    std::fs::write(sub.join("helper.mds"), "").unwrap();

    // sub/lib.mds — imports ./helper.mds (relative: resolution forces parent_dir call
    // on sub/lib.mds's canonical key) and defines greet(x) for main.mds to consume.
    std::fs::write(
        sub.join("lib.mds"),
        "@import \"./helper.mds\"\n@define greet(x):\nHello {{x}}!\n@end\n",
    )
    .unwrap();

    // main.mds — imports sub/lib.mds and calls greet
    let main_path = dir.path().join("main.mds");
    std::fs::write(
        &main_path,
        "@import \"./sub/lib.mds\"\n{{greet(\"World\")}}\n",
    )
    .unwrap();

    let result = crate::compile_file(main_path.to_str().unwrap());
    assert!(
        result.is_ok(),
        "nested file import chain must compile (parent_dir cross-platform guard, #146): {result:?}"
    );
    let output = result.unwrap().into_markdown().unwrap();
    assert!(
        output.contains("Hello World!"),
        "nested import must produce expected output (parent_dir resolved sub/ correctly): {output}"
    );
}

// ── line_len_at — UTF-8 char-boundary safety (issue #29) ─────────────────────
//
// These tests verify `line_len_at` degrades to 0 rather than panicking on
// non-boundary or out-of-bounds offsets, matching the guard in
// `build_type_mismatch` (evaluator.rs) per ADR-005.
//
// Since #220 `line_len_at` is also the helper behind `attach_import_span`
// (resolver.rs) and `check_child_only_blocks` (resolver/inheritance.rs). Both
// used to slice `source` directly below a `debug_assert!`, so a non-boundary
// offset panicked in release; they now route the underline length through this
// helper and degrade to a zero-length span.

#[test]
fn line_len_at_out_of_bounds_returns_zero() {
    // offset past the end of source must return 0, never panic.
    assert_eq!(
        line_len_at("hello", 10),
        0,
        "out-of-bounds offset must return 0"
    );
}

#[test]
fn line_len_at_non_boundary_multibyte_returns_zero() {
    // "é" is a 2-byte UTF-8 sequence (0xC3 0xA9).
    // offset 1 is the second byte — not a char boundary.
    let s = "é\nmore";
    assert_eq!(
        line_len_at(s, 1),
        0,
        "offset inside a multi-byte char must return 0, not panic"
    );
}

#[test]
fn line_len_at_valid_boundary_returns_line_length() {
    // "héllo\nworld": h=1, é=2, l=1, l=1, o=1 → 6 bytes before '\n'.
    let s = "héllo\nworld";
    assert_eq!(
        line_len_at(s, 0),
        6,
        "offset 0 must return bytes to the first newline"
    );
}

#[test]
fn line_len_at_mid_string_boundary() {
    // offset 3 (after "hé" = 3 bytes) is on a char boundary.
    // remaining: "llo\nworld" → 3 bytes to '\n'.
    let s = "héllo\nworld";
    assert_eq!(
        line_len_at(s, 3),
        3,
        "offset at a mid-string char boundary must return bytes to the next newline"
    );
}

#[test]
fn line_len_at_no_newline_returns_remaining_len() {
    let s = "hello";
    assert_eq!(
        line_len_at(s, 0),
        5,
        "when no newline exists, must return the remaining length"
    );
}

#[test]
fn line_len_at_offset_at_end_returns_zero() {
    // offset == source.len() is a valid char boundary (end of string).
    // The slice [len..] is empty, so find('\n') is None and .len() is 0.
    let s = "hello";
    assert_eq!(
        line_len_at(s, 5),
        0,
        "offset == source.len() must return 0 (empty trailing slice)"
    );
}

// ── #220: span attribution must hold in RELEASE builds too ───────────────────
//
// Two different remedies, chosen per site by what a defect there would cost:
//
// * `attach_import_span` and `check_child_only_blocks` DEGRADE. Both used to slice
//   `source[offset..]` one line below a `debug_assert!` on the very condition the
//   slice needs, so a bad offset panicked the compiler in release and merely
//   changed WHICH assert fired in debug. Routing them through `line_len_at` yields
//   a zero-length span: the numeric offset survives, the snippet is dropped, and no
//   caret is painted over the wrong bytes.
// * `spliced_regions` FAILS LOUDLY. Its release fallback spliced the base default in
//   silence, i.e. dropped a child's `@block` override from the compiled output with
//   no diagnostic at all — strictly worse than a panic.

/// Render a `catch_unwind` payload as text, for both `&'static str` and `String`
/// panic payloads (a formatted `assert!` message is always the latter).
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else {
        "<non-string panic payload>".to_string()
    }
}

#[test]
fn attach_import_span_non_boundary_offset_degrades_to_zero_len_span() {
    // "é" is a 2-byte UTF-8 sequence; byte offset 1 sits inside it and is therefore
    // never a char boundary.
    //
    // The DEBUG-profile branch deliberately depends on `MdsError::at()` (error.rs)
    // keeping its OWN debug-only canary for the cross-source case. That assert is not
    // promoted: promoting it would make the release branch below panic as well, which
    // is exactly the degradation this test exists to forbid.
    let caught = std::panic::catch_unwind(|| {
        attach_import_span(
            MdsError::file_not_found("./x.mds"),
            "./x.mds",
            "<child>",
            "é\n",
            1,
        )
    });

    if cfg!(debug_assertions) {
        let payload = caught.expect_err("debug builds must still trip the MdsError::at() canary");
        let msg = panic_message(&*payload);
        assert!(
            msg.contains("MdsError::at(): cross-source offset mismatch"),
            "the surviving debug canary must be the one in MdsError::at(), which proves \
             attach_import_span degraded to a zero-length span instead of slicing; got: {msg}"
        );
        assert!(
            !msg.contains("attach_import_span:"),
            "attach_import_span must no longer carry a boundary assert of its own; got: {msg}"
        );
    } else {
        let err = caught.unwrap_or_else(|payload| {
            panic!(
                "release builds must not panic on a non-boundary offset; got: {}",
                panic_message(&*payload)
            )
        });
        match err {
            MdsError::FileNotFound { span, src, .. } => {
                let span = span.expect("the numeric offset must survive the degradation");
                assert_eq!(span.offset(), 1, "the offset must be preserved verbatim");
                assert_eq!(span.len(), 0, "a degraded span must have zero length");
                assert!(
                    src.is_none(),
                    "no snippet may be attached when the offset cannot be trusted"
                );
            }
            other => panic!("expected a FileNotFound error, got: {other:?}"),
        }
    }
}

#[test]
fn attach_import_span_out_of_range_offset_on_empty_source_degrades() {
    // An empty source is `MdsError::at()`'s documented exemption, so no canary fires in
    // either profile: the only thing that could panic here is the raw `source[offset..]`
    // slice this site used to perform.
    let err = attach_import_span(
        MdsError::file_not_found("./x.mds"),
        "./x.mds",
        "<child>",
        "",
        1,
    );
    match err {
        MdsError::FileNotFound { span, src, .. } => {
            let span = span.expect("the numeric offset must survive the degradation");
            assert_eq!(span.offset(), 1, "the offset must be preserved verbatim");
            assert_eq!(span.len(), 0, "a degraded span must have zero length");
            assert!(src.is_none(), "no snippet may be attached");
        }
        other => panic!("expected a FileNotFound error, got: {other:?}"),
    }
}

#[test]
fn attach_import_span_valid_offset_spans_whole_directive_and_passes_other_errors() {
    // Positive control: a trustworthy offset must still underline the whole directive
    // and still carry the snippet, so the degradation above is not a blanket downgrade.
    let source = "@import \"./x.mds\"\nnext\n";
    let err = attach_import_span(
        MdsError::file_not_found("./x.mds"),
        "./x.mds",
        "child.mds",
        source,
        0,
    );
    match err {
        MdsError::FileNotFound { span, src, .. } => {
            let span = span.expect("a valid offset must produce a span");
            assert_eq!(span.offset(), 0);
            assert_eq!(
                span.len(),
                17,
                "the span must cover `@import \"./x.mds\"` exactly (17 bytes, no newline)"
            );
            assert!(
                src.is_some(),
                "a trustworthy offset must still carry the source snippet"
            );
        }
        other => panic!("expected a FileNotFound error, got: {other:?}"),
    }

    // Control: variants outside the three span-attaching arms are returned unchanged.
    let passthrough = attach_import_span(MdsError::syntax("x"), "./x.mds", "child.mds", source, 0);
    match passthrough {
        MdsError::Syntax { message, span, src } => {
            assert_eq!(message, "x", "the message must be untouched");
            assert!(span.is_none(), "no span may be attached to a syntax error");
            assert!(
                src.is_none(),
                "no snippet may be attached to a syntax error"
            );
        }
        other => panic!("expected the syntax error to pass through, got: {other:?}"),
    }
}

/// Build the borrowed module context the `@extends` helpers take.
fn extends_ctx<'a>(source: &'a str, vars: &'a Arc<VarLayer>) -> ModuleCtx<'a> {
    ModuleCtx {
        key: "<child>",
        file_str: "<child>",
        source,
        base_dir: ".",
        runtime_vars: vars,
        origin: OnceCell::new(),
    }
}

#[test]
fn check_child_only_blocks_non_boundary_offset_degrades() {
    // Same shape as `attach_import_span`: offset 1 is inside the 2-byte "é".
    let caught = std::panic::catch_unwind(|| {
        let vars = Arc::default();
        let ctx = extends_ctx("é\n", &vars);
        let body = vec![Node::EscapedBrace { offset: 1 }];
        check_child_only_blocks(&body, &ctx)
    });

    if cfg!(debug_assertions) {
        let payload = caught.expect_err("debug builds must still trip the MdsError::at() canary");
        let msg = panic_message(&*payload);
        assert!(
            msg.contains("MdsError::at(): cross-source offset mismatch"),
            "the surviving debug canary must be the one in MdsError::at(), which proves \
             check_child_only_blocks degraded instead of slicing; got: {msg}"
        );
        assert!(
            !msg.contains("check_child_only_blocks:"),
            "check_child_only_blocks must no longer carry a boundary assert of its own; got: {msg}"
        );
    } else {
        let result = caught.unwrap_or_else(|payload| {
            panic!(
                "release builds must not panic on a non-boundary offset; got: {}",
                panic_message(&*payload)
            )
        });
        match result {
            Err(MdsError::Extends { span, src, .. }) => {
                let span = span.expect("the numeric offset must survive the degradation");
                assert_eq!(span.offset(), 1, "the offset must be preserved verbatim");
                assert_eq!(span.len(), 0, "a degraded span must have zero length");
                assert!(src.is_none(), "no snippet may be attached");
            }
            other => panic!("expected an Extends error, got: {other:?}"),
        }
    }
}

#[test]
fn check_child_only_blocks_out_of_range_offset_on_empty_source_degrades() {
    let vars = Arc::default();
    let ctx = extends_ctx("", &vars);
    let body = vec![Node::EscapedBrace { offset: 1 }];
    match check_child_only_blocks(&body, &ctx) {
        Err(MdsError::Extends { span, src, .. }) => {
            let span = span.expect("the numeric offset must survive the degradation");
            assert_eq!(span.offset(), 1, "the offset must be preserved verbatim");
            assert_eq!(span.len(), 0, "a degraded span must have zero length");
            assert!(src.is_none(), "no snippet may be attached");
        }
        other => panic!("expected an Extends error, got: {other:?}"),
    }
}

#[test]
fn check_child_only_blocks_valid_offset_spans_stray_line_and_accepts_blocks() {
    // Positive control: a trustworthy offset still underlines the stray line and still
    // carries the snippet; and a well-formed child body is still accepted.
    let source = "@block a:\n@end\n\\{{ x\n";
    let vars = Arc::default();
    let ctx = extends_ctx(source, &vars);

    let stray = vec![
        Node::Block(BlockNode {
            name: "a".to_string(),
            body: vec![],
            offset: 0,
        }),
        Node::EscapedBrace { offset: 15 },
    ];
    match check_child_only_blocks(&stray, &ctx) {
        Err(MdsError::Extends { span, src, .. }) => {
            let span = span.expect("a valid offset must produce a span");
            assert_eq!(span.offset(), 15);
            assert_eq!(
                span.len(),
                5,
                "the span must cover the stray `\\{{{{ x` line (5 bytes, no newline)"
            );
            assert!(
                src.is_some(),
                "a trustworthy offset must still carry the source snippet"
            );
        }
        other => panic!("expected an Extends error, got: {other:?}"),
    }

    let clean = vec![
        Node::Block(BlockNode {
            name: "a".to_string(),
            body: vec![],
            offset: 0,
        }),
        Node::Text(crate::ast::TextNode {
            text: "\n".to_string(),
            offset: 14,
        }),
    ];
    assert!(
        check_child_only_blocks(&clean, &ctx).is_ok(),
        "@block nodes plus whitespace-only text must still be accepted"
    );
}

#[test]
#[should_panic(expected = "override map was not built for this skeleton")]
fn spliced_regions_missing_effective_block_panics_in_every_profile() {
    let skeleton = vec![Node::Block(BlockNode {
        name: "content".to_string(),
        body: vec![],
        offset: 0,
    })];
    let effective: IndexMap<String, EffectiveBlock> = IndexMap::new();
    let skeleton_origin = Origin {
        file: Arc::from("base.mds"),
        display: Arc::from("base.mds"),
        source: Arc::from("@block content:\n@end\n"),
    };
    let _ = spliced_regions(&skeleton, &effective, &skeleton_origin);
}

#[test]
fn spliced_regions_pairs_every_skeleton_block_with_its_effective_entry() {
    // Positive control for the assert above: when the map IS built, every region must
    // carry the origin of the file its offsets index into — the override's for a block
    // placeholder, the skeleton's for everything else.
    let skeleton = vec![
        Node::Text(crate::ast::TextNode {
            text: "intro\n".to_string(),
            offset: 0,
        }),
        Node::Block(BlockNode {
            name: "content".to_string(),
            body: vec![],
            offset: 6,
        }),
    ];
    let skeleton_origin = Origin {
        file: Arc::from("base.mds"),
        display: Arc::from("base.mds"),
        source: Arc::from("intro\n@block content:\n@end\n"),
    };
    let child_origin = Origin {
        file: Arc::from("child.mds"),
        display: Arc::from("child.mds"),
        source: Arc::from("@extends \"./base.mds\"\n@block content:\nhi\n@end\n"),
    };

    let mut effective: IndexMap<String, EffectiveBlock> = IndexMap::new();
    effective.insert(
        "content".to_string(),
        EffectiveBlock {
            node: Arc::new(BlockNode {
                name: "content".to_string(),
                body: vec![Node::Text(crate::ast::TextNode {
                    text: "hi\n".to_string(),
                    offset: 37,
                })],
                offset: 22,
            }),
            origin: child_origin,
        },
    );

    let regions = spliced_regions(&skeleton, &effective, &skeleton_origin);
    assert_eq!(
        regions.len(),
        2,
        "one region per skeleton node (text + block placeholder)"
    );
    assert_eq!(
        regions[1].0.len(),
        1,
        "the placeholder must yield the effective block's body nodes, not the skeleton's"
    );

    let entry = effective
        .get("content")
        .expect("the effective entry must still be in the map");
    assert!(
        Arc::ptr_eq(&regions[1].1.source, &entry.origin.source),
        "the block region must carry the OVERRIDING file's origin"
    );
    assert!(
        Arc::ptr_eq(&regions[0].1.source, &skeleton_origin.source),
        "non-block regions must carry the skeleton origin"
    );
}

// ── R3 / CWE-209: construction-time display-path relativization ──────────────
//
// These tests pin the diagnostic display contract on the NativeFs backend:
// NamedSource names and diagnostic file labels are project-root-relative (never
// absolute), while emitted source-map sources[] stay exactly as they were
// pre-R3 (map-relative when source_map_base is set — ADR-005 byte-identity).
// Every absence assertion is paired with a PF-013 positive control.

/// Temp project with a `.mdsroot` marker at the root and a `sub/` directory.
///
/// The marker makes the `find_project_root` walk-up deterministic; the returned
/// root is canonicalized (macOS resolves `/var` → `/private/var`) so prefix
/// checks compare against the same form the canonical module keys carry.
fn r3_project() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::write(root.join(".mdsroot"), "").unwrap();
    std::fs::create_dir_all(root.join("sub")).unwrap();
    (dir, root)
}

#[test]
fn r3_compile_error_named_source_is_root_relative() {
    let (_guard, root) = r3_project();
    let entry = root.join("sub").join("err.mds");
    std::fs::write(&entry, "Hello {{undefined_var}}!\n").unwrap();

    let err = crate::compile(&entry, None).unwrap_err();
    let name = err
        .source_name()
        .expect("undefined-variable error must carry a NamedSource");

    // Positive control (PF-013): the canonical key IS absolute — the absence
    // assertions below would catch it if it leaked into the name.
    let canonical = entry.canonicalize().unwrap();
    assert!(
        canonical.is_absolute(),
        "positive control: canonical key must be absolute"
    );
    assert_eq!(
        name, "sub/err.mds",
        "NamedSource name must be project-root-relative"
    );
    assert!(
        !std::path::Path::new(name).is_absolute(),
        "NamedSource name must never be absolute; got: {name}"
    );
}

#[test]
fn r3_cross_file_error_keeps_own_files_display() {
    // PF-012: relativize the EXISTING label — never substitute another file's.
    // The error originates in sub/lib.mds; its display must name sub/lib.mds,
    // not the importing main.mds and never the absolute canonical key.
    let (_guard, root) = r3_project();
    std::fs::write(root.join("main.mds"), "@import \"./sub/lib.mds\"\nbody\n").unwrap();
    let lib = root.join("sub").join("lib.mds");
    std::fs::write(&lib, "{{undefined_var}}\n").unwrap();

    let err = crate::compile(root.join("main.mds"), None).unwrap_err();
    let name = err
        .source_name()
        .expect("imported-file error must carry a NamedSource");

    // Positive control (PF-013): the canonical key of the ERRORING file is
    // absolute; the equality below would fail if it leaked.
    assert!(
        lib.canonicalize().unwrap().is_absolute(),
        "positive control: canonical key must be absolute"
    );
    assert_eq!(
        name, "sub/lib.mds",
        "cross-file error must keep its OWN file's root-relative display (PF-012)"
    );
    assert_ne!(
        name, "main.mds",
        "cross-file error must not be attributed to the importing file"
    );
}

#[test]
fn r3_sources_stay_map_relative_with_source_map_base() {
    // ADR-005 byte-identity: sources[] emission reads each registered Origin's
    // canonical key (relativized after finalize), never its display. With
    // source_map_base set, the entries are exactly the pre-R3 map-relative
    // values. The CLI-level goldens (cli_source_map.rs SM-GOLD / SM-DET) pin
    // the end-to-end surface; this is the core-level pin with an explicit base.
    let (_guard, root) = r3_project();
    let entry = root.join("main.mds");
    std::fs::write(&entry, "@import \"./sub/lib.mds\"\n{{greet(\"World\")}}\n").unwrap();
    std::fs::write(
        root.join("sub").join("lib.mds"),
        "@define greet(x):\nHello {{x}}!\n@end\n",
    )
    .unwrap();

    let opts = crate::CompileOptions::default()
        .with_source_map(true)
        .with_source_map_base(Some(root.join("out")));
    let result = crate::compile_with_deps_opts(&entry, None, opts).expect("compile must succeed");
    let sm = result.source_map.expect("source map must be emitted");

    // Exact pre-R3 expected values: anchored to the MAP location (root/out),
    // so both entries climb one level (map-relative, inside the root).
    assert_eq!(
        sm.sources,
        vec!["../main.mds".to_string(), "../sub/lib.mds".to_string()],
        "sources[] must be byte-identical to the pre-R3 map-relative form"
    );
    // Positive control (PF-013): the canonical keys ARE absolute — the loop
    // below would catch one leaking through.
    assert!(
        entry.canonicalize().unwrap().is_absolute(),
        "positive control: canonical key must be absolute"
    );
    for s in &sm.sources {
        assert!(
            !std::path::Path::new(s).is_absolute(),
            "sources[] entry must never be absolute; got: {s}"
        );
    }
}

#[test]
fn r3_map_mode_eval_diagnostic_display_is_root_relative() {
    // Map-mode diagnostics derive their file label from the current source's
    // Origin.display (evaluate_with_map_seeded), not from its canonical key. A
    // cross-type comparison errors at eval time — exactly the path that reads
    // that display — and must show the root-relative display.
    let (_guard, root) = r3_project();
    let entry = root.join("sub").join("cond.mds");
    std::fs::write(&entry, "---\nx: 3\n---\n@if x == \"3\":\nyes\n@end\n").unwrap();

    let opts = crate::CompileOptions::default().with_source_map(true);
    let err = crate::compile_with_deps_opts(&entry, None, opts).unwrap_err();
    let name = err
        .source_name()
        .expect("map-mode type-mismatch must carry a NamedSource");

    // Positive control (PF-013): the canonical key IS absolute.
    assert!(
        entry.canonicalize().unwrap().is_absolute(),
        "positive control: canonical key must be absolute"
    );
    assert_eq!(
        name, "sub/cond.mds",
        "map-mode diagnostic must use the root-relative display"
    );
    assert!(
        !std::path::Path::new(name).is_absolute(),
        "map-mode diagnostic label must never be absolute; got: {name}"
    );
}

#[test]
fn r3_extends_errors_name_the_base_root_relative() {
    // #114 / AC-114-2: an error raised inside inherited base content names the base by
    // its root-relative display, never its absolute canonical key — for validation
    // (an undefined variable in a base-default block) and for evaluation (a cross-type
    // comparison in the base skeleton), with source maps on and off.
    let (_guard, root) = r3_project();
    let cases = [
        (
            "validation",
            "@block greeting:\nHello {{customer_name}}, welcome.\n@end\n",
            "mds::undefined_var",
        ),
        (
            "evaluation",
            "---\nn: hi\n---\n@if n == 5:\nx\n@end\n@block body:\ndefault\n@end\n",
            "mds::type_mismatch",
        ),
    ];
    for (stage, base, code) in cases {
        let dir = root.join(stage);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("base.mds"), base).unwrap();
        let child = dir.join("child.mds");
        std::fs::write(&child, "@extends \"./base.mds\"\n").unwrap();

        // Positive control (PF-013): the base's canonical key IS absolute, so the
        // equality below fails if it leaks into the name.
        assert!(
            dir.join("base.mds").canonicalize().unwrap().is_absolute(),
            "positive control: canonical key must be absolute"
        );
        for source_map in [false, true] {
            let opts = crate::CompileOptions::default().with_source_map(source_map);
            let err = crate::compile_with_deps_opts(&child, None, opts).unwrap_err();
            assert_eq!(
                err.serialize().code,
                code,
                "{stage}, source_map={source_map}: {err}"
            );
            assert_eq!(
                err.source_name(),
                Some(format!("{stage}/base.mds").as_str()),
                "{stage}, source_map={source_map}: the error must name the base by its \
                 root-relative display"
            );
        }
    }
}

// ── #416: the source-map builder shares module sources ──────────────────────

fn arc_origin(file: &str, display: &str, source: &str) -> Origin {
    Origin {
        file: Arc::from(file),
        display: Arc::from(display),
        source: Arc::from(source),
    }
}

fn parse_nodes(source: &str) -> Vec<Node> {
    let tokens = tokenize(source, "t.mds").expect("fixture must tokenize");
    parse_with_ctx(&tokens, "t.mds", source)
        .expect("fixture must parse")
        .body
}

#[test]
fn stripped_fence_opener_maps_its_prefix_and_one_source_byte() {
    let source = "  ```js \t+mds\n  @if 1 == 1:\n  keep\n  @end\n  ```\n";
    let origin = arc_origin("/p/main.mds", "main.mds", source);
    let nodes = parse_nodes(source);
    let (output, map) = ModuleCache::evaluate_regions_with_map(
        &[(&nodes, &origin)],
        &mut Scope::new(),
        &mut vec![],
        Some(crate::sourcemap::MapBuilder::new(origin.clone())),
        &mut EvalBudget::default(),
    )
    .unwrap();

    assert_eq!(output, "  ```js\n  keep\n  ```\n");
    let segments: Vec<_> = map
        .unwrap()
        .segments
        .iter()
        .map(|s| (s.out, s.src, s.src_off, s.len))
        .collect();
    assert_eq!(segments, [(0, 0, 0, 8), (8, 0, 28, 7), (15, 0, 42, 6)]);
    // The opener's synthetic LF takes the place of the space after `js`.
    assert_eq!(&source[..8], "  ```js ");
}

/// #416: after region-by-region evaluation every builder entry is the region
/// origin that registered it, pointer-identical to that module's source — including
/// when a later region carries another `Origin` value of an already-registered module
/// (a clone, or a separate allocation of the same key).
#[test]
fn map_builder_shares_origin_arc_across_regions() {
    let base = arc_origin("/p/base.mds", "base.mds", "one {{v}}\ntwo\n");
    let child = arc_origin("/p/child.mds", "child.mds", "child {{v}}\n");
    let base_clone = base.clone();
    let base_copy = arc_origin("/p/base.mds", "base.mds", "one {{v}}\ntwo\n");
    let base_nodes = parse_nodes(&base.source);
    let child_nodes = parse_nodes(&child.source);
    let regions: Vec<(&[Node], &Origin)> = vec![
        (&base_nodes[..1], &base),
        (&base_nodes[1..], &base),
        (&child_nodes, &child),
        (&base_nodes, &base_clone),
        (&base_nodes, &base_copy),
    ];
    let mut scope = Scope::new();
    scope.set_var("v", Value::String("V".to_string()));

    let (output, map) = ModuleCache::evaluate_regions_with_map(
        &regions,
        &mut scope,
        &mut vec![],
        Some(crate::sourcemap::MapBuilder::new(base.clone())),
        &mut EvalBudget::default(),
    )
    .expect("regions must evaluate");
    let map = map.expect("the builder must be handed back");

    assert_eq!(output, "one V\ntwo\nchild V\none V\ntwo\none V\ntwo\n");
    assert_eq!(map.sources().len(), 2, "one entry per module key");
    for (idx, expected) in [(0, &base), (1, &child)] {
        let entry = map.sources().get(idx).expect("registered");
        assert!(
            Arc::ptr_eq(&entry.source, &expected.source),
            "entry {idx}: source must be the module's own Arc"
        );
    }
    // Control: the separate allocation is not pointer-equal to the stored entry.
    assert!(!Arc::ptr_eq(&base_copy.source, &base.source));
    assert!(
        map.segments.iter().any(|s| s.src == 1),
        "non-vacuity: the child region records against its own entry"
    );
}

/// #416, the imported extending-module arm (#412): the map of an extending
/// module reached through `@import` records its chain's own origins, so after an
/// importer splices it every builder entry is pointer-identical to the source its
/// module was loaded with — the root base, the intermediate and the leaf.
#[test]
fn map_builder_shares_origin_arc_across_an_imported_extending_splice() {
    let mut cache = virtual_cache(&[
        (
            "a.mds",
            "A-HEAD\n@block b1:\nA-ONE\n@end\n@block b2:\nA-TWO\n@end\n",
        ),
        ("b.mds", "@extends \"./a.mds\"\n@block b1:\nB-ONE\n@end\n"),
        ("c.mds", "@extends \"./b.mds\"\n@block b2:\nC-TWO\n@end\n"),
    ]);
    cache.source_map_mode = true;
    let leaf = cache
        .resolve_key("c.mds", &HashMap::new(), &mut vec![])
        .expect("the chain must resolve");
    let fragment = leaf
        .prompt_map
        .clone()
        .expect("an extending module with a non-empty prompt must carry a map");
    let root = &cache
        .modules
        .get("a.mds")
        .expect("root cached")
        .skeleton_origin;
    let chain = [
        &leaf.skeleton_origin,
        &leaf.effective_blocks["b1"].origin,
        &leaf.effective_blocks["b2"].origin,
    ];
    assert!(
        Arc::ptr_eq(&chain[0].source, &root.source),
        "the chain's root origin is the one the cache loaded"
    );

    let main = arc_origin("main.mds", "main.mds", "@include c\n");
    let mut importer = crate::sourcemap::MapBuilder::new(main.clone());
    importer.splice_fragment(&fragment, 0);
    let expected = [&main, chain[0], chain[1], chain[2]];
    assert_eq!(
        importer.sources().len(),
        expected.len(),
        "one entry per file"
    );
    for (idx, (entry, want)) in importer.sources().iter().zip(expected).enumerate() {
        assert!(
            Arc::ptr_eq(&entry.source, &want.source),
            "entry {idx}: source must be the module's own Arc"
        );
        assert!(
            Arc::ptr_eq(&entry.display, &want.display),
            "entry {idx}: display must be the module's own Arc"
        );
    }
    let displays: Vec<&str> = importer.sources().iter().map(|o| &*o.display).collect();
    assert_eq!(displays, ["main.mds", "a.mds", "b.mds", "c.mds"]);
    // Control: an equal-content source in its own allocation is not pointer-equal.
    assert!(!Arc::ptr_eq(&Arc::<str>::from(&*root.source), &root.source));
}

/// #412: an imported module keeps a map only when its `prompt` is exported and
/// renders text — standalone and extending alike — while its body is evaluated either
/// way (WARN-B reads a not-exported module's text). Observed on the module itself: an
/// importer cannot tell, because it drops a not-exported map and splices nothing for an
/// empty body. Each gate has a control that keeps its map.
#[test]
fn prompt_map_is_kept_only_for_an_exported_prompt_that_renders_text() {
    let mut cache = virtual_cache(&[
        ("lib.mds", "LIB\n"),
        ("hidden.mds", "@define f():\nF\n@end\n@export f\nHIDDEN\n"),
        ("defs.mds", "@define f():\nF\n@end\n"),
        ("base.mds", "@block body:\nBASE\n@end\n"),
        (
            "child.mds",
            "@extends \"./base.mds\"\n@block body:\nCHILD\n@end\n",
        ),
        (
            "blank.mds",
            "@extends \"./base.mds\"\n@block body:\n\n@end\n",
        ),
    ]);
    cache.source_map_mode = true;
    // (module, its prompt body, whether a map is kept)
    let rows = [
        ("lib.mds", Some("LIB\n"), true),
        ("hidden.mds", Some("HIDDEN\n"), false),
        ("defs.mds", None, false),
        ("child.mds", Some("CHILD\n"), true),
        ("blank.mds", None, false),
    ];
    for (key, body, mapped) in rows {
        let module = cache
            .resolve_key(key, &HashMap::new(), &mut vec![])
            .unwrap_or_else(|err| panic!("{key} must resolve: {err}"));
        assert_eq!(module.prompt_body.as_deref(), body, "{key}: evaluated body");
        assert_eq!(module.prompt_map.is_some(), mapped, "{key}: map kept");
    }
}

/// #416: a region whose module was first registered in the builder by an
/// `@include` splice — a source of the included module's fragment — names that
/// module's root-relative display path in a runtime diagnostic, never its canonical
/// key, and the error is identical with and without a builder.
#[test]
fn region_first_registered_by_a_splice_names_its_display_path() {
    let helper = arc_origin(
        "/abs/proj/g5/b.mds",
        "g5/b.mds",
        "@if n == 5:\nfive\n@end\n",
    );
    let module = arc_origin("/abs/proj/g5/m.mds", "g5/m.mds", "{{b.helper()}}\n");
    let root = arc_origin("/abs/proj/g5/a.mds", "g5/a.mds", "@block second:\n@end\n");

    // The included module's own builder registered the helper file through an S8
    // call, with that module's own Origin value of it.
    let mut module_builder = crate::sourcemap::MapBuilder::new(module);
    let helper_as_imported = arc_origin(&helper.file, &helper.display, &helper.source);
    module_builder.switch_to(&helper_as_imported);
    module_builder.push_segment(0, 0, 4);
    let fragment = Arc::new(module_builder.into_fragment());

    let mut importer = crate::sourcemap::MapBuilder::new(root);
    importer.splice_fragment(&fragment, 0);
    assert!(
        importer.sources().iter().any(|o| *o.file == *helper.file),
        "precondition: the splice registered the helper's key"
    );
    // Positive control (PF-013): the key is absolute, so the equality below fails if
    // the key reaches the diagnostic.
    assert!(helper.file.starts_with('/') && *helper.file != *helper.display);

    let nodes = parse_nodes(&helper.source);
    let regions: Vec<(&[Node], &Origin)> = vec![(&nodes, &helper)];
    let [off, on] = [None, Some(importer)].map(|map| {
        let mut scope = Scope::new();
        scope.set_var("n", Value::String("hi".to_string()));
        match ModuleCache::evaluate_regions_with_map(
            &regions,
            &mut scope,
            &mut vec![],
            map,
            &mut EvalBudget::default(),
        ) {
            Ok(_) => panic!("a cross-type comparison must fail"),
            Err(err) => err,
        }
    });
    assert_eq!(on.serialize().code, "mds::type_mismatch", "{on}");
    assert_eq!(
        on.source_name(),
        Some("g5/b.mds"),
        "the diagnostic must name the root-relative display, never the key"
    );
    assert_eq!(
        off.serialize(),
        on.serialize(),
        "the error must not depend on source maps"
    );
}

// ── #371: filesystem-root base dir ────────────────────────────────────────
//
// A base directory that IS the filesystem root (`/` on Unix, a drive root
// like `C:\` on Windows) must anchor resolution AT the root, never silently
// fall back to the current working directory. The root is obtained portably
// via a tempdir's topmost ancestor -- never a hardcoded "/" -- so these tests
// run unchanged on the Windows CI leg (#147).
//
// Before the #371 fix, NativeFs's base-directory resolution routed every path (including
// the root) through `check_symlink_named`, whose first step is
// `path.file_name()` -- `None` for a root path -- so ANY string
// compile/check/lint call whose base_dir resolved to the root failed with
// "cannot resolve path /: file not found: /" before a single syscall ran.

#[test]
fn compile_str_with_root_base_dir_no_imports_succeeds() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().ancestors().last().unwrap();
    let result = crate::compile_str_with("Hello world\n", Some(root), None);
    assert!(
        result.is_ok(),
        "compile_str_with with a root base_dir and no imports must succeed, got: {result:?}"
    );
}

#[test]
fn check_str_with_root_base_dir_no_imports_succeeds() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().ancestors().last().unwrap();
    let result = crate::check_str_with("Hello world\n", Some(root), None);
    assert!(
        result.is_ok(),
        "check_str_with with a root base_dir and no imports must succeed, got: {result:?}"
    );
}

#[test]
fn lint_str_with_root_base_dir_no_imports_succeeds() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().ancestors().last().unwrap();
    let result = crate::lint_str_with(
        "Hello world\n",
        Some(root),
        None,
        &crate::LintConfig::default(),
    );
    assert!(
        result.is_ok(),
        "lint_str_with with a root base_dir and no imports must succeed, got: {result:?}"
    );
}

#[test]
fn import_through_root_base_dir_resolves() {
    // A real @import, resolved through a base_dir that IS the filesystem
    // root, using a canonicalized tempdir path stripped down to a
    // root-relative import string -- the shape any real root-anchored
    // compile hits (e.g. `docker run` with no WORKDIR set, cwd == "/").
    //
    // Both `root` and `canonical_lib` are derived from `canonicalize()`
    // output (never a mix of canonical and non-canonical forms): on Windows,
    // `canonicalize()` returns a verbatim (`\\?\`) path, and stripping a
    // non-verbatim root from a verbatim file path would fail to find a
    // common prefix.
    let dir = tempfile::TempDir::new().unwrap();
    let canonical_dir = dir
        .path()
        .canonicalize()
        .expect("tempdir must canonicalize");
    let root = canonical_dir.ancestors().last().unwrap().to_path_buf();

    let lib_path = dir.path().join("lib.mds");
    std::fs::write(&lib_path, "@define greet(x):\nHello {{x}}!\n@end\n").unwrap();
    let canonical_lib = lib_path
        .canonicalize()
        .expect("temp file must canonicalize");

    let relative = canonical_lib
        .strip_prefix(&root)
        .expect("a filesystem root is a prefix of every absolute path under it")
        .to_str()
        .expect("temp path must be valid UTF-8")
        .replace(std::path::MAIN_SEPARATOR, "/");

    let source = format!("@import \"./{relative}\"\n{{{{greet(\"World\")}}}}\n");
    let output = crate::compile_str_with(&source, Some(&root), None)
        .expect("compile through a root base_dir with a real import must succeed")
        .into_markdown()
        .expect("markdown output expected");
    assert_eq!(output, "Hello World!\n");
}

// ── validate_import_path ──────────────────────────────────────────────────

/// The resolver's own import-string guard (it runs before any backend, so it covers
/// custom `with_fs` backends): an import must be `./`/`../`-relative and NUL-free.
#[test]
fn validate_import_path_requires_relative_nul_free_form() {
    for bad in ["lib.mds", "/abs/lib.mds", "", "sub/../lib.mds"] {
        let err = validate_import_path(bad).unwrap_err();
        assert!(
            matches!(err, MdsError::ImportError { .. }),
            "{bad:?}: expected ImportError, got {err:?}"
        );
        assert!(
            err.to_string().contains("import path must be relative"),
            "{bad:?}: got {err}"
        );
    }
    let err = validate_import_path("./a\0b.mds").unwrap_err();
    assert!(
        err.to_string().contains("import path contains null byte"),
        "got {err}"
    );
    // Controls: both relative forms pass.
    for ok in ["./lib.mds", "../lib.mds"] {
        assert!(validate_import_path(ok).is_ok(), "{ok:?} must be accepted");
    }
}

/// #265: the resolver's guard refuses all 80 forbidden codepoints (it is what covers
/// a custom `with_fs` backend), and `import_path_violation` names the rule each
/// path breaks — the classification the frontmatter `imports:` parser reports.
#[test]
fn validate_import_path_refuses_every_forbidden_char() {
    let chars: Vec<char> = (0..=0x10_FFFF_u32)
        .filter_map(char::from_u32)
        .filter(|&c| crate::lint::is_forbidden_path_char(c))
        .collect();
    assert_eq!(chars.len(), 80, "non-vacuity");
    for ch in chars {
        let path = format!("./a{ch}.mds");
        let u = format!("U+{:04X}", u32::from(ch));
        let err = validate_import_path(&path).unwrap_err();
        assert!(matches!(err, MdsError::ImportError { .. }), "{u}: {err:?}");
        let msg = err.to_string();
        if ch == '\0' {
            assert_eq!(
                import_path_violation(&path),
                Some(ImportPathViolation::NullByte)
            );
            assert!(msg.contains("import path contains null byte"), "{msg}");
        } else {
            assert_eq!(
                import_path_violation(&path),
                Some(ImportPathViolation::ForbiddenChar(ch))
            );
            let expected = format!(
                "import path contains forbidden character {u}: \"./a\\u{:04X}.mds\"",
                u32::from(ch)
            );
            assert!(msg.contains(&expected), "{u}: {msg}");
        }
        assert!(
            !msg.chars().any(crate::lint::is_forbidden_path_char),
            "{u}: raw char in {msg:?}"
        );
    }

    // The relative-form rule is checked first; its message escapes the path too.
    let hostile_bare = format!("lib{}.mds", '\x1b');
    assert_eq!(
        import_path_violation(&hostile_bare),
        Some(ImportPathViolation::NotRelative)
    );
    let msg = validate_import_path(&hostile_bare).unwrap_err().to_string();
    assert!(
        msg.contains(&format!(
            "must be relative (start with './' or '../'): \"lib\\u{:04X}.mds\"",
            0x1B
        )),
        "{msg}"
    );
    // Reasons, as the frontmatter parser prints them.
    assert_eq!(
        ImportPathViolation::NotRelative.reason(),
        "must start with './' or '../'"
    );
    assert_eq!(ImportPathViolation::NullByte.reason(), "contains null byte");
    assert_eq!(
        ImportPathViolation::ForbiddenChar('\u{202E}').reason(),
        "contains forbidden character U+202E"
    );
    // Control: a clean relative path has no violation.
    assert_eq!(import_path_violation("./a b-\u{00FC}.mds"), None);
}

/// A custom backend of the one kind that reaches the source-map root safety net:
/// absolute keys, no root concept (`source_root` stays `None`), and an
/// `anchor_base_dir` that refuses every directory and counts its calls.
struct UnanchorableFs {
    anchor_calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl FileSystem for UnanchorableFs {
    fn resolve_entry(&self, path: &str) -> Result<String, MdsError> {
        Ok(path.to_string())
    }
    fn normalize_in_dir(&self, dir: &str, relative: &str) -> Result<String, MdsError> {
        Ok(format!("{dir}/{}", relative.trim_start_matches("./")))
    }
    fn parent_dir(&self, key: &str) -> String {
        key.rsplit_once('/').map_or("", |(dir, _)| dir).to_string()
    }
    fn read(&self, key: &str) -> Result<String, MdsError> {
        match key {
            "/proj/main.mds" => Ok("Hello!\n".to_string()),
            "/proj/child.mds" => Ok("@extends \"./base.mds\"\n".to_string()),
            "/proj/base.mds" => Ok("Base\n@block body:\ndefault\n@end\n".to_string()),
            _ => Err(MdsError::module_not_found(key)),
        }
    }
    fn is_markdown(&self, _key: &str) -> bool {
        false
    }
    fn anchor_base_dir(&self, dir: &str) -> Result<String, MdsError> {
        self.anchor_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(MdsError::io(format!("refused: {dir}")))
    }
}

/// #155: the source-map root safety net is best-effort. A backend that refuses to
/// anchor the entry's directory there still compiles — it accepted the entry — and
/// its absolute `sources[]` keys degrade to basenames, never to absolute paths.
/// Standalone and `@extends` entries each reach their own call site.
#[test]
fn source_map_root_safety_net_never_fails_a_compile() {
    for (entry, source, output) in [
        ("/proj/main.mds", "main.mds", "Hello!\n"),
        ("/proj/child.mds", "base.mds", "Base\ndefault\n"),
    ] {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut cache = ModuleCache::with_fs(Box::new(UnanchorableFs {
            anchor_calls: Arc::clone(&calls),
        }));
        let opts = crate::CompileOptions::default().with_source_map(true);
        let (compiled, sm) = cache
            .resolve_virtual_intrinsic_opts(entry, &HashMap::new(), &opts, &mut vec![])
            .unwrap_or_else(|e| panic!("{entry}: a refused anchor must not fail the compile: {e}"));
        // Reachability (PF-013): the refusal below was actually returned.
        assert!(
            calls.load(std::sync::atomic::Ordering::SeqCst) > 0,
            "{entry}: the safety net must have called anchor_base_dir"
        );
        assert!(
            matches!(&compiled, crate::CompiledOutput::Markdown(s) if s == output),
            "{entry}: unexpected output {compiled:?}"
        );
        let sm = sm.expect("source map must be emitted");
        // Positive control (PF-013): the raw key IS absolute.
        assert!(
            entry.starts_with('/'),
            "positive control: raw key is absolute"
        );
        assert_eq!(
            sm.sources.first().map(String::as_str),
            Some(source),
            "{entry}"
        );
        for s in &sm.sources {
            assert!(
                !s.starts_with('/'),
                "{entry}: sources[] must never be absolute; got {s}"
            );
        }
    }
}

// ── #417: check_module_type ───────────────────────────────────────────────

/// `check_module_type` judges the extension on the resolved key and names the path
/// as typed, escaped (#417). The escape is defence in depth — every built-in route
/// refuses a forbidden character in the typed path first — so only this unit test
/// can reach it.
#[test]
fn check_module_type_judges_the_key_and_names_the_typed_path_escaped() {
    let check = |key: &str, typed: &str, source: &str| {
        check_module_type(ModuleRef { key, typed }, source).map_err(|e| e.to_string())
    };
    let bs = '\\';
    let hostile = format!("./a{}b\tc\u{202E}d.txt", '\x1b');
    assert_eq!(
        check("/abs/proj/doc.txt", &hostile, "Hi\n"),
        Err(format!(
            "not an MDS file: ./a{bs}u001Bb{bs}u0009c{bs}u202Ed.txt"
        )),
        "the path as typed, escaped — never the key"
    );
    // The extension is the key's, whatever the typed spelling says.
    assert_eq!(
        check("/abs/proj/doc.txt", "./doc.mds", ""),
        Err("not an MDS file: ./doc.mds".to_string())
    );
    assert_eq!(check("/abs/proj/doc.mds", "./Doc.MDS", ""), Ok(()));
    // Controls: an `.md` key passes only with `type: mds` in its frontmatter.
    assert_eq!(
        check(
            "/abs/proj/page.md",
            "./page.md",
            "---\ntype: mds\n---\nHi\n"
        ),
        Ok(())
    );
    assert_eq!(
        check("/abs/proj/page.md", "./page.md", "Hi\n"),
        Err("not an MDS file: ./page.md".to_string())
    );
}

// #423 directive coverage: import (alias/merge/selective), export-from
// (named/wildcard), and extends: span yes. Include does not load a file; its
// namespace import owns the span. Entry and frontmatter imports: span no.
fn assert_not_mds_directive_span(directive: &str) {
    let (_guard, root) = r3_project();
    let entry = root.join("a.mds");
    let importer = root.join("sub").join("b.mds");
    let source = format!("\n\n{directive}\n");
    std::fs::write(&entry, "@import \"./sub/b.mds\" as b\nEntry\n").unwrap();
    std::fs::write(root.join("sub").join("c.txt"), "@import \"./never.mds\"\n").unwrap();
    std::fs::write(
        root.join("sub").join("c.mds"),
        "@define f():\nOK\n@end\nBody\n",
    )
    .unwrap();

    // The same directive accepts an MDS target before it refuses c.txt.
    std::fs::write(&importer, source.replace("c.txt", "c.mds")).unwrap();
    crate::compile(&entry, None).expect("control: the directive reaches an MDS target");
    std::fs::write(&importer, &source).unwrap();
    assert!(importer.canonicalize().unwrap().is_absolute());

    let intrinsic = crate::compile(&entry, None).unwrap_err();
    let cached = ModuleCache::native()
        .resolve_path(entry.to_str().unwrap(), &HashMap::new(), &mut Vec::new())
        .unwrap_err();
    for err in [intrinsic, cached] {
        let serialized = err.serialize();
        assert_eq!(serialized.code, "mds::not_mds");
        assert_eq!(serialized.message, "not an MDS file: ./c.txt");
        let span = serialized
            .span
            .expect("the inner directive must carry a span");
        assert_eq!(
            (span.offset, span.length),
            (2, directive.lines().next().unwrap().len())
        );
        assert_eq!((span.line, span.column), (Some(3), Some(1)));
        assert_eq!(err.source_name(), Some("sub/b.mds"));
        assert!(!std::path::Path::new(err.source_name().unwrap()).is_absolute());
    }
}

#[test]
fn not_mds_alias_import_keeps_inner_importer_span() {
    assert_not_mds_directive_span("@import \"./c.txt\" as c");
}

#[test]
fn not_mds_merge_import_keeps_inner_importer_span() {
    assert_not_mds_directive_span("@import \"./c.txt\"");
}

#[test]
fn not_mds_selective_import_keeps_inner_importer_span() {
    assert_not_mds_directive_span("@import { f } from \"./c.txt\"");
}

#[test]
fn not_mds_named_reexport_keeps_inner_importer_span() {
    assert_not_mds_directive_span("@export f from \"./c.txt\"");
}

#[test]
fn not_mds_wildcard_reexport_keeps_inner_importer_span() {
    assert_not_mds_directive_span("@export * from \"./c.txt\"");
}

#[test]
fn not_mds_extends_keeps_inner_importer_span() {
    assert_not_mds_directive_span("@extends \"./c.txt\"");
}

#[test]
fn not_mds_intermediate_extends_keeps_the_base_importer_span() {
    let (_guard, root) = r3_project();
    let entry = root.join("a.mds");
    let importer = root.join("sub/c.mds");
    let directive = "@extends \"./d.txt\"";
    let source = format!("\n\n{directive}\n");
    std::fs::write(&entry, "@import \"./sub/b.mds\" as b\nEntry\n").unwrap();
    std::fs::write(root.join("sub/b.mds"), "@extends \"./c.mds\"\n").unwrap();
    std::fs::write(root.join("sub/d.txt"), "@import \"./never.mds\"\n").unwrap();
    std::fs::write(root.join("sub/d.mds"), "Body\n").unwrap();
    std::fs::write(&importer, source.replace("d.txt", "d.mds")).unwrap();
    crate::compile(&entry, None).expect("control: the intermediate base reaches an MDS target");
    std::fs::write(&importer, &source).unwrap();

    let intrinsic = crate::compile(&entry, None).unwrap_err();
    let cached = ModuleCache::native()
        .resolve_path(entry.to_str().unwrap(), &HashMap::new(), &mut Vec::new())
        .unwrap_err();
    for err in [intrinsic, cached] {
        let serialized = err.serialize();
        assert_eq!(serialized.code, "mds::not_mds");
        assert_eq!(serialized.message, "not an MDS file: ./d.txt");
        assert_eq!(err.source_name(), Some("sub/c.mds"));
        let span = serialized
            .span
            .expect("the intermediate extends must carry a span");
        assert_eq!((span.offset, span.length), (2, directive.len()));
        assert_eq!((span.line, span.column), (Some(3), Some(1)));
    }
}

#[test]
fn not_mds_include_uses_its_namespace_import_span() {
    assert_not_mds_directive_span("@import \"./c.txt\" as c\n@include c");
}

#[test]
fn not_mds_frame_neutralizes_control_on_import_line() {
    let source = "\n@import \x0b\"./c.txt\"\n";
    assert!(
        source.contains(char::from(0x0b)),
        "control: source has a control byte"
    );
    let err = compile_virtual(&[("a.mds", source), ("c.txt", "Hi\n")], "a.mds").unwrap_err();
    assert_eq!(err.serialize().code, "mds::not_mds");
    let mut frame = String::new();
    miette::NarratableReportHandler::new()
        .render_report(&mut frame, &err)
        .unwrap();
    assert!(frame.contains("@import ?\"./c.txt\""), "{frame}");
    assert!(!frame.contains(char::from(0x0b)), "{frame}");
}

#[test]
fn not_mds_nested_frontmatter_imports_stay_spanless_like_missing_files() {
    let (_guard, root) = r3_project();
    let entry = root.join("a.mds");
    let importer = root.join("sub").join("b.mds");
    std::fs::write(root.join("sub").join("c.txt"), "Text\n").unwrap();
    for directive in [
        "@import \"./sub/b.mds\" as b",
        "@import \"./sub/b.mds\"",
        "@import { f } from \"./sub/b.mds\"",
        "@export f from \"./sub/b.mds\"",
        "@export * from \"./sub/b.mds\"",
        "@extends \"./sub/b.mds\"",
    ] {
        std::fs::write(&entry, format!("\n\n{directive}\n")).unwrap();
        for form in ["", "    as: c\n", "    names: [f]\n"] {
            for (target, code) in [("missing.mds", "mds::import"), ("c.txt", "mds::not_mds")] {
                let source = format!("---\nimports:\n  - path: ./{target}\n{form}---\nBody\n");
                std::fs::write(&importer, source).unwrap();
                let intrinsic = crate::compile(&entry, None).unwrap_err();
                let cached = ModuleCache::native()
                    .resolve_path(entry.to_str().unwrap(), &HashMap::new(), &mut Vec::new())
                    .unwrap_err();
                for err in [intrinsic, cached] {
                    let serialized = err.serialize();
                    assert_eq!(serialized.code, code, "{directive}: {serialized:?}");
                    assert_eq!(serialized.span, None, "{directive}: {serialized:?}");
                    assert_eq!(err.source_name(), None, "{directive}: {serialized:?}");
                }
            }
        }
    }
}

#[test]
fn not_mds_entry_and_frontmatter_imports_keep_no_span() {
    let entry = compile_virtual(&[("c.txt", "Hi\n")], "c.txt").unwrap_err();
    for err in std::iter::once(entry).chain(["", "    as: c\n", "    names: [f]\n"].map(|form| {
        let source = format!("---\nimports:\n  - path: ./c.txt\n{form}---\nBody\n");
        compile_virtual(&[("a.mds", &source), ("c.txt", "Hi\n")], "a.mds").unwrap_err()
    })) {
        assert_eq!(err.serialize().code, "mds::not_mds");
        assert!(err.serialize().span.is_none());
        assert!(err.source_name().is_none());
    }
}

fn limit_cache_headroom(cache: &mut ModuleCache, headroom: usize) {
    let prior = MAX_COMPILE_MEMORY - headroom;
    cache.budget.mem.charge(Part::Buffers, Some(prior)).unwrap();
    cache.commit_retained(prior);
}

/// The tree estimate that a compile charges for `source`.
fn tree_of(source: &str) -> usize {
    tree_estimate(&tokenize(source, "tree.mds").unwrap(), source.len()).unwrap()
}

#[test]
fn cached_module_moves_its_text_tree_and_fragment_to_retained() {
    let key = "part.mds";
    let raw = "label: value\n";
    let body = "body\n";
    let source = format!("---\n{raw}---\n{body}");
    let mut cache = virtual_cache(&[(key, &source)]);
    cache.source_map_mode = true;
    let resolved = cache
        .resolve_key(key, &Default::default(), &mut vec![])
        .unwrap();
    assert_eq!(resolved.raw_frontmatter.as_deref(), Some(raw));
    assert_eq!(resolved.prompt_body.as_deref(), Some(body));
    assert_eq!(
        resolved.frontmatter_values.as_ref().unwrap()["label"],
        "value"
    );
    let vars = Arc::default();
    let ctx = extends_ctx(&source, &vars);
    let mut builder = MapBuilder::new(ctx.make_origin());
    builder.push_segment(0, 0, body.len() as u32);
    assert!(resolved.prompt_map.is_some());
    let origin_bytes = source.len() + 2 * key.len() + 6 * size_of::<usize>();
    // The entry keeps its parsed frontmatter tree and its syntax tree estimate. The
    // module defines no function, so it does not keep its scope layer.
    let tree = parse_frontmatter_yaml_with(raw, Allowance::in_compile(&MemBudget::default()))
        .unwrap()
        .1;
    assert_eq!(resolved.frontmatter_bytes, tree);
    assert_eq!(
        cache.retained,
        origin_bytes + raw.len() + body.len() + builder.segments_bytes() + tree + tree_of(&source)
    );
    assert_eq!(
        cache.budget.mem.headroom,
        MAX_COMPILE_MEMORY - cache.retained
    );
    assert_eq!(cache.budget.mem.buffers, 0);
}

#[test]
fn cached_modules_count_across_resolve_calls() {
    let files = [("one.mds", "one\n"), ("two.mds", "two\n")];
    let run =
        |cache: &mut ModuleCache, key| cache.resolve_key(key, &Default::default(), &mut vec![]);
    let cost = 2 * ("one\n".len() + "one.mds".len()) + 6 * size_of::<usize>() + tree_of("one\n");
    let mut reused = virtual_cache(&files);
    limit_cache_headroom(&mut reused, 2 * cost - 1);
    run(&mut reused, "one.mds").unwrap();
    let error = run(&mut reused, "two.mds").unwrap_err().serialize();
    assert_eq!(error.code, "mds::resource_limit");
    assert_eq!(
        error.message,
        "resource limit exceeded: compile memory exceeds maximum of 209715200 bytes"
    );
    assert!(error.span.is_none());
    assert!(error.help.is_none());
    assert_eq!(reused.dependencies(), ["one.mds"]);
    assert!(reused.resolving.is_empty());
    let mut fresh = virtual_cache(&files);
    limit_cache_headroom(&mut fresh, 2 * cost - 1);
    assert_eq!(
        run(&mut fresh, "two.mds").unwrap().prompt_body.as_deref(),
        Some("two\n")
    );
}

#[test]
fn fragment_refusal_keeps_prompt_and_uses_existing_warning() {
    let files = [("part.mds", "body\n")];
    let mut control = virtual_cache(&files);
    control.source_map_mode = true;
    let key = "part.mds";
    let control_part = control
        .resolve_key(key, &Default::default(), &mut vec![])
        .unwrap();
    assert!(control_part.prompt_map.is_some());

    let mut tight = virtual_cache(&files);
    tight.source_map_mode = true;
    // Leave enough for the read, the tree estimate and all retained text, but not
    // the fragment.
    let text = "body\n".len();
    limit_cache_headroom(
        &mut tight,
        3 * text + 2 * key.len() + 6 * size_of::<usize>() + tree_of("body\n"),
    );
    let mut warnings = vec![];
    let part = tight
        .resolve_key(key, &Default::default(), &mut warnings)
        .unwrap();
    assert_eq!(
        part.prompt_body.as_deref(),
        control_part.prompt_body.as_deref()
    );
    assert!(part.prompt_map.is_none());
    assert_eq!(warnings, ["source map segment cap (1000000 segments) exceeded in imported module 'part.mds'; text included from it is left unmapped in the source map"]);
    assert_eq!(tight.budget.mem.buffers, 0);
}

/// Each read of `PaddedReads` has this capacity. It is more than the tree estimate
/// and the output of a small string entry, so a refused read leaves room for one.
const PADDED_READ: usize = 2048;

struct PaddedReads(VirtualFs);

impl FileSystem for PaddedReads {
    fn resolve_entry(&self, path: &str) -> Result<String, MdsError> {
        self.0.resolve_entry(path)
    }

    fn normalize_in_dir(&self, dir: &str, relative: &str) -> Result<String, MdsError> {
        self.0.normalize_in_dir(dir, relative)
    }

    fn parent_dir(&self, key: &str) -> String {
        self.0.parent_dir(key)
    }

    fn read(&self, key: &str) -> Result<String, MdsError> {
        let source = self.0.read(key)?;
        let mut padded = String::with_capacity(PADDED_READ);
        padded.push_str(&source);
        Ok(padded)
    }

    fn is_markdown(&self, key: &str) -> bool {
        self.0.is_markdown(key)
    }
}

#[test]
fn module_reads_release_to_parent_and_inserts_keep_only_warnings() {
    let files = [
        (
            "main.mds",
            "@import \"./a.mds\" as a\n@import \"./b.mds\" as b\n@include b\n",
        ),
        (
            "a.mds",
            "@import \"./empty.mds\" as empty\n@include empty\n",
        ),
        ("b.mds", "body\n"),
        ("empty.mds", "   \n"),
    ];
    let fs = VirtualFs::new(
        files
            .map(|(key, src)| (key.to_owned(), src.to_owned()))
            .into(),
    );
    let mut cache = ModuleCache::with_fs(Box::new(PaddedReads(fs)));
    let mut warnings = vec![];
    let (output, _) = cache
        .resolve_virtual_intrinsic_opts(
            "main.mds",
            &Default::default(),
            &CompileOptions::default(),
            &mut warnings,
        )
        .unwrap();
    assert_eq!(output, crate::CompiledOutput::Markdown("body\n".to_owned()));
    assert_eq!(warnings.len(), 1);
    let [root, a, empty, b] = cache.module_probes.as_slice() else {
        panic!("four modules")
    };
    // The intrinsic entry holds its tree estimate as a temporary while its imports
    // resolve. A cached import keeps its tree, so `empty` sees only the read of `a`.
    let root_tree = tree_of(files[0].1);
    let read = PADDED_READ;
    for (probe, outer) in [
        (root, 0),
        (a, read + root_tree),
        (empty, 2 * read + root_tree),
        (b, read + root_tree),
    ] {
        assert_eq!(probe.temporaries_before, outer);
        assert_eq!(probe.temporaries_open, outer + read);
        assert_eq!(probe.temporaries_closed, outer);
    }
    assert_eq!(
        a.buffers_after_insert,
        Some(a.buffers_before + warnings[0].len())
    );
    for probe in [empty, b] {
        assert_eq!(probe.buffers_after_insert, Some(probe.buffers_before));
    }
    assert_eq!(cache.budget.mem.mark(), 0);
}

#[test]
fn refused_root_read_releases_the_module_mark() {
    let fs = VirtualFs::new(HashMap::from([("main.mds".to_owned(), "body".to_owned())]));
    let mut cache = ModuleCache::with_fs(Box::new(PaddedReads(fs)));
    limit_cache_headroom(&mut cache, PADDED_READ - 1);
    let vars = Default::default();
    let mut warnings = vec![];
    let error = cache
        .resolve_virtual_intrinsic("main.mds", &vars, &mut warnings)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "resource limit exceeded: compile memory exceeds maximum of 209715200 bytes"
    );
    assert!(cache.resolving.is_empty());
    assert_eq!(cache.budget.mem.mark(), 0);
    assert_eq!(
        cache
            .resolve_source_intrinsic("body", "", &vars, &mut warnings)
            .unwrap(),
        crate::CompiledOutput::Markdown("body\n".to_owned())
    );
}

#[test]
fn skeleton_upgrade_adds_to_the_old_retained_charge() {
    let files = [
        ("base.mds", "body\n"),
        ("child.mds", "@extends \"./base.mds\"\n"),
    ];
    let mut cache = virtual_cache(&files);
    let vars = Default::default();
    let mut warnings = vec![];
    cache
        .resolve_key("child.mds", &vars, &mut warnings)
        .unwrap();
    assert!(cache.modules["base.mds"].is_skeleton);
    let before = cache.retained;
    let upgraded = cache.resolve_key("base.mds", &vars, &mut warnings).unwrap();
    assert_eq!(upgraded.prompt_body.as_deref(), Some("body\n"));
    let mut fresh = virtual_cache(&files);
    fresh.resolve_key("base.mds", &vars, &mut warnings).unwrap();
    // The skeleton entry counts the tree of the base. The upgrade counts it once,
    // so it adds the charge of a fresh compile without that tree.
    let tree = tree_of(files[0].1);
    assert!(fresh.retained > tree);
    assert_eq!(cache.retained, before + fresh.retained - tree);
}

#[test]
fn finalize_refusal_keeps_output_and_releases_map_work() {
    let mut cache = virtual_cache(&[]);
    let source = "body\n";
    let opts = CompileOptions::default()
        .with_source_map(true)
        .with_include_sources_content(true);
    let compile = |cache: &mut ModuleCache, opts: &CompileOptions, warnings: &mut Vec<String>| {
        cache
            .resolve_source_intrinsic_opts(source, "", &Default::default(), opts, warnings)
            .unwrap()
    };
    let (_, control_map) = compile(&mut cache, &opts, &mut vec![]);
    assert_eq!(
        control_map.unwrap().sources_content,
        Some(vec![source.to_owned()])
    );
    let (plain, _) = compile(&mut cache, &CompileOptions::default(), &mut vec![]);
    // Enough to charge the tree estimate, the work arrays and body, but not the
    // retained map.
    limit_cache_headroom(&mut cache, 230 + tree_of(source));
    let mut warnings = vec![];
    let (tight, dropped) = compile(&mut cache, &opts, &mut warnings);
    assert_eq!(tight, plain);
    assert!(dropped.is_none());
    assert_eq!(
        warnings,
        ["source map segment cap (1000000 segments) exceeded; no source map will be generated"]
    );
    assert_eq!(cache.budget.mem.mark(), 0);
    assert_eq!(cache.budget.mem.buffers, source.len());

    // The failed map reservation must restore the parent's live bytes immediately.
    let vars = Arc::default();
    let ctx = extends_ctx(source, &vars);
    let mut builder = MapBuilder::new(ctx.make_origin());
    builder.push_segment(0, 0, source.len() as u32);
    let mem = &mut cache.budget.mem;
    // In a compile, the entry holds its tree estimate as a temporary at finalize.
    mem.charge(Part::Temp, Some(3 + tree_of(source))).unwrap();
    let before = *mem;
    let costs = FinalizeCosts {
        content: source.len(),
        map_origin_bytes: ctx.origin_bytes(),
        prefix_lines: 0,
        label_base_bytes: 0,
    };
    assert!(!charge_finalize(mem, &builder, source, costs));
    assert_eq!(*mem, before);
}

#[test]
fn final_output_charges_prefix_and_growth_before_the_reserve() {
    let cases = [
        "body".to_owned(),
        "body".to_owned() + &" ".repeat(50),
        " ".repeat(50),
    ];
    let raw = "label: value\n";
    // The parsed frontmatter and its scope layer stay charged while the module runs.
    let spent = parse_frontmatter_yaml_with(raw, Allowance::in_compile(&MemBudget::default()))
        .unwrap()
        .1;
    for body in cases {
        let growth = raw.len() + 8 + usize::from(!body.trim_end().is_empty());
        let source = format!("---\n{raw}---\n{body}");
        // The entry holds its tree estimate as a temporary for the whole compile.
        let tree = tree_of(&source);
        let needed = body.len() + 2 * raw.len() + 9 + growth + 2 * spent + tree;
        let compile = |headroom| {
            let mut cache = virtual_cache(&[]);
            limit_cache_headroom(&mut cache, headroom);
            cache
                .resolve_source_intrinsic_opts(
                    &source,
                    "",
                    &Default::default(),
                    &Default::default(),
                    &mut vec![],
                )
                .map(|output| (output, cache.budget.mem))
        };
        let (output, mem) = compile(needed).unwrap();
        let crate::CompiledOutput::Markdown(output) = output.0 else {
            panic!("markdown output")
        };
        assert_eq!(
            output,
            format!(
                "---\n{raw}---\n{}",
                if body.trim_end().is_empty() {
                    ""
                } else {
                    "body\n"
                }
            )
        );
        assert_eq!(mem.buffers, output.capacity());
        assert_eq!(mem.mark(), 0);
        assert_eq!(
            compile(needed - 1).unwrap_err().to_string(),
            "resource limit exceeded: compile memory exceeds maximum of 209715200 bytes"
        );
        assert_eq!(
            compile(body.len() + 2 * raw.len() + 8 + tree)
                .unwrap_err()
                .to_string(),
            "resource limit exceeded: compile memory exceeds maximum of 209715200 bytes"
        );
    }
}

#[test]
fn only_commit_retained_updates_cache_headroom() {
    fn writes(source: &str, field: &str) -> usize {
        source
            .match_indices(field)
            .filter(|(pos, _)| {
                let tail = &source[pos + field.len()..];
                let first = tail.as_bytes().first().copied();
                (tail.starts_with('=') && !tail.starts_with("=="))
                    || (first.is_some_and(|byte| b"+-*/%&|^".contains(&byte))
                        && tail.as_bytes().get(1) == Some(&b'='))
            })
            .count()
    }

    fn code(source: &str) -> String {
        source
            .lines()
            .flat_map(|line| {
                line.split_once("//")
                    .map_or(line, |(code, _)| code)
                    .split_whitespace()
            })
            .collect()
    }

    fn valid(resolver: &str, evaluator: &str) -> bool {
        let resolver = code(resolver);
        let evaluator = code(evaluator);
        let Some((_, commit)) = resolver.split_once("fncommit_retained(") else {
            return false;
        };
        let body = commit.split("}fn").next().unwrap_or_default();
        [".retained", ".headroom"].iter().all(|field| {
            writes(&resolver, field) == 1
                && writes(body, field) == 1
                && writes(&evaluator, field) == 0
        }) && !evaluator.contains("ModuleCache")
            && !evaluator.contains(".resolve_")
    }

    let resolver = include_str!("resolver.rs");
    let evaluator = concat!(
        include_str!("evaluator.rs"),
        include_str!("evaluator/budget.rs")
    );
    assert!(valid(resolver, evaluator));
    assert!(!valid(
        &format!("{resolver}\nself.retained = 0;"),
        evaluator
    ));
    assert!(!valid(&format!("{resolver}\nmem.headroom = 0;"), evaluator));
    assert!(!valid(
        resolver,
        &format!("{evaluator}\nmem.headroom -= 1;")
    ));
    assert!(!valid(
        resolver,
        &format!("{evaluator}\nuse crate::resolver::ModuleCache;")
    ));
    assert!(!valid(
        resolver,
        &format!("{evaluator}\ncache.resolve_by_key(key);")
    ));
}

pub(super) const TEMPORARIES_SEED_KEY: &str = "root-with-live-temporaries.mds";

fn assert_internal(error: MdsError) {
    let record = error.serialize();
    assert_eq!(record.code, "mds::internal");
    assert_eq!(record.message, "internal compiler error");
    assert_eq!(record.help, None);
    assert_eq!(record.span, None);
    assert_eq!(error.source_name(), None);
}

#[test]
fn resolve_source_refuses_to_return_output_with_live_temporaries() {
    let mut cache = virtual_cache(&[]);
    cache.temporaries_seed = 1;
    let result = cache.resolve_source("Body", "", &HashMap::new(), &mut vec![]);
    assert_eq!(cache.budget.mem.mark(), 1);
    assert_internal(result.unwrap_err());
}

#[test]
fn compile_virtual_refuses_to_return_output_with_live_temporaries() {
    let modules = HashMap::from([(TEMPORARIES_SEED_KEY.into(), "Body".into())]);
    assert_internal(crate::compile_virtual(modules, TEMPORARIES_SEED_KEY, None).unwrap_err());
}

#[test]
fn finish_preserves_the_first_error_even_with_live_temporaries() {
    let mut cache = virtual_cache(&[]);
    let mut resolve = |seed| {
        cache.temporaries_seed = seed;
        cache
            .resolve_source("{{missing}}", "", &HashMap::new(), &mut vec![])
            .unwrap_err()
            .serialize()
    };
    let original = resolve(0);
    assert_eq!(original.code, "mds::undefined_var");
    assert_eq!(resolve(1), original);
    assert_eq!(cache.budget.mem.mark(), 1);
}

#[test]
fn resolve_key_does_not_cache_output_when_temporaries_remain() {
    let key = "part.mds";
    let mut cache = virtual_cache(&[(key, "Body")]);
    cache.temporaries_seed = 1;
    assert_internal(
        cache
            .resolve_key(key, &HashMap::new(), &mut vec![])
            .unwrap_err(),
    );
    assert!(cache.dependencies().is_empty());
    cache.temporaries_seed = 0;
    assert!(cache.resolve_key(key, &HashMap::new(), &mut vec![]).is_ok());
}

#[test]
fn resolve_key_refuses_an_incomplete_retained_transfer() {
    let key = "part.mds";
    let mut cache = virtual_cache(&[(key, "Body")]);
    cache.retained_transfer_seed = 1;
    let result = cache.resolve_key(key, &HashMap::new(), &mut vec![]);
    assert_eq!(cache.budget.mem.mark(), 0);
    assert_eq!(cache.retained, 0);
    assert!(cache.budget.mem.buffers > 0);
    assert_internal(result.unwrap_err());
    assert!(cache.dependencies().is_empty());
    cache.retained_transfer_seed = 0;
    cache
        .resolve_key(key, &HashMap::new(), &mut vec![])
        .unwrap();
    assert!(cache.retained > 0);
}

#[test]
fn resolution_funnel_has_one_pop_and_five_finish_calls() {
    fn valid(source: &str) -> bool {
        let body = |name| {
            let (_, rest) = source.split_once(&format!("fn {name}("))?;
            let (body, _) = rest.split_once("\n    }")?;
            Some(body)
        };
        let Some(pop) = body("pop_resolving") else {
            return false;
        };
        let bodies = [Some(pop), body("finish_root"), body("commit_retained")];
        let code = |text: &str| {
            text.lines()
                .flat_map(|line| {
                    line.split("//")
                        .next()
                        .unwrap_or_default()
                        .split_whitespace()
                })
                .collect::<String>()
        };
        let all = code(source);
        all.matches(".resolving.pop(").count() == 1
            && code(pop).contains(".resolving.pop(")
            && all.matches(".finish_root(").count() == 5
            && all.matches("fnfinish_root(").count() == 1
            && bodies.into_iter().all(|body| {
                body.is_some_and(|body| {
                    body.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
                        .all(|token| {
                            !matches!(token, "expect" | "unwrap" | "panic" | "RefCell")
                                && !token.starts_with("assert")
                                && !token.starts_with("debug_assert")
                        })
                })
            })
    }

    let source = include_str!("resolver.rs");
    assert!(valid(source));
    assert!(!valid(&format!("{source}\nself.resolving . pop ();")));
    assert!(!valid(&format!("{source}\nself.finish_root()?;")));
    let with_assert = source.replacen(
        "self.resolving.pop()",
        "{ assert!(false); self.resolving.pop() }",
        1,
    );
    assert!(!valid(&with_assert));
}

/// Parse `yaml` as a compile parse does. Returns the mapping and its spent bytes.
fn parsed_mapping(yaml: &str) -> (serde_yaml_ng::Mapping, usize) {
    let (tree, spent) =
        parse_frontmatter_yaml_with(yaml, Allowance::in_compile(&MemBudget::default())).unwrap();
    let serde_yaml_ng::Value::Mapping(mapping) = tree else {
        panic!("expected a mapping");
    };
    (mapping, spent)
}

#[test]
fn merged_frontmatter_text_matches_the_text_of_a_cloned_value() {
    let (mapping, spent) = parsed_mapping(concat!(
        "name: \"a: b\"\n",
        "nested:\n  list: [1, two, \"3\", ~]\n  text: \"line one\\nline two\"\n",
        "quote: \"it's\"\n",
        "control: \"\\x01\\t\\u0085\\u2028\"\n",
    ));
    let cloned = serde_yaml_ng::to_string(&serde_yaml_ng::Value::Mapping(mapping.clone())).unwrap();
    let mut cache = virtual_cache(&[]);
    let text = cache
        .serialize_merged_frontmatter(&mapping, spent)
        .unwrap()
        .unwrap();
    assert_eq!(text, format!("{}\n", cloned.trim_end()));
    // The text is kept at its capacity, and no temporary remains.
    assert_eq!(cache.budget.mem.buffers, text.capacity());
    assert_eq!(cache.kept, text.capacity());
    assert_eq!(cache.budget.mem.mark(), 0);
}

#[test]
fn a_control_character_scalar_serializes_within_its_charge() {
    // Each control byte serializes as a four-byte escape.
    let (mapping, spent) = parsed_mapping(&format!("k: \"{}\"\n", "\\x01".repeat(100_000)));
    let mut cache = virtual_cache(&[]);
    let text = cache
        .serialize_merged_frontmatter(&mapping, spent)
        .unwrap()
        .unwrap();
    assert!(text.len() > 3 * spent, "{} B", text.len());
    // The bound sized the text, so the text did not grow past it.
    assert_eq!(text.capacity(), 4 * spent + 1);
}

#[test]
fn a_frontmatter_merge_charges_both_trees_first() {
    let mut cache = virtual_cache(&[("base.mds", "---\na: 1\n---\nBase\n")]);
    let base = cache
        .resolve_key("base.mds", &HashMap::new(), &mut vec![])
        .unwrap();
    let (child, child_bytes) = parsed_mapping("b: 2\n");
    let child = Some(child);
    let charge = base.frontmatter_bytes + child_bytes;
    let (merged, charged) = cache.merge_frontmatter(&base, &child, child_bytes).unwrap();
    assert_eq!((merged.len(), charged), (2, charge));
    assert_eq!(cache.budget.mem.mark(), charge);
    // With one byte less left than both trees take, the merge is refused.
    let left = cache.budget.mem.headroom - cache.budget.mem.buffers - charge;
    cache
        .budget
        .mem
        .charge(Part::Buffers, Some(left - charge + 1))
        .unwrap();
    assert_eq!(
        cache
            .merge_frontmatter(&base, &child, child_bytes)
            .unwrap_err()
            .to_string(),
        "resource limit exceeded: compile memory exceeds maximum of 209715200 bytes"
    );
}

#[test]
fn every_frontmatter_merge_goes_through_the_charged_helper() {
    fn calls(code: &str) -> usize {
        code.lines()
            .map(|line| {
                line.split("//")
                    .next()
                    .unwrap_or_default()
                    .matches("deep_merge_yaml(")
                    .count()
            })
            .sum()
    }
    fn valid(resolver: &str, frontmatter: &str) -> bool {
        let helper = resolver
            .split_once("fn merge_frontmatter(")
            .and_then(|(_, rest)| rest.split_once("\n    }"));
        // Outside its tests, frontmatter.rs holds the definition and its recursion.
        let production = frontmatter.split_once("\n#[cfg(test)]\nmod tests {");
        calls(resolver) == 1
            && helper.is_some_and(|(body, _)| calls(body) == 1)
            && production.is_some_and(|(code, _)| calls(code) == 2)
    }

    let resolver = include_str!("resolver.rs");
    let frontmatter = include_str!("resolver/frontmatter.rs");
    assert!(valid(resolver, frontmatter));
    let planted = "let merged = deep_merge_yaml(&base, &child, 0)?;";
    assert!(!valid(&format!("{resolver}\n{planted}"), frontmatter));
    assert!(!valid(resolver, &format!("{planted}\n{frontmatter}")));
}

#[test]
fn the_extends_scope_layer_is_charged_with_the_merge_charge() {
    let mut cache = virtual_cache(&[("base.mds", "---\na: 1\n---\nBase\n")]);
    let base = cache
        .resolve_key("base.mds", &HashMap::new(), &mut vec![])
        .unwrap();
    let (child, child_bytes) = parsed_mapping("b: 2\n");
    let vars = Arc::default();
    let ctx = extends_ctx("", &vars);
    let (scope, merged, charge) = cache
        .build_merged_extends_scope(
            &base,
            &Some(child),
            child_bytes,
            "base.mds",
            &ctx,
            &mut vec![],
        )
        .unwrap();
    assert_eq!(merged.len(), 2);
    assert!(scope.get_slot("b").is_some());
    // The merged mapping and the scope layer each hold the merge charge.
    assert_eq!(charge, base.frontmatter_bytes + child_bytes);
    assert_eq!(cache.budget.mem.mark(), 2 * charge);
}

/// The tree estimate assumes these type sizes on a 64-bit host. `tests/memory_bound.rs`
/// counts tokens and expression bytes by the rules that the samples check. A type that
/// grows, or a lexer that counts in a different way, fails this test, so the estimate
/// factors are measured again.
#[test]
fn tree_estimate_inputs_keep_their_measured_sizes() {
    use crate::ast::{Arg, Condition, ElseifBranch, Expr};
    use crate::lexer::Token;
    let sizes = [
        ("Node", size_of::<Node>(), 256),
        ("Token", size_of::<Token>(), 40),
        ("Expr", size_of::<Expr>(), 72),
        ("Condition", size_of::<Condition>(), 152),
        ("Arg", size_of::<Arg>(), 56),
        ("ElseifBranch", size_of::<ElseifBranch>(), 184),
    ];
    for (name, size, _) in sizes {
        eprintln!("size_of::<{name}>() = {size} B");
    }
    if size_of::<usize>() == 8 {
        for (name, size, measured) in sizes {
            assert!(
                size <= measured,
                "{name} is {size} B; the estimate assumes {measured} B"
            );
        }
    }

    // (source, tokens, expression bytes): directive lines without their newline and
    // trimmed interpolation text are expression bytes. Fence lines and raw content
    // runs count as tokens but do not add expression bytes.
    let samples = [
        ("\\{{a\\{{a", 4, 0),
        ("@define f():\n\\{{a\\{{a\n@end\n", 6, 16),
        ("---\na: ''\n---\n{{a}}{{ a }}", 5, 2),
        ("x\n@if a&&a:\n@end\n", 3, 13),
        ("{{f(f(a))}}text{{a.a}}", 3, 10),
        ("a@b\n@c", 2, 2),
        ("```\n{{a}}\n```\n``` +mds\n @if a:\n@end\n```\n", 7, 11),
        ("---\nindented-directives: true\n---\n\t@if a:", 4, 7),
    ];
    for (source, tokens, expr) in samples {
        let lexed = tokenize(source, "sample.mds").unwrap();
        let expr_bytes: usize = lexed
            .iter()
            .map(|token| match token {
                Token::Interpolation(text, _) | Token::Directive(text, _) => text.len(),
                _ => 0,
            })
            .sum();
        assert_eq!((lexed.len(), expr_bytes), (tokens, expr), "{source:?}");
        // The compile charges these counts with the factors from limits.rs.
        let len = source.len();
        assert_eq!(
            tree_estimate(&lexed, len),
            Some(TREE_PER_TOKEN * tokens + TREE_PER_EXPR_BYTE * expr + TREE_PER_TEXT_BYTE * len),
            "{source:?}"
        );
    }
}

const COMPILE_MEMORY_REFUSAL: &str =
    "resource limit exceeded: compile memory exceeds maximum of 209715200 bytes";

/// Dense syntax: each four bytes `\{{a` are two tokens. The tree estimate of 2^17
/// repeats (512 KiB of source) is past the compile memory budget.
fn dense_past_the_budget() -> String {
    "\\{{a".repeat(1 << 17)
}

/// About 1 MiB of prose in lines of 80 bytes: one text token, so the tree estimate
/// is mostly the text term. A prose source can not pass the budget alone, so the
/// prose cases first fill the budget to `PROSE_HEADROOM`.
fn prose_mib() -> String {
    format!("{}\n", "a".repeat(79)).repeat((1 << 20) / 80)
}

/// More than the compile of `prose_mib` needs without its tree, and less than its
/// tree estimate.
const PROSE_HEADROOM: usize = 12 << 20;

/// The cases that did not fail with the compile memory error, each with its result.
/// Every case runs before the caller asserts, so a failure names all of them.
fn missed_memory_refusals(cases: Vec<(String, Result<(), MdsError>)>) -> Vec<String> {
    cases
        .into_iter()
        .filter_map(|(what, result)| match result {
            Ok(()) => Some(format!("{what}: the compile passed")),
            Err(error) => {
                let record = error.serialize();
                (record.code != "mds::resource_limit" || record.message != COMPILE_MEMORY_REFUSAL)
                    .then(|| format!("{what}: {}: {}", record.code, record.message))
            }
        })
        .collect()
}

fn assert_memory_refusal(result: Result<(), MdsError>, what: &str) {
    let misses = missed_memory_refusals(vec![(what.to_owned(), result)]);
    assert!(misses.is_empty(), "{misses:?}");
}

#[test]
fn a_cached_import_whose_tree_estimate_passes_the_budget_is_refused() {
    let root = "@import \"./part.mds\" as part\nroot\n";
    let dense = dense_past_the_budget();
    let prose = prose_mib();
    let mut cases = vec![];
    for (what, part, headroom) in [
        ("dense import", &dense, MAX_COMPILE_MEMORY),
        ("prose import", &prose, PROSE_HEADROOM),
    ] {
        let mut cache = virtual_cache(&[("main.mds", root), ("part.mds", part)]);
        limit_cache_headroom(&mut cache, headroom);
        let result = cache.resolve_key("main.mds", &HashMap::new(), &mut vec![]);
        assert!(cache.resolving.is_empty(), "{what}");
        assert_eq!(cache.budget.mem.mark(), 0, "{what}");
        cases.push((what.to_owned(), result.map(drop)));
    }
    let misses = missed_memory_refusals(cases);
    assert!(misses.is_empty(), "{misses:#?}");
}

type Entry = fn(&mut ModuleCache, &str) -> Result<(), MdsError>;

/// The resolve functions of each entry kind. The entry is `main.mds`, or the string
/// `source`; `child.mds` extends `main.mds`.
const ENTRY_KINDS: [(&str, Entry); 6] = [
    ("source intrinsic", |cache, source| {
        cache
            .resolve_source_intrinsic(source, "", &HashMap::new(), &mut vec![])
            .map(drop)
    }),
    ("source intrinsic opts", |cache, source| {
        let opts = CompileOptions::default();
        cache
            .resolve_source_intrinsic_opts(source, "", &HashMap::new(), &opts, &mut vec![])
            .map(drop)
    }),
    ("resolve_source", |cache, source| {
        cache
            .resolve_source(source, "", &HashMap::new(), &mut vec![])
            .map(drop)
    }),
    ("virtual intrinsic", |cache, _| {
        cache
            .resolve_virtual_intrinsic("main.mds", &HashMap::new(), &mut vec![])
            .map(drop)
    }),
    ("resolve_key", |cache, _| {
        cache
            .resolve_key("main.mds", &HashMap::new(), &mut vec![])
            .map(drop)
    }),
    ("extends base", |cache, _| {
        cache
            .resolve_key("child.mds", &HashMap::new(), &mut vec![])
            .map(drop)
    }),
];

const EXTENDS_MAIN: &str = "@extends \"./main.mds\"\n";

#[test]
fn every_entry_kind_refuses_a_tree_estimate_past_the_budget() {
    let dense = dense_past_the_budget();
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("main.mds");
    std::fs::write(&path, &dense).unwrap();
    let modules = || {
        HashMap::from([
            ("main.mds".to_owned(), dense.clone()),
            ("child.mds".to_owned(), EXTENDS_MAIN.to_owned()),
        ])
    };
    let public = [
        ("compile_str", crate::compile_str(&dense).map(drop)),
        (
            "compile_str_with_deps_opts",
            crate::compile_str_with_deps_opts(&dense, None, None, CompileOptions::default())
                .map(drop),
        ),
        ("file entry", crate::compile(&path, None).map(drop)),
        (
            "virtual entry",
            crate::compile_virtual(modules(), "main.mds", None).map(drop),
        ),
        (
            "virtual extends base",
            crate::compile_virtual(modules(), "child.mds", None).map(drop),
        ),
    ];
    let mut cases: Vec<_> = public
        .into_iter()
        .map(|(what, result)| (what.to_owned(), result))
        .collect();
    let prose = prose_mib();
    for (kind, source, headroom) in [
        ("dense", &dense, MAX_COMPILE_MEMORY),
        ("prose", &prose, PROSE_HEADROOM),
    ] {
        for (what, entry) in ENTRY_KINDS {
            let mut cache = virtual_cache(&[("main.mds", source), ("child.mds", EXTENDS_MAIN)]);
            limit_cache_headroom(&mut cache, headroom);
            let result = entry(&mut cache, source);
            assert!(cache.resolving.is_empty(), "{kind} {what}");
            assert_eq!(cache.budget.mem.mark(), 0, "{kind} {what}");
            cases.push((format!("{kind} {what}"), result));
        }
    }
    let misses = missed_memory_refusals(cases);
    assert!(misses.is_empty(), "{misses:#?}");
}

/// The least headroom with which `entry` compiles `source`.
fn least_headroom(files: &[(&str, &str)], source: &str, entry: Entry) -> usize {
    least_passing_headroom(|headroom| {
        let mut cache = virtual_cache(files);
        limit_cache_headroom(&mut cache, headroom);
        entry(&mut cache, source)
    })
}

/// The least headroom with which `run` passes. The search keeps a refused low end
/// and a passing high end, so it ends at an at-cap pair.
fn least_passing_headroom(run: impl Fn(usize) -> Result<(), MdsError>) -> usize {
    let (mut refused, mut passed) = (0, MAX_COMPILE_MEMORY);
    assert!(run(passed).is_ok(), "the full budget compiles");
    for _ in 0..usize::BITS {
        if passed - refused <= 1 {
            break;
        }
        let mid = refused + (passed - refused) / 2;
        match run(mid) {
            Ok(()) => passed = mid,
            Err(_) => refused = mid,
        }
    }
    assert_eq!(passed - refused, 1);
    assert!(run(passed).is_ok());
    assert_memory_refusal(run(refused), "one byte under the least headroom");
    passed
}

#[test]
fn each_entry_kind_compiles_at_the_cap_that_includes_the_tree_estimate() {
    let dense = "\\{{a".repeat((1 << 10) + 1);
    let prose = format!("{}\n", "a".repeat(79)).repeat((1 << 16) / 80 + 1);
    let cached_import: (&str, Entry) = ("cached import", |cache, _| {
        cache
            .resolve_key("root.mds", &HashMap::new(), &mut vec![])
            .map(drop)
    });
    for source in [&dense, &prose] {
        let files = [
            ("main.mds", source.as_str()),
            ("child.mds", EXTENDS_MAIN),
            ("root.mds", "@import \"./main.mds\" as main\n"),
        ];
        let tree = tree_of(source);
        for (what, entry) in ENTRY_KINDS.into_iter().chain([cached_import]) {
            let least = least_headroom(&files, source, entry);
            eprintln!(
                "{what}: source={} B, tree estimate={tree} B, least headroom={least} B",
                source.len()
            );
            assert!(least > tree, "{what}: {least} B, tree estimate {tree} B");
        }
    }
}

#[test]
fn tree_estimate_counts_past_the_address_space_fail_closed() {
    let wrapping = [
        (usize::MAX / TREE_PER_TOKEN + 1, 0, 0),
        (0, usize::MAX / TREE_PER_EXPR_BYTE + 1, 0),
        (0, 0, usize::MAX / TREE_PER_TEXT_BYTE + 1),
        // Each product fits, and their sum does not.
        (
            usize::MAX / TREE_PER_TOKEN,
            0,
            usize::MAX / TREE_PER_TEXT_BYTE,
        ),
        (
            usize::MAX / TREE_PER_TOKEN,
            usize::MAX / TREE_PER_EXPR_BYTE,
            0,
        ),
    ];
    for (tokens, expr, len) in wrapping {
        let estimate = tree_bytes(tokens, expr, len);
        assert_eq!(estimate, None, "({tokens}, {expr}, {len})");
        let error = MemBudget::default()
            .charge(Part::Temp, estimate)
            .unwrap_err();
        assert_eq!(
            error.serialize(),
            MdsError::compile_memory_exceeded().serialize()
        );
    }
    // The largest counts that fit give the exact product.
    let most = usize::MAX / TREE_PER_TOKEN;
    assert_eq!(tree_bytes(most, 0, 0), Some(most * TREE_PER_TOKEN));
    assert_eq!(
        tree_bytes(1, 1, 1),
        Some(TREE_PER_TOKEN + TREE_PER_EXPR_BYTE + TREE_PER_TEXT_BYTE)
    );
}

#[test]
fn each_entry_kind_holds_the_tree_estimate_in_its_own_budget_part() {
    // "main.mds" and the string label "<source>" have the same length, so the two
    // entries keep the same origin bytes.
    assert_eq!("main.mds".len(), SOURCE_LABEL.len());
    let part = "part\n";
    let root = "@import \"./part.mds\" as part\nroot\n";
    let files = [("main.mds", root), ("part.mds", part)];
    let part_kept = kept_bytes(part, "part.mds", part);
    let root_kept = kept_bytes(root, "main.mds", "root\n");
    let vars = HashMap::new();

    // The intrinsic entry holds its tree as a temporary while its import resolves.
    let mut cache = virtual_cache(&files);
    cache
        .resolve_source_intrinsic(root, "", &vars, &mut vec![])
        .unwrap();
    let [entry, import] = cache.module_probes.as_slice() else {
        panic!("two modules")
    };
    assert_eq!(import.temporaries_before, tree_of(root));
    assert_eq!(entry.temporaries_closed, 0);
    assert_eq!(cache.budget.mem.mark(), 0);
    assert_eq!(cache.retained, part_kept);

    // The string entry keeps its tree in buffers, never in the retained part.
    let mut cache = virtual_cache(&files);
    cache.resolve_source(root, "", &vars, &mut vec![]).unwrap();
    assert_eq!(cache.retained, part_kept);
    assert_eq!(cache.budget.mem.buffers, root_kept);

    // A key entry and its cached import add their trees to the retained part.
    let mut cache = virtual_cache(&files);
    cache.resolve_key("main.mds", &vars, &mut vec![]).unwrap();
    assert_eq!(cache.retained, part_kept + root_kept);
    assert_eq!(cache.budget.mem.buffers, 0);
}

/// The bytes that a cached module of `source` keeps: its source, two copies of its
/// key, its origin counts, its prompt `body` and its tree estimate.
fn kept_bytes(source: &str, key: &str, body: &str) -> usize {
    source.len() + 2 * key.len() + 6 * size_of::<usize>() + body.len() + tree_of(source)
}

type VarsEntry = fn(&mut ModuleCache, &str, &HashMap<String, Value>) -> Result<(), MdsError>;

/// The nine resolve functions that copy the caller variables. Each compiles
/// `main.mds`, or the string `source`.
const VARS_ENTRIES: [(&str, VarsEntry); 9] = [
    ("resolve_path", |cache, _, vars| {
        cache.resolve_path("main.mds", vars, &mut vec![]).map(drop)
    }),
    ("resolve_path_intrinsic", |cache, _, vars| {
        cache
            .resolve_path_intrinsic("main.mds", vars, &mut vec![])
            .map(drop)
    }),
    ("resolve_path_intrinsic_opts", |cache, _, vars| {
        let opts = CompileOptions::default();
        cache
            .resolve_path_intrinsic_opts("main.mds", vars, &opts, &mut vec![])
            .map(drop)
    }),
    ("resolve_key", |cache, _, vars| {
        cache.resolve_key("main.mds", vars, &mut vec![]).map(drop)
    }),
    ("resolve_source", |cache, source, vars| {
        cache
            .resolve_source(source, "", vars, &mut vec![])
            .map(drop)
    }),
    ("resolve_virtual_intrinsic", |cache, _, vars| {
        cache
            .resolve_virtual_intrinsic("main.mds", vars, &mut vec![])
            .map(drop)
    }),
    ("resolve_virtual_intrinsic_opts", |cache, _, vars| {
        let opts = CompileOptions::default();
        cache
            .resolve_virtual_intrinsic_opts("main.mds", vars, &opts, &mut vec![])
            .map(drop)
    }),
    ("resolve_source_intrinsic", |cache, source, vars| {
        cache
            .resolve_source_intrinsic(source, "", vars, &mut vec![])
            .map(drop)
    }),
    ("resolve_source_intrinsic_opts", |cache, source, vars| {
        let opts = CompileOptions::default();
        cache
            .resolve_source_intrinsic_opts(source, "", vars, &opts, &mut vec![])
            .map(drop)
    }),
];

const IMPORT_ROOT: &str = "@import \"./part.mds\" as part\nroot\n";
const IMPORT_FILES: [(&str, &str); 2] = [("main.mds", IMPORT_ROOT), ("part.mds", "part\n")];

/// Run `entry` on a new cache with `headroom` left. Every run ends with no module
/// on the stack and no temporary.
fn run_with_vars(
    entry: VarsEntry,
    headroom: usize,
    vars: &HashMap<String, Value>,
) -> (ModuleCache, Result<(), MdsError>) {
    let mut cache = virtual_cache(&IMPORT_FILES);
    limit_cache_headroom(&mut cache, headroom);
    let result = entry(&mut cache, IMPORT_ROOT, vars);
    assert!(cache.resolving.is_empty());
    assert_eq!(cache.budget.mem.mark(), 0);
    (cache, result)
}

/// A 64 KiB text and 100 small variables, so the table, the names and the values
/// all count.
fn caller_vars() -> HashMap<String, Value> {
    let mut vars: HashMap<_, _> = (0..100)
        .map(|i| (format!("v{i:02}"), Value::Null))
        .collect();
    vars.insert("pad".to_owned(), Value::String("x".repeat(1 << 16)));
    vars
}

#[test]
fn each_entry_charges_the_caller_variable_layer_once_at_the_root() {
    let vars = caller_vars();
    let layer = runtime_layer(&vars).bytes().unwrap();
    let names: usize = vars.keys().map(String::len).sum();
    let per_var = size_of::<(String, crate::scope::Slot)>() + size_of::<Value>();
    let floor = vars.len() * per_var + names + (1 << 16);
    assert!(layer >= floor, "layer {layer} B, floor {floor} B");
    let empty = HashMap::new();
    let (mut cases, mut misses) = (vec![], vec![]);
    for (what, entry) in VARS_ENTRIES {
        // Exactly at the least headroom the compile passes, one byte under it is refused.
        let least = |vars| least_passing_headroom(|room| run_with_vars(entry, room, vars).1);
        let added = least(&vars).checked_sub(least(&empty));
        if added != Some(layer) {
            misses.push(format!(
                "{what}: the variables add {added:?} B, not {layer} B"
            ));
        }
        // The layer is the first charge of the compile, in buffers below the root mark.
        // A layer exactly at the headroom is charged, and one byte more is refused.
        for (headroom, charged) in [(layer, layer), (layer - 1, 0)] {
            let (cache, result) = run_with_vars(entry, headroom, &vars);
            let probes: Vec<_> = cache
                .module_probes
                .iter()
                .map(|probe| (probe.buffers_before, probe.temporaries_before))
                .collect();
            if probes != [(charged, 0)] {
                misses.push(format!("{what} at headroom {headroom}: {probes:?}"));
            }
            cases.push((format!("{what} at headroom {headroom}"), result));
        }
    }
    misses.extend(missed_memory_refusals(cases));
    assert!(misses.is_empty(), "{misses:#?}");
}

#[test]
fn empty_caller_variables_charge_no_layer_bytes() {
    assert_eq!(runtime_layer(&HashMap::new()).bytes(), Some(0));
    for (what, entry) in VARS_ENTRIES {
        let (cache, result) = run_with_vars(entry, MAX_COMPILE_MEMORY, &HashMap::new());
        result.unwrap();
        assert_eq!(cache.module_probes[0].buffers_before, 0, "{what}");
    }
}

#[test]
fn a_layer_changed_after_its_count_fails_the_root_charge() {
    assert_eq!(VarLayer::new().bytes(), Some(0));
    let mut layer = runtime_layer(&HashMap::from([("a".to_owned(), Value::Null)]));
    assert!(layer.bytes().is_some());
    // An insert after the collect makes the count unknown, never stale.
    Arc::get_mut(&mut layer)
        .unwrap()
        .insert("b".to_owned(), Arc::new(Value::Null));
    assert_eq!(layer.bytes(), None);
    let mut cache = virtual_cache(&[]);
    let (_, charged) = cache.begin_module("main.mds", &layer, 0);
    assert_memory_refusal(charged, "a layer with no byte count");
}

#[test]
fn a_reused_cache_retains_kept_bytes_and_never_the_caller_layer() {
    let other = "other\n";
    let mut cache = virtual_cache(&[IMPORT_FILES[0], IMPORT_FILES[1], ("other.mds", other)]);
    let vars = caller_vars();
    let layer = runtime_layer(&vars).bytes().unwrap();
    let main_kept =
        kept_bytes("part\n", "part.mds", "part\n") + kept_bytes(IMPORT_ROOT, "main.mds", "root\n");
    let mut retained = 0;
    for (key, kept) in [
        ("main.mds", main_kept),
        ("other.mds", kept_bytes(other, "other.mds", other)),
    ] {
        cache.resolve_key(key, &vars, &mut vec![]).unwrap();
        retained += kept;
        assert_eq!(cache.retained, retained, "{key}");
        assert_eq!(cache.budget.mem.buffers, layer, "{key}");
        assert_eq!(cache.budget.mem.mark(), 0, "{key}");
    }
}

#[test]
fn a_caller_value_past_the_counted_depth_is_refused_by_the_layer_charge() {
    let nested =
        |levels: usize| (0..levels).fold(Value::Null, |value, _| Value::Array(vec![value]));
    let parsed = |levels: usize| {
        let json = format!("{}null{}", "[".repeat(levels), "]".repeat(levels));
        Value::from_json(serde_json::from_str(&json).unwrap())
    };
    // The JSON input of the bindings and the CLI admits 64 levels and refuses 65.
    assert_eq!(parsed(64).unwrap(), nested(64));
    assert!(parsed(65).is_err());
    let compile = |levels| {
        let vars = HashMap::from([("deep".to_owned(), nested(levels))]);
        crate::compile_str_with("Body\n", None, Some(vars)).map(drop)
    };
    compile(64).unwrap();
    assert_memory_refusal(compile(65), "a value nested 65 levels deep");
}

/// A compile refuses a value past 64 levels at the variable charge, so this test
/// puts the value in a scope directly: the path checks past 64 steps still hold.
#[test]
fn a_path_past_64_steps_into_a_scope_value_keeps_its_errors() {
    let nest = |levels: usize, inner| {
        (0..levels).fold(inner, |value, _| {
            Value::Object(HashMap::from([("a".to_owned(), value)]))
        })
    };
    // `x` is 31 fields and one index deep. `y` is `fields` more fields and one index.
    let run = |fields: usize, inner: Value, tail: &str| {
        let value = nest(
            31,
            Value::Array(vec![nest(fields, Value::Array(vec![inner]))]),
        );
        let (x, y) = (".a".repeat(31), ".a".repeat(fields));
        let source = format!("@for x in v{x}:\n@for y in x{y}:\n{tail}\n@end\n@end\n");
        let tokens = tokenize(&source, "deep.mds").unwrap();
        let module = parse_with_ctx(&tokens, "deep.mds", &source).unwrap();
        let mut scope = Scope::new();
        scope.set_var("v", value);
        let error = evaluate(&module.body, &mut scope, &mut vec![], "deep.mds", &source);
        let record = error.unwrap_err().serialize();
        (record.code, record.message)
    };
    let leaf = || Value::String("leaf".into());
    // `y` is 64 steps deep, so `y.a` is 65 steps deep.
    assert_eq!(
        run(31, nest(1, leaf()), "{{y.a}}"),
        (
            "mds::syntax".to_owned(),
            "syntax error: internal error: an evaluated value has an invalid projection".to_owned()
        )
    );
    // `y.a` is 64 steps deep, so the loop variable `z` is 65 steps deep.
    let inner = nest(1, Value::Array(vec![leaf()]));
    assert_eq!(
        run(30, inner, "@for z in y.a:\n{{z}}\n@end"),
        (
            "mds::undefined_var".to_owned(),
            "undefined variable 'z'".to_owned()
        )
    );
}

/// Sources one byte past the `sourcesContent` ceiling: the map drops `sourcesContent`
/// with the ceiling warning and keeps every source and mapping. Sources at exactly the
/// ceiling keep `sourcesContent` and do not warn (control).
///
/// A compile does not reach this ceiling: the compile memory budget refuses a smaller
/// set of sources first, because each source byte costs at least 17 bytes of the
/// budget. The ceiling stays as an extra guard, so this test drives the check itself.
#[test]
fn sources_content_one_byte_past_the_ceiling_is_dropped_with_its_warning() {
    let body = "L1\nL2\n";
    let opts = CompileOptions::default()
        .with_source_map(true)
        .with_include_sources_content(true);
    let two_sources = || {
        let mut builder = MapBuilder::from_parts("l1.mds".into(), "l1.mds".into(), "L1\n".into());
        builder.push_segment(0, 0, 3);
        builder.switch_to(&Origin {
            file: Arc::from("l2.mds"),
            display: Arc::from("l2.mds"),
            source: Arc::from("L2\n"),
        });
        builder.push_segment(3, 0, 3);
        builder
    };
    // Non-vacuity: with no check, each source owns one mapped line and embeds its text.
    let full = two_sources().finalize_points(body).finish(body, 0, None);
    assert_eq!(full.sources, ["l1.mds", "l2.mds"]);
    assert_eq!(
        full.sources_content,
        Some(vec!["L1\n".to_string(), "L2\n".to_string()])
    );
    assert_eq!(
        full.mappings.split(';').filter(|l| !l.is_empty()).count(),
        2
    );
    for (total, over) in [
        (MAX_SOURCES_CONTENT_BYTES + 1, true),
        (MAX_SOURCES_CONTENT_BYTES, false),
    ] {
        let mut warnings = Vec::new();
        let sm = apply_map_degradation(two_sources(), total, &opts, &mut warnings)
            .expect("the ceiling never drops the whole map")
            .finalize_points(body)
            .finish(body, 0, None);
        // The literal pins the ceiling at 50 MiB.
        let expected_warnings = if over {
            vec![format!(
                "sourcesContent total size ({total} bytes) exceeds the 52428800 byte \
                 ceiling; sourcesContent will be omitted from the source map"
            )]
        } else {
            vec![]
        };
        assert_eq!(warnings, expected_warnings, "total={total}");
        let expected_content = if over {
            None
        } else {
            full.sources_content.clone()
        };
        assert_eq!(
            sm.sources_content, expected_content,
            "total={total}: sourcesContent is kept only up to the ceiling"
        );
        assert_eq!(
            sm.sources, full.sources,
            "total={total}: every source is kept"
        );
        assert_eq!(
            sm.mappings, full.mappings,
            "total={total}: every mapping is kept"
        );
    }
}
