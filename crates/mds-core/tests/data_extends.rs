use mds::{compile_virtual_fs, CompileOptions, MdsError, Value, VirtualFs};

fn compile(files: &[(&str, &str)]) -> Result<String, MdsError> {
    compile_with_replay(files, None)
}

fn compile_with_replay(files: &[(&str, &str)], name: Option<&str>) -> Result<String, MdsError> {
    let fs = VirtualFs::new(
        files
            .iter()
            .map(|&(key, src)| (key.into(), src.into()))
            .collect(),
    );
    let vars = name.map(|name| [(name.into(), Value::from("replayed"))].into());
    compile_virtual_fs(fs, "main.mds", vars, CompileOptions::default())?.into_markdown()
}

const BASE: &str = "---\ndata:\n  title: ./title.yml\n  suffix: ./suffix.yml\n---\n@block body:\n{{title}} {{suffix}}\n@end\n";

#[test]
fn inherited_files_use_the_directory_of_each_declaration() {
    let child = "@extends \"./middle/mid.mds\"\n";
    let middle = "---\ndata:\n  title: ./title.yml\n---\n@extends \"../base/base.mds\"\n";
    let output = compile(&[
        ("main.mds", child),
        ("middle/mid.mds", middle),
        ("base/base.mds", BASE),
        ("middle/title.yml", "Middle"),
        ("base/title.yml", "Base"),
        ("base/suffix.yml", "Suffix"),
    ])
    .unwrap();
    assert_eq!(output, "Middle Suffix\n");
}

#[test]
fn child_replaces_the_whole_entry_and_the_base_file_is_not_read() {
    let control = "@extends \"./base/base.mds\"\n";
    let child = format!("---\ndata:\n  title: ./title.json\n---\n{control}");
    let compile_child = |src| {
        compile(&[
            ("main.mds", src),
            ("base/base.mds", BASE),
            ("base/title.yml", "["),
            ("base/suffix.yml", "Suffix"),
            ("title.json", "\"Child\""),
        ])
    };
    assert_eq!(compile_child(&child).unwrap(), "Child Suffix\n");
    let err = compile_child(control).unwrap_err();
    assert_eq!(err.serialize().code, "mds::yaml");
    assert!(
        err.to_string().contains("data file \"base/title.yml\""),
        "{err}"
    );
    assert!(err.to_string().contains("entry \"title\""), "{err}");
}

#[test]
fn inherited_references_see_the_child_scope_before_any_data_read() {
    let base = "---\ndata:\n  remote: 'https://example.com/{{name}}'\n---\nBase.\n";
    let child = "@extends \"./base.mds\"\n";
    let files = |tail| {
        let part = format!("---\n{tail}data:\n  sample: ./bad.yml\n---\n{child}");
        compile(&[("main.mds", &part), ("base.mds", base), ("bad.yml", "[")]).unwrap_err()
    };
    let err = files("");
    assert_eq!(err.serialize().code, "mds::data");
    assert!(
        err.to_string().contains("unknown data reference name"),
        "{err}"
    );
    let err = files("name: valid\n");
    assert_eq!(err.serialize().code, "mds::network_denied");
    assert!(err.to_string().contains("remote"), "{err}");
}

#[test]
fn replacing_an_entry_can_remove_an_inherited_graph_cycle() {
    let base =
        "---\ndata:\n  item: 'https://example.com/{{item}}'\n---\n@block body:\n{{item}}\n@end\n";
    let ext = "@extends \"./base.mds\"\n";
    let control = [("main.mds", ext), ("base.mds", base)];
    let err = compile(&control).unwrap_err();
    assert!(err.to_string().contains("dependency cycle"), "{err}");
    let child = format!("---\ndata:\n  item: ./item.json\n---\n{ext}");
    assert_eq!(
        compile(&[
            ("main.mds", &child),
            ("base.mds", base),
            ("item.json", "42")
        ])
        .unwrap(),
        "42\n"
    );
}

