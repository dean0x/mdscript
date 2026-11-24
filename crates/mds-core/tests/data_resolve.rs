use std::collections::HashMap;

use mds::{
    compile_virtual_fs, CompileOptions, CompileResult, MdsError, ModuleCache, Value, VirtualFs,
};

fn compile(
    files: &[(&str, &str)],
    vars: Option<HashMap<String, Value>>,
) -> Result<CompileResult, MdsError> {
    let fs = VirtualFs::new(files.iter().map(|&(k, v)| (k.into(), v.into())).collect());
    compile_virtual_fs(fs, "main.mds", vars, CompileOptions::default())
}

fn rendered(result: Result<CompileResult, MdsError>) -> String {
    result.unwrap().into_markdown().unwrap()
}

fn replay(name: &str) -> Option<HashMap<String, Value>> {
    Some(HashMap::from([(name.into(), Value::from("replayed"))]))
}

#[test]
fn binds_local_data_before_functions_capture_their_scope() {
    let main = "@import \"./parts/card.mds\" as card\n{{card.title()}}\n";
    let part = "---\ndata:\n  meta: ./note.yml\n---\n@define title():\n{{meta.title}}\n@end\n";
    let output = rendered(compile(
        &[
            ("main.mds", main),
            ("parts/card.mds", part),
            ("parts/note.yml", "title: Example"),
        ],
        None,
    ));
    assert_eq!(output.trim(), "Example");
}

#[test]
fn mds_data_is_not_a_variable_but_an_ordinary_mapping_is() {
    for (key, reserved) in [("data", true), ("info", false)] {
        let source = format!("---\n{key}:\n  settings: ./data.json\n---\n{{{{{key}.settings}}}}\n");
        let result = compile(&[("main.mds", &source), ("data.json", "42")], None);
        if reserved {
            assert!(
                matches!(result, Err(MdsError::UndefinedVariable { .. })),
                "{result:?}"
            );
        } else {
            assert!(rendered(result).ends_with("./data.json\n"));
        }
    }
}

#[test]
fn runtime_data_mapping_is_not_a_declaration() {
    let vars = HashMap::from([(
        "data".into(),
        Value::Object(HashMap::from([(
            "settings".into(),
            Value::from("./bad.yml"),
        )])),
    )]);
    assert_eq!(
        rendered(compile(
            &[("main.mds", "{{data.settings}}\n"), ("bad.yml", "[")],
            Some(vars)
        )),
        "./bad.yml\n"
    );
    let source = "---\ndata:\n  settings: ./bad.yml\n---\n{{settings}}\n";
    let error = compile(&[("main.mds", source), ("bad.yml", "[")], None).unwrap_err();
    assert_eq!(error.serialize().code, "mds::yaml", "{error}");
}

#[test]
fn declarations_cannot_claim_frontmatter_or_import_alias_names_even_with_replay() {
    let part = ("part.mds", "Part.\n");
    for (fm, body) in [
        ("settings: 1\n", ""),
        ("imports: [{path: ./part.mds, as: settings}]\n", ""),
        ("", "@import \"./part.mds\" as settings\n"),
    ] {
        let source =
            format!("---\n{fm}data:\n  settings: ./missing.json\n---\n{body}{{{{settings}}}}\n");
        let err = compile(&[("main.mds", &source), part], replay("settings")).unwrap_err();
        assert!(matches!(err, MdsError::NameCollision { .. }), "{err}");
    }
    let source = "---\ndata:\n  settings: ./missing.json\n---\n{{settings}}\n";
    assert!(rendered(compile(&[("main.mds", source)], replay("settings"))).ends_with("replayed\n"));
}

#[test]
fn replay_replaces_the_entry_in_the_root_and_in_imported_modules() {
    let main = "---\ndata:\n  settings: ./bad.yml\n---\n@import \"./part.mds\" as part\n{{settings}}\n@include part\n";
    let part = "---\ndata:\n  settings: ./bad.yml\n---\n{{settings}}\n";
    let files = [("main.mds", main), ("part.mds", part), ("bad.yml", "[")];
    assert!(rendered(compile(&files, replay("settings"))).ends_with("replayed\nreplayed\n"));
    let err = compile(&files, None).unwrap_err();
    assert_eq!(err.serialize().code, "mds::yaml", "{err}");
    assert!(err.to_string().contains("data file \"bad.yml\""), "{err}");
}

