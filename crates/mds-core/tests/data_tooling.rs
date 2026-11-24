use std::collections::HashMap;
use std::fs;

use mds::{CompileOptions, LintConfig, Value};

const SOURCE: &str = "---\ndata:\n  rows: ./bad.yml\n  other: ./bad.yml\n---\n@for row in rows:\n{{row.name}}\n@end\n@for item in other.items:\n{{item}}\n@end\n";

fn modules(files: &[(&str, &str)]) -> HashMap<String, String> {
    files
        .iter()
        .map(|(k, v)| ((*k).into(), (*v).into()))
        .collect()
}

#[test]
fn tooling_checks_pending_variables_without_opening_data() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let entry = root.join("main.mds");
    fs::write(&entry, SOURCE).unwrap();
    fs::write(root.join("bad.yml"), "[").unwrap();

    let ((), warnings) = mds::check_collecting_warnings(&entry, None).unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("rows") && warnings[0].contains("other"));
    mds::lint(&entry, None, &LintConfig::default()).unwrap();
    mds::format_str_with(SOURCE, Some(root)).unwrap();

    let err = mds::compile_with_deps(&entry, None).unwrap_err();
    assert_eq!(err.serialize().code, "mds::yaml");
    assert!(err.to_string().contains("bad.yml"));
    let typo = SOURCE.replace("{{item}}", "{{typo}}");
    let err = mds::check_str_with(&typo, Some(root), None).unwrap_err();
    assert_eq!(err.serialize().code, "mds::undefined_var");
}

#[test]
fn pending_data_suppresses_evaluation_even_in_earlier_imports() {
    let main = "@import \"./first.mds\" as first\n@import \"./later.mds\" as later\n@include first\n@include later\n";
    let first = "---\nobject: {}\n---\n{{object.absent}}\n";
    let later = "---\ndata:\n  settings: ./bad.yml\n---\n{{upper(settings.key)}}\n";
    let files = modules(&[
        ("main.mds", main),
        ("first.mds", first),
        ("later.mds", later),
        ("bad.yml", "["),
    ]);
    let ((), warnings) =
        mds::check_virtual_collecting_warnings(files.clone(), "main.mds", None).unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("settings"));
    mds::lint_virtual(files.clone(), "main.mds", None, &LintConfig::default()).unwrap();

    let replay = Some(HashMap::from([(
        "settings".into(),
        Value::String("replayed".into()),
    )]));
    let err = mds::check_virtual(files, "main.mds", replay).unwrap_err();
    assert_eq!(err.serialize().code, "mds::syntax");
    assert!(err.to_string().contains("absent"), "{err}");
}

#[test]
fn selective_prompt_import_has_unknown_value_until_replay() {
    let main = "@import { prompt } from \"./part.mds\"\n{{prompt}}\n";
    let part = "---\ndata:\n  item: ./bad.yml\n---\n{{item}}\n";
    let files = modules(&[("main.mds", main), ("part.mds", part), ("bad.yml", "[")]);
    let ((), warnings) =
        mds::check_virtual_collecting_warnings(files.clone(), "main.mds", None).unwrap();
    assert_eq!(warnings.len(), 1);
    let replay = Some(HashMap::from([("item".into(), Value::from("replayed"))]));
    let ((), warnings) = mds::check_virtual_collecting_warnings(files, "main.mds", replay).unwrap();
    assert!(warnings.is_empty());
}

#[test]
fn compile_entry_points_bind_local_data() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let entry = root.join("main.mds");
    let src = "---\ndata:\n  title: ./title.json\n---\n{{title}}\n";
    fs::write(root.join("title.json"), "\"bound\"").unwrap();
    fs::write(&entry, src).unwrap();
    let files = modules(&[("main.mds", src), ("title.json", "\"bound\"")]);
    for result in [
        mds::compile_with_deps(&entry, None),
        mds::compile_with_deps_opts(&entry, None, CompileOptions::default()),
        mds::compile_str_with_deps(src, Some(root), None),
        mds::compile_str_with_deps_opts(src, Some(root), None, CompileOptions::default()),
        mds::compile_virtual_with_deps(files, "main.mds", None),
    ] {
        assert!(result
            .unwrap()
            .into_markdown()
            .unwrap()
            .ends_with("bound\n"));
    }
}

#[test]
fn url_frontier_precedes_local_reads_and_follows_module_then_declaration_order() {
    let main = "---\ndata:\n  root_z: https://example.com/z\n  root_a: https://example.com/a\n  local: ./bad.yml\n---\n@import \"./part.mds\" as part\n@include part\n";
    let part = "---\ndata:\n  part_z: https://example.com/z\n  part_a: https://example.com/a\n---\nPart.\n";
    let files = modules(&[("main.mds", main), ("part.mds", part), ("bad.yml", "[")]);
    let compile = |vars| {
        mds::compile_virtual_with_deps_opts(
            files.clone(),
            "main.mds",
            vars,
            CompileOptions::default(),
        )
        .unwrap_err()
    };
    let err = compile(None);
    assert_eq!(err.serialize().code, "mds::network_denied");
    let message = err.to_string();
    let names = ["root_z", "root_a", "part_z", "part_a"];
    let positions = names.map(|name| message.find(name).unwrap());
    assert!(positions.windows(2).all(|p| p[0] < p[1]), "{message}");
    let replay = names
        .into_iter()
        .map(|name| (name.into(), Value::String("replayed".into())))
        .collect();
    let err = compile(Some(replay));
    assert_eq!(err.serialize().code, "mds::yaml");
    assert!(err.to_string().contains("bad.yml"));
}

#[test]
fn inherited_frontier_follows_each_declaring_modules_order() {
    let main = "---\ndata:\n  leaf_first: https://example.com/a\n  replaced: https://example.com/b\n---\n@extends \"./base.mds\"\n";
    let base = "---\ndata:\n  replaced: https://example.com/old\n  base_only: https://example.com/c\n---\nBody.\n";
    let err = mds::compile_virtual(
        modules(&[("main.mds", main), ("base.mds", base)]),
        "main.mds",
        None,
    )
    .unwrap_err();
    assert_eq!(err.serialize().code, "mds::network_denied");
    let text = err.to_string();
    let offsets = ["leaf_first", "replaced", "base_only"].map(|name| text.find(name).unwrap());
    assert!(offsets.windows(2).all(|pair| pair[0] < pair[1]), "{text}");
    assert_eq!(text.matches("replaced").count(), 1, "{text}");
}

#[test]
fn replay_keeps_static_type_checks() {
    let src = "---\ndata:\n  rows: ./missing.yml\n---\n@for row in rows:\n{{row}}\n@end\n";
    mds::check_str(src).unwrap();
    let vars = HashMap::from([("rows".into(), Value::Object(Default::default()))]);
    let err = mds::check_str_with(src, None, Some(vars)).unwrap_err();
    assert!(
        err.to_string().contains("cannot iterate over object"),
        "{err}"
    );
}