#[test]
fn collisions_use_frontmatter_and_import_aliases_from_the_chain() {
    let base = "@import \"./part.mds\" as body_alias\n@block body:\n{{item}}\n@end\n";
    let child =
        |name| format!("---\ndata:\n  {name}: ./item.json\n---\n@extends \"./middle.mds\"\n");
    let middle = "---\nfm_var: 1\nimports: [{path: ./part.mds, as: fm_alias}]\n---\n@extends \"./base.mds\"\n";
    for name in ["fm_var", "fm_alias", "body_alias", "item"] {
        let source = child(name);
        let files = [
            ("main.mds", source.as_str()),
            ("middle.mds", middle),
            ("base.mds", base),
            ("part.mds", "Part.\n"),
        ];
        let result = compile_with_replay(&files, Some(name));
        if name == "item" {
            assert!(result.unwrap().ends_with("replayed\n"));
        } else {
            let err = result.unwrap_err();
            assert_eq!(err.serialize().code, "mds::name_collision", "{err}");
            assert!(err.to_string().contains(name), "{err}");
        }
    }
}

#[test]
fn invalid_later_inheritance_graph_precedes_a_data_read_in_an_earlier_import() {
    let main =
        "@import \"./first.mds\" as first\n@import \"./later.mds\" as later\n@include first\n";
    let first = "---\ndata:\n  item: ./bad.json\n---\n{{item}}\n";
    let later = "@extends \"./base.mds\"\n";
    let run = |reference, replay| {
        let base = format!("---\ndata:\n  remote: 'https://example.com/{{{{{reference}}}}}'\nname: ok\n---\nBase.\n");
        compile_with_replay(
            &[
                ("main.mds", main),
                ("first.mds", first),
                ("later.mds", later),
                ("base.mds", &base),
                ("bad.json", "["),
            ],
            replay,
        )
        .unwrap_err()
    };
    let err = run("unknown", None);
    assert_eq!(err.serialize().code, "mds::data");
    assert!(
        err.to_string().contains("unknown data reference name"),
        "{err}"
    );
    let err = run("name", None);
    assert_eq!(err.serialize().code, "mds::network_denied", "{err}");
    assert!(err.to_string().contains("remote"), "{err}");
    let err = run("name", Some("remote"));
    assert_eq!(err.serialize().code, "mds::yaml", "{err}");
    assert!(err.to_string().contains("bad.json"), "{err}");
}

#[test]
fn base_functions_capture_the_winning_data_binding() {
    let base = "---\ndata:\n  item: ./unused.json\nlabel: Base\n---\n@define inner():\n{{label}} {{item}}\n@end\n@define outer():\n{{inner()}}\n@end\n@block body:\n{{outer()}}\n@end\n";
    let child = "---\ndata:\n  item: ./item.json\nlabel: Child\n---\n@extends \"./base.mds\"\n";
    let output = compile(&[("main.mds", child), ("base.mds", base), ("item.json", "42")]).unwrap();
    assert!(output.ends_with("Base 42\n"), "{output}");
}

#[test]
fn a_cached_extending_import_still_carries_its_transitive_frontmatter_and_data() {
    let main = "@import \"./mid.mds\" as mid\n@import \"./leaf.mds\" as leaf\n@include leaf\n";
    let leaf = "@extends \"./mid.mds\"\n";
    let mid = "@extends \"./base.mds\"\n";
    let base = "---\nlabel: Base\ndata:\n  item: ./item.json\n---\n@block body:\n{{label}} {{item}}\n@end\n";
    let output = compile(&[
        ("main.mds", main),
        ("leaf.mds", leaf),
        ("mid.mds", mid),
        ("base.mds", base),
        ("item.json", "42"),
    ])
    .unwrap();
    assert_eq!(output, "Base 42\n");
}

#[test]
fn a_base_function_does_not_keep_its_standalone_data_value_in_a_child() {
    let main = "@import \"./base.mds\" as base\n@import \"./leaf.mds\" as leaf\n@include base\n@include leaf\n";
    let base = "---\ndata:\n  item: ./base.json\n---\n@define show():\n{{item}}\n@end\n@block body:\n{{show()}}\n@end\n";
    let leaf = "---\ndata:\n  item: ./leaf.json\n---\n@extends \"./base.mds\"\n";
    let output = compile(&[
        ("main.mds", main),
        ("base.mds", base),
        ("leaf.mds", leaf),
        ("base.json", "1"),
        ("leaf.json", "2"),
    ])
    .unwrap();
    assert_eq!(output, "1\n2\n");
}