#[test]
fn graph_errors_in_a_later_module_precede_local_data_reads() {
    let main =
        "@import \"./first.mds\" as first\n@import \"./later.mds\" as later\n@include first\n";
    let first = "---\ndata:\n  settings: ./bad.yml\n---\n{{settings}}\n";
    for (entries, reason) in [
        (
            "remote: 'https://example.com/{{unknown}}'",
            "unknown data reference name",
        ),
        (
            "remote: 'https://example.com/{{remote}}'",
            "dependency cycle",
        ),
        (
            "remote: 'https://example.com/{{peer}}'\n  peer: 'https://example.com/{{remote}}'",
            "dependency cycle",
        ),
    ] {
        let later = format!("---\ndata:\n  {entries}\n---\nLater.\n");
        let files = [
            ("main.mds", main),
            ("first.mds", first),
            ("later.mds", &later),
            ("bad.yml", "["),
        ];
        let err = compile(&files, None).unwrap_err();
        assert!(matches!(err, MdsError::Data { .. }), "{err}");
        assert!(err.to_string().contains("later.mds"), "{err}");
        assert!(err.to_string().contains(reason), "{err}");
    }
    let later = "---\ndata:\n  remote: 'https://example.com/{{name}}'\nname: ok\n---\nLater.\n";
    let files = [
        ("main.mds", main),
        ("first.mds", first),
        ("later.mds", later),
        ("bad.yml", "["),
    ];
    let err = compile(&files, replay("remote")).unwrap_err();
    assert_eq!(err.serialize().code, "mds::yaml", "{err}");
    assert!(err.to_string().contains("data file \"bad.yml\""), "{err}");
}

#[test]
fn static_url_checks_run_before_replay_and_unreplayed_urls_are_denied() {
    let compile_url = |url, vars| {
        let source = format!("---\ndata:\n  remote: '{url}'\nname: ok\n---\n{{{{remote}}}}\n");
        compile(&[("main.mds", &source)], vars)
    };
    let url = "https://example.com/{{name}}";
    assert!(rendered(compile_url(url, replay("remote"))).ends_with("replayed\n"));
    let err = compile_url("https://{{name}}/path", replay("remote")).unwrap_err();
    assert!(matches!(err, MdsError::Data { .. }), "{err}");
    let err = compile_url(url, None).unwrap_err();
    assert!(matches!(err, MdsError::NetworkDenied { .. }), "{err}");
    assert!(err.to_string().contains("remote"), "{err}");
    for url in [
        "http://example.com/path",
        "file:///path",
        "data:text/plain,path",
        "other://example.com/path",
    ] {
        let err = compile_url(url, replay("remote")).unwrap_err();
        assert!(matches!(err, MdsError::Data { .. }), "{err}");
    }
}

