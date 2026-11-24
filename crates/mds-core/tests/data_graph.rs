use std::collections::HashMap;

use mds::{CompileOptions, Value};

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
fn public_compile_applies_the_level_cap_before_url_denial() {
    let compile = |levels| {
        mds::compile_virtual_with_deps_opts(
            HashMap::from([("main.mds".into(), chain(levels))]),
            "main.mds",
            None,
            CompileOptions::default(),
        )
        .unwrap_err()
    };
    let err = compile(16);
    assert_eq!(err.serialize().code, "mds::network_denied", "{err}");
    let err = compile(17);
    assert_eq!(err.serialize().code, "mds::resource_limit", "{err}");
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
