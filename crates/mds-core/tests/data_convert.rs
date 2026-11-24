use std::collections::HashMap;

use mds::{compile_virtual_fs, CompileOptions, MdsError, VirtualFs};

fn compile(source: &str, ext: &str, raw: &str) -> Result<String, MdsError> {
    let key = format!("settings.{ext}");
    let fs = VirtualFs::new(HashMap::from([
        ("main.mds".into(), source.replace("FILE", &key)),
        (key, raw.into()),
    ]));
    compile_virtual_fs(fs, "main.mds", None, CompileOptions::default())?.into_markdown()
}

const SOURCE: &str = "---\ndata:\n  settings: ./FILE\n---\n{{settings}}\n";

fn data_error(ext: &str, raw: &str) -> String {
    let err = compile(SOURCE, ext, raw).unwrap_err();
    assert!(matches!(err, MdsError::Data { .. }), "{err}");
    let message = err.to_string();
    assert!(message.contains("entry \"settings\""), "{message}");
    message
}

#[test]
fn decoded_keys_and_values_refuse_disallowed_characters() {
    for ext in ["json", "yaml"] {
        for cp in [0x1b, 0x202e, 0x2028] {
            let escaped = format!("{}u{cp:04X}", char::from(92));
            for (raw, path) in [
                (
                    format!(r#"{{"outer":{{"key":"{escaped}"}}}}"#),
                    "outer.key".into(),
                ),
                (
                    format!(r#"{{"outer":{{"key{escaped}":0}}}}"#),
                    format!("outer.key{escaped}"),
                ),
            ] {
                let message = data_error(ext, &raw);
                assert!(message.contains(&path), "{message}");
                assert!(!message.contains(char::from_u32(cp).unwrap()), "{message}");
            }
        }
        let source = SOURCE.replace("{{settings}}", "<{{settings.ok}}>");
        assert!(compile(&source, ext, r#"{"ok":"a\tb\nc","key\t\n":0}"#)
            .unwrap()
            .ends_with("<a\tb\nc>\n"));
    }
    let message = data_error("yaml", &format!("key: \"{}\"", char::from(27)));
    assert!(message.contains("invalid YAML data"), "{message}");
}

#[test]
fn converts_crlf_only_in_string_values() {
    for ext in ["json", "yml"] {
        for raw in [r#""a\r\nb""#, r#""a\nb""#] {
            assert!(compile(SOURCE, ext, raw).unwrap().ends_with("a\nb\n"));
        }
        let message = data_error(ext, r#"{"outer":"a\rb"}"#);
        assert!(message.contains("outer"), "{message}");
        let message = data_error(ext, r#"{"a\r\nb":0}"#);
        let path = format!("a{slash}u000D{slash}u000Ab", slash = char::from(92));
        assert!(message.contains(&path), "{message}");
    }
}

#[test]
fn data_uses_bounded_format_parsers() {
    for ext in ["json", "yaml"] {
        let raw = format!("{}42", char::from_u32(0xfeff).unwrap());
        assert!(compile(SOURCE, ext, &raw).unwrap().ends_with("42\n"));
        assert!(compile(SOURCE, ext, "9007199254740992")
            .unwrap()
            .ends_with("9007199254740992\n"));
        data_error(ext, "9007199254740993");
        data_error(ext, r#"{"a":1,"a":2}"#);
    }
    data_error("json", "1e400");
    data_error("yaml", ".inf");
    assert!(compile(SOURCE, "yaml", "!local 42")
        .unwrap()
        .ends_with("42\n"));
    data_error("yaml", "42\n---\n43");
    data_error("json", "{\"a\":1 # YAML only\n}");
}

#[test]
fn frontmatter_keeps_its_number_policy() {
    let source = "---\nsettings: 9007199254740993\n---\n{{settings}}\n";
    assert!(compile(source, "json", "9007199254740993")
        .unwrap()
        .ends_with("9007199254740992\n"));
    data_error("json", "9007199254740993");
}