#[test]
fn data_values_are_text_and_do_not_add_module_edges() {
    let main = "---\ndata:\n  settings: ./value.json\n---\n@import \"./part.mds\" as part\n{{settings}}\n@include part\n";
    let files = |json| {
        compile(
            &[
                ("main.mds", main),
                ("part.mds", "Part.\n"),
                ("value.json", json),
            ],
            None,
        )
        .unwrap()
    };
    let control = files(r#""abc""#);
    let literal = files(r#""{{unknown}}\n@import \"./absent.mds\" as p\n@message user:""#);
    assert_eq!(control.dependencies, ["part.mds", "value.json"]);
    assert_eq!(control.dependencies, literal.dependencies);
    assert!(control.into_markdown().unwrap().ends_with("abc\nPart.\n"));
    assert!(literal
        .into_markdown()
        .unwrap()
        .ends_with("{{unknown}}\n@import \"./absent.mds\" as p\n@message user:\nPart.\n"));
}

#[test]
fn default_cache_does_not_resolve_or_read_data_files() {
    let main = "---\ndata:\n  settings: ./bad.yml\n---\n{{settings}}\n";
    let mut cache = ModuleCache::virtual_fs(HashMap::from([
        ("main.mds".into(), main.into()),
        ("bad.yml".into(), "[".into()),
    ]));
    let err = cache
        .resolve_virtual_intrinsic("main.mds", &HashMap::new(), &mut vec![])
        .unwrap_err();
    assert!(matches!(err, MdsError::UndefinedVariable { .. }), "{err}");
    let err = compile(&[("main.mds", main), ("bad.yml", "[")], None).unwrap_err();
    assert_eq!(err.serialize().code, "mds::yaml", "{err}");
    assert!(err.to_string().contains("data file \"bad.yml\""), "{err}");
}

#[test]
fn canonical_memo_does_not_charge_data_bytes_again_for_an_alias_or_another_module() {
    // Four reads of this file would exceed the 32 MiB total. The canonical file
    // is one byte below the individual 10 MiB cap and has only one value node.
    let mut text = " ".repeat(10 * 1024 * 1024 - 2);
    text.push('7');
    let declarations = "---\ndata:\n  item: ./item.json\n  alias: ./alias.json\n---\n";
    let main = format!(
        "{declarations}@import \"./part.mds\" as part\n{{{{item}}}} {{{{alias}}}}\n@include part\n"
    );
    let part = format!(
        "{declarations}@import \"./last.mds\" as last\n{{{{item}}}} {{{{alias}}}}\n@include last\n"
    );
    let last = format!("{declarations}{{{{item}}}} {{{{alias}}}}\n");
    let fs = VirtualFs::new(HashMap::from([
        ("main.mds".into(), main),
        ("part.mds".into(), part),
        ("last.mds".into(), last),
        ("item.json".into(), text),
    ]))
    .with_aliases(HashMap::from([("alias.json".into(), "item.json".into())]))
    .unwrap();
    assert!(rendered(compile_virtual_fs(
        fs,
        "main.mds",
        None,
        CompileOptions::default()
    ))
    .ends_with("7 7\n7 7\n7 7\n"));
}

#[test]
fn dependencies_list_modules_then_data_files_in_first_read_order() {
    // The entry binds its entries before it resolves its imports. A file that two
    // modules name, directly or through an alias, is read once and listed once.
    let main = "---\ndata:\n  shared: ./shared.json\n  own: ./own.yml\n---\n@import \"./a.mds\" as a\n@import \"./b.mds\" as b\n{{shared}} {{own}}\n@include a\n@include b\n";
    let a = "---\ndata:\n  item: ./parts/a.json\n  same: ./shared.json\n---\n{{item}} {{same}}\n";
    let b = "---\ndata:\n  item: ./alias.json\n  remote: https://example.com/data.json\n  skipped: ./absent.json\n---\n{{item}} {{remote}} {{skipped}}\n";
    let fs = VirtualFs::new(HashMap::from([
        ("main.mds".into(), main.into()),
        ("a.mds".into(), a.into()),
        ("b.mds".into(), b.into()),
        ("shared.json".into(), "1".into()),
        ("own.yml".into(), "2".into()),
        ("parts/a.json".into(), "3".into()),
    ]))
    .with_aliases(HashMap::from([("alias.json".into(), "shared.json".into())]))
    .unwrap();
    // Replay supplies the URL entry and the absent file, so neither is read.
    let vars = HashMap::from([
        ("remote".into(), Value::from("r")),
        ("skipped".into(), Value::from("s")),
    ]);
    let result = compile_virtual_fs(fs, "main.mds", Some(vars), CompileOptions::default());
    let result = result.unwrap();
    assert_eq!(
        result.dependencies,
        ["a.mds", "b.mds", "shared.json", "own.yml", "parts/a.json"]
    );
    assert!(result
        .into_markdown()
        .unwrap()
        .ends_with("1 2\n3 1\n1 r s\n"));
}

#[test]
fn virtual_compile_keeps_data_free_extends_working() {
    let child = "@extends \"./base.mds\"\n@block body:\nChild.\n@end\n";
    let base = "@block body:\nBase.\n@end\n";
    assert_eq!(
        rendered(compile(&[("main.mds", child), ("base.mds", base)], None)),
        "Child.\n"
    );
}

#[test]
fn typed_markdown_uses_the_mds_data_scope() {
    let source = "---\ntype: mds\ndata:\n  item: ./item.json\n---\n{{item}}\n";
    let fs = VirtualFs::new(HashMap::from([
        ("main.md".into(), source.into()),
        ("item.json".into(), "42".into()),
    ]));
    assert!(rendered(compile_virtual_fs(
        fs,
        "main.md",
        None,
        CompileOptions::default()
    ))
    .ends_with("42\n"));
}
