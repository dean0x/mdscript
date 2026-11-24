use std::collections::HashMap;

use mds::Value;

fn chain(levels: usize) -> String {
    let mut source = String::from("---\ndata:\n  entry0: https://example.com/root\n");
    for level in 1..levels {
        source.push_str(&format!(
            "  entry{level}: 'https://example.com/{{{{entry{}}}}}'\n",
            level - 1
        ));
    }
    source.push_str("---\nHello.\n");
    source
}

#[test]
fn public_check_accepts_sixteen_levels_and_refuses_seventeen() {
    mds::check_str(&chain(16)).unwrap();
    let err = mds::check_str(&chain(17)).unwrap_err();
    assert_eq!(err.serialize().code, "mds::resource_limit", "{err}");
}

#[test]
fn graph_errors_precede_local_data_reads() {
    let compile = |refs: &str, runtime| {
        let source = format!("---\ndata:\n  local: ./bad.yml\n  a: 'https://example.com/{refs}'\n  b: 'https://example.com/{{{{a}}}}'\n---\nHello.\n");
        mds::compile_virtual_with_deps(
            HashMap::from([("main.mds".into(), source), ("bad.yml".into(), "[".into())]),
            "main.mds",
            runtime,
        )
        .unwrap_err()
    };
    for (refs, reason) in [
        ("{{unknown}}", "unknown"),
        ("{{a}}", "cycle"),
        ("{{b}}", "cycle"),
    ] {
        let err = compile(refs, None);
        assert_eq!(err.serialize().code, "mds::data", "{err}");
        assert!(err.to_string().contains(reason), "{err}");
    }
    let replay = ["a", "b"]
        .into_iter()
        .map(|key| (key.into(), Value::String("valid".into())))
        .collect();
    let err = compile("{{local}}", Some(replay));
    assert_eq!(err.serialize().code, "mds::yaml", "{err}");
    assert!(err.to_string().contains("bad.yml"), "{err}");
}

fn compile_graph(modules: &[(&str, &str)], entry: &str) -> mds::MdsError {
    let modules = modules
        .iter()
        .map(|(key, source)| ((*key).to_owned(), (*source).to_owned()))
        .collect();
    mds::compile_virtual_with_deps(modules, entry, None).unwrap_err()
}

/// The graph check reads every module before the compile runs. A fault that it finds
/// later in the graph must not replace the fault that the compile meets first.
/// Control: the missing import alone is refused, so the check does reach it.
#[test]
fn a_later_missing_import_does_not_replace_the_first_fault_of_the_compile() {
    let missing = "@import \"./missing.mds\" as m\n";
    let cycle = [
        (
            "main.mds",
            format!("@import \"./a.mds\" as a\n{missing}hi\n"),
        ),
        ("a.mds", "@import \"./main.mds\" as e\nA\n".to_owned()),
    ];
    let cycle: Vec<_> = cycle.iter().map(|(k, s)| (*k, s.as_str())).collect();
    let err = compile_graph(&cycle, "main.mds");
    assert_eq!(err.serialize().code, "mds::circular_import", "{err}");
    assert!(
        err.to_string()
            .contains("main.mds \u{2192} a.mds \u{2192} main.mds"),
        "{err}"
    );

    let evaluation = format!("@import \"./a.mds\" as a\n{missing}hi\n");
    let err = compile_graph(
        &[("main.mds", &evaluation), ("a.mds", "{{nope}}\n")],
        "main.mds",
    );
    assert_eq!(err.serialize().code, "mds::undefined_var", "{err}");

    let base_cycle = [
        ("a.mds", "@extends \"./b.mds\"\n@block body:\nA\n@end\n"),
        ("b.mds", "@extends \"./a.mds\"\n@block body:\nB\n@end\n"),
    ];
    let err = compile_graph(&base_cycle, "a.mds");
    assert_eq!(err.serialize().code, "mds::circular_import", "{err}");
    assert!(err.to_string().contains('\u{2192}'), "{err}");

    let control = format!("{missing}hi\n");
    let err = compile_graph(&[("main.mds", &control)], "main.mds");
    assert_eq!(err.serialize().code, "mds::module_not_found", "{err}");
}

/// A refused graph check stays final for data: the compile reads no data file, and a
/// module that binds an entry gets the refusal of the check.
/// Control: with no graph fault, the compile reads the bad file and reports it.
#[test]
fn a_refused_graph_check_still_blocks_every_data_read() {
    let source = |import: &str| format!("---\ndata:\n  x: ./bad.yml\n---\n{import}{{{{x}}}}\n");
    let refused = source("@import \"./missing.mds\" as m\n");
    let err = compile_graph(&[("main.mds", &refused), ("bad.yml", "[")], "main.mds");
    assert_eq!(err.serialize().code, "mds::module_not_found", "{err}");
    assert!(err.to_string().contains("missing.mds"), "{err}");

    let control = source("");
    let err = compile_graph(&[("main.mds", &control), ("bad.yml", "[")], "main.mds");
    assert_eq!(err.serialize().code, "mds::yaml", "{err}");
}
