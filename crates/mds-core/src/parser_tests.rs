//! Parser unit and integration tests, extracted from parser.rs.

use super::helpers::*;
use super::*;
use crate::ast::{Arg, CondValue, ExportDirective, Expr, ImportDirective};
use crate::lexer::tokenize;

#[test]
fn parse_simple_text() {
    let tokens = tokenize("Hello world!", "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    assert!(module.frontmatter.is_none());
    assert_eq!(module.body.len(), 1);
}

#[test]
fn parse_frontmatter() {
    let src = "---\nname: Alice\n---\nHello!";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    assert!(module.frontmatter.is_some());
    assert!(module.frontmatter.unwrap().raw.contains("name: Alice"));
}

#[test]
fn parse_if_block() {
    let src = "@if premium:\nPremium!\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    assert!(matches!(module.body[0], Node::If(_)));
}

#[test]
fn parse_if_else() {
    let src = "@if premium:\nPremium!\n@else:\nFree!\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::If(block) = &module.body[0] {
        assert!(block.else_body.is_some());
    } else {
        panic!("expected If node");
    }
}

#[test]
fn parse_for_block() {
    let src = "@for item in items:\n- {item}\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    assert!(matches!(module.body[0], Node::For(_)));
}

#[test]
fn parse_define() {
    let src = "@define greet(name):\nHello {name}!\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    assert!(matches!(module.body[0], Node::Define(_)));
}

#[test]
fn parse_import_alias() {
    let src = "@import \"./utils.mds\" as utils\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    assert!(matches!(
        module.body[0],
        Node::Import(ImportDirective::Alias { .. })
    ));
}

#[test]
fn parse_import_merge() {
    let src = "@import \"./base.mds\"\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    assert!(matches!(
        module.body[0],
        Node::Import(ImportDirective::Merge { .. })
    ));
}

#[test]
fn parse_import_selective() {
    let src = "@import { greet, farewell } from \"./utils.mds\"\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::Import(ImportDirective::Selective { names, .. }) = &module.body[0] {
        assert_eq!(names, &["greet", "farewell"]);
    } else {
        panic!("expected Selective import");
    }
}

#[test]
fn parse_export_named() {
    let src = "@export greet\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    assert!(matches!(
        module.body[0],
        Node::Export(ExportDirective::Named { .. })
    ));
}

#[test]
fn parse_export_reexport() {
    let src = "@export greet from \"./greetings.mds\"\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    assert!(matches!(
        module.body[0],
        Node::Export(ExportDirective::ReExport { .. })
    ));
}

#[test]
fn parse_export_wildcard() {
    let src = "@export * from \"./formatting.mds\"\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    assert!(matches!(
        module.body[0],
        Node::Export(ExportDirective::Wildcard { .. })
    ));
}

#[test]
fn parse_include() {
    let src = "@include footer\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    assert!(matches!(module.body[0], Node::Include(_)));
}

#[test]
fn parse_function_call_interpolation() {
    let src = "{greet(\"Alice\")}";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::Interpolation(interp) = &module.body[0] {
        assert!(matches!(interp.expr, Expr::Call { .. }));
    } else {
        panic!("expected Interpolation node");
    }
}

#[test]
fn parse_qualified_call() {
    let src = "{utils.greet(\"Alice\")}";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::Interpolation(interp) = &module.body[0] {
        assert!(matches!(interp.expr, Expr::QualifiedCall { .. }));
    } else {
        panic!("expected Interpolation node with QualifiedCall");
    }
}

#[test]
fn parse_single_arg_lone_quote_returns_error() {
    // A lone `"` is not a valid string literal (len < 2) — must not panic
    let result = parse_single_arg("\"");
    assert!(result.is_err(), "lone quote should return Err, not panic");
}

#[test]
fn parse_single_arg_escaped_quote_in_string() {
    // `"say \"hi\""` should parse to the string: say "hi"
    let result = parse_single_arg(r#""say \"hi\"""#);
    assert!(result.is_ok(), "escaped quote in string should parse ok");
    if let Ok(Arg::StringLiteral(s)) = result {
        assert_eq!(s, r#"say "hi""#);
    } else {
        panic!("expected StringLiteral");
    }
}

#[test]
fn unescape_backslash_then_quote() {
    // `"a\\\"b"` inner content is `a\\\"b`:
    // \\  -> single backslash
    // \"  -> literal quote
    // Result: `a\"b` (backslash, quote, b)
    let result = parse_single_arg(r#""a\\\"b""#).unwrap();
    if let Arg::StringLiteral(s) = result {
        assert_eq!(s, "a\\\"b", "escaped backslash then escaped quote");
    } else {
        panic!("expected StringLiteral");
    }
}

#[test]
fn unescape_double_backslash() {
    // `"a\\\\b"` inner content is `a\\\\b`:
    // \\  -> single backslash
    // \\  -> single backslash
    // Result: `a\\b`
    let result = parse_single_arg(r#""a\\\\b""#).unwrap();
    if let Arg::StringLiteral(s) = result {
        assert_eq!(s, "a\\\\b", "double escaped backslash");
    } else {
        panic!("expected StringLiteral");
    }
}

// --- Tests for new features: MemberAccess, key-value @for, dot-path conditions ---

#[test]
fn parse_member_access_interpolation() {
    // {config.key} should produce Expr::MemberAccess
    let src = "{config.key}";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::Interpolation(interp) = &module.body[0] {
        if let Expr::MemberAccess { object, fields } = &interp.expr {
            assert_eq!(object, "config");
            assert_eq!(fields, &["key"]);
        } else {
            panic!("expected Expr::MemberAccess, got {:?}", interp.expr);
        }
    } else {
        panic!("expected Interpolation node");
    }
}

#[test]
fn parse_member_access_multi_segment() {
    // {a.b.c} should produce MemberAccess with two fields
    let src = "{a.b.c}";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::Interpolation(interp) = &module.body[0] {
        if let Expr::MemberAccess { object, fields } = &interp.expr {
            assert_eq!(object, "a");
            assert_eq!(fields, &["b", "c"]);
        } else {
            panic!("expected Expr::MemberAccess");
        }
    } else {
        panic!("expected Interpolation node");
    }
}

#[test]
fn parse_arg_member_access() {
    // {greet(config.name)} should produce Expr::Call with Arg::MemberAccess
    let src = r#"{greet(config.name)}"#;
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::Interpolation(interp) = &module.body[0] {
        if let Expr::Call { name, args } = &interp.expr {
            assert_eq!(name, "greet");
            assert_eq!(args.len(), 1);
            if let Arg::MemberAccess { object, fields } = &args[0] {
                assert_eq!(object, "config");
                assert_eq!(fields, &["name"]);
            } else {
                panic!("expected Arg::MemberAccess, got {:?}", args[0]);
            }
        } else {
            panic!("expected Expr::Call");
        }
    } else {
        panic!("expected Interpolation node");
    }
}

#[test]
fn parse_for_key_value_destructuring() {
    // @for key, value in obj: should produce ForBlock with key_var set
    let src = "@for key, value in obj:\n{key}: {value}\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::For(block) = &module.body[0] {
        assert_eq!(block.key_var.as_deref(), Some("key"));
        assert_eq!(block.var, "value");
        assert_eq!(block.iterable, &["obj"]);
    } else {
        panic!("expected For node");
    }
}

#[test]
fn parse_for_dot_path_iterable() {
    // @for item in data.list: — iterable is a dot-separated path
    let src = "@for item in data.list:\n- {item}\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::For(block) = &module.body[0] {
        assert_eq!(block.key_var, None);
        assert_eq!(block.var, "item");
        assert_eq!(block.iterable, &["data", "list"]);
    } else {
        panic!("expected For node");
    }
}

#[test]
fn parse_if_dot_path_condition() {
    // @if config.debug: — condition is Condition::Truthy with dot path
    let src = "@if config.debug:\nDebugging\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::If(block) = &module.body[0] {
        assert!(
            matches!(&block.condition, Condition::Truthy(p) if p == &["config", "debug"]),
            "expected Condition::Truthy([\"config\", \"debug\"]), got {:?}",
            block.condition
        );
        assert!(block.elseif_branches.is_empty());
    } else {
        panic!("expected If node");
    }
}

#[test]
fn parse_invalid_dot_path_interpolation_returns_error() {
    // {a.123.b} — "123" is not a valid identifier; should be an error
    let src = "{a.123.b}";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "test.mds", src);
    assert!(result.is_err(), "invalid dot-path segment should fail");
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("invalid dot-path"),
        "error should mention 'invalid dot-path', got: {err_msg}"
    );
}

// --- Tests for MAX_DOT_SEGMENTS limit ---

#[test]
fn parse_dot_path_at_limit_accepted() {
    // MAX_DOT_SEGMENTS segments (e.g. a.b.c...32 parts) must be accepted.
    let segments: Vec<&str> = std::iter::repeat_n("x", MAX_DOT_SEGMENTS).collect();
    let path = segments.join(".");
    let src = format!("{{{path}}}");
    let tokens = tokenize(&src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_ok(),
        "exactly MAX_DOT_SEGMENTS segments must be accepted: {result:?}"
    );
}

#[test]
fn parse_interpolation_dot_path_exceeds_limit_rejected() {
    // MAX_DOT_SEGMENTS + 1 segments in an interpolation must be rejected.
    let segments: Vec<&str> = std::iter::repeat_n("x", MAX_DOT_SEGMENTS + 1).collect();
    let path = segments.join(".");
    let src = format!("{{{path}}}");
    let tokens = tokenize(&src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "test.mds", &src);
    assert!(
        result.is_err(),
        "dot path exceeding MAX_DOT_SEGMENTS must be rejected"
    );
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("segment count"),
        "error must mention segment count, got: {err_msg}"
    );
}

#[test]
fn parse_if_condition_dot_path_exceeds_limit_rejected() {
    // @if with too many dot segments must be rejected.
    let segments: Vec<&str> = std::iter::repeat_n("x", MAX_DOT_SEGMENTS + 1).collect();
    let path = segments.join(".");
    let src = format!("@if {path}:\ncontent\n@end\n");
    let tokens = tokenize(&src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_err(),
        "@if dot path exceeding MAX_DOT_SEGMENTS must be rejected"
    );
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("segment count"),
        "error must mention segment count, got: {err_msg}"
    );
}

#[test]
fn parse_for_iterable_dot_path_exceeds_limit_rejected() {
    // @for with too many dot segments in iterable must be rejected.
    let segments: Vec<&str> = std::iter::repeat_n("x", MAX_DOT_SEGMENTS + 1).collect();
    let path = segments.join(".");
    let src = format!("@for item in {path}:\n- {{item}}\n@end\n");
    let tokens = tokenize(&src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_err(),
        "@for iterable dot path exceeding MAX_DOT_SEGMENTS must be rejected"
    );
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("segment count"),
        "error must mention segment count, got: {err_msg}"
    );
}

#[test]
fn parse_arg_dot_path_exceeds_limit_rejected() {
    // Function arg with too many dot segments must be rejected.
    let segments: Vec<&str> = std::iter::repeat_n("x", MAX_DOT_SEGMENTS + 1).collect();
    let path = segments.join(".");
    let result = parse_args(&path);
    assert!(
        result.is_err(),
        "arg dot path exceeding MAX_DOT_SEGMENTS must be rejected"
    );
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("segment count"),
        "error must mention segment count, got: {err_msg}"
    );
}

#[test]
fn unescape_unknown_sequence_preserved() {
    // `"a\nb"` — `\n` is not a recognized escape, kept verbatim
    let result = parse_single_arg(r#""a\nb""#).unwrap();
    if let Arg::StringLiteral(s) = result {
        assert_eq!(s, "a\\nb", "unknown escape sequence kept verbatim");
    } else {
        panic!("expected StringLiteral");
    }
}

#[test]
fn parse_args_escaped_comma_in_string() {
    // A comma inside a string arg must not split the arg
    let result = parse_args(r#""hello, world""#).unwrap();
    assert_eq!(result.len(), 1);
    if let Arg::StringLiteral(s) = &result[0] {
        assert_eq!(s, "hello, world");
    } else {
        panic!("expected StringLiteral");
    }
}

#[test]
fn parse_nesting_depth_limit_rejected() {
    // Build a source string with MAX_NESTING_DEPTH + 1 nested @if blocks.
    // Each @if requires a condition variable — we use "x" consistently.
    //
    // MAX_NESTING_DEPTH=64 keeps recursive parse frames well within the
    // 2 MB default thread stack, so no enlarged stack is required here.
    let depth = MAX_NESTING_DEPTH + 1;
    let mut src = String::new();
    for _ in 0..depth {
        src.push_str("@if x:\n");
    }
    for _ in 0..depth {
        src.push_str("@end\n");
    }
    let tokens = tokenize(&src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_err(),
        "nesting depth > MAX_NESTING_DEPTH must be rejected"
    );
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("nesting depth"),
        "error must mention nesting depth, got: {msg}"
    );
}

#[test]
fn parse_nesting_depth_at_limit_accepted() {
    // Exactly MAX_NESTING_DEPTH nested @if blocks must succeed.
    //
    // MAX_NESTING_DEPTH=64 keeps recursive parse frames well within the
    // 2 MB default thread stack, so no enlarged stack is required here.
    let depth = MAX_NESTING_DEPTH;
    let mut src = String::new();
    for _ in 0..depth {
        src.push_str("@if x:\n");
    }
    for _ in 0..depth {
        src.push_str("@end\n");
    }
    let tokens = tokenize(&src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_ok(),
        "nesting depth == MAX_NESTING_DEPTH must be accepted: {result:?}"
    );
}

#[test]
fn is_valid_identifier_rejects_unicode() {
    assert!(!is_valid_identifier("café"), "unicode must be rejected");
    assert!(
        !is_valid_identifier("αβγ"),
        "greek letters must be rejected"
    );
    assert!(is_valid_identifier("hello"), "ascii ident must be accepted");
    assert!(is_valid_identifier("_foo_42"), "underscored ident ok");
}

// --- Tests for batch-1 fixes ---

// Fix: parser:212:error-msg — @elseif outside @if gives targeted error
#[test]
fn elseif_outside_if_gives_targeted_error() {
    let src = "@elseif x:\nfoo\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(result.is_err(), "@elseif outside @if must be rejected");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("@elseif must appear inside an @if block"),
        "error must mention @if block context, got: {msg}"
    );
}

#[test]
fn elseif_colon_without_condition_gives_targeted_error() {
    // @elseif: (has colon but no condition) used as a top-level directive
    let src = "@elseif:\nfoo\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(result.is_err(), "@elseif: at top level must be rejected");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("condition required") || msg.contains("@elseif"),
        "error must mention missing condition, got: {msg}"
    );
}

#[test]
fn unknown_directive_lists_elseif() {
    // An unrecognized directive gives an error listing valid directives
    // including @elseif
    let src = "@bogus\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(result.is_err(), "@bogus must be rejected");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("@elseif"),
        "valid-directives list must include @elseif, got: {msg}"
    );
}

// Fix: parser:464:nan-infinity — NaN/Infinity rejected in condition values
#[test]
fn condition_value_nan_rejected() {
    let result = parse_cond_value("NaN");
    assert!(result.is_err(), "NaN must be rejected as a condition value");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("NaN") || msg.contains("infinity"),
        "error must mention NaN/infinity, got: {msg}"
    );
}

#[test]
fn condition_value_infinity_rejected() {
    let result = parse_cond_value("inf");
    assert!(
        result.is_err(),
        "infinity must be rejected as a condition value"
    );
}

#[test]
fn condition_value_negative_infinity_rejected() {
    let result = parse_cond_value("-inf");
    assert!(
        result.is_err(),
        "-infinity must be rejected as a condition value"
    );
}

#[test]
fn condition_value_finite_numbers_accepted() {
    assert!(parse_cond_value("42").is_ok());
    assert!(parse_cond_value("-5").is_ok());
    assert!(parse_cond_value("3.14").is_ok());
}

// Fix: parser:436:escape-sequences — escape sequences in condition string literals
#[test]
fn condition_value_escaped_quote_in_string() {
    // @if var == "say \"hi\"": — inner escaped quote must be unescaped
    let result = parse_cond_value(r#""say \"hi\"""#);
    assert!(
        result.is_ok(),
        "escaped quote in condition value must parse"
    );
    if let Ok(CondValue::String(s)) = result {
        assert_eq!(s, r#"say "hi""#, "escaped quote must be unescaped");
    } else {
        panic!("expected CondValue::String");
    }
}

#[test]
fn condition_value_unescaped_string_unchanged() {
    // Plain strings with no escapes must pass through unchanged
    let result = parse_cond_value(r#""hello world""#);
    assert!(result.is_ok());
    if let Ok(CondValue::String(s)) = result {
        assert_eq!(s, "hello world");
    } else {
        panic!("expected CondValue::String");
    }
}

// Fix: parser:493:escape-order — escaped close-quote inside string does not
// terminate the string prematurely in find_unquoted_operator
#[test]
fn find_unquoted_operator_escaped_close_quote_not_terminator() {
    // In `var == "say \"hi\""`, the \" inside the string must not end the string.
    // The operator == must still be found (outside the string).
    let result = find_unquoted_operator(r#"var == "say \"hi\"""#);
    assert!(
        result.is_some(),
        "== must be found outside the string literal"
    );
    let (pos, op) = result.unwrap();
    assert_eq!(op, "==");
    assert_eq!(pos, 4, "== must be at byte 4");
}

// --- Tests for MAX_ELSEIF_BRANCHES limit ---

#[test]
fn parse_elseif_branch_at_limit_accepted() {
    // Exactly MAX_ELSEIF_BRANCHES @elseif branches must be accepted.
    let mut src = String::from("@if flag:\nfirst\n");
    for i in 0..MAX_ELSEIF_BRANCHES {
        src.push_str(&format!("@elseif flag{i}:\nbranch{i}\n"));
    }
    src.push_str("@end\n");
    let tokens = tokenize(&src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_ok(),
        "exactly MAX_ELSEIF_BRANCHES @elseif branches must be accepted: {result:?}"
    );
}

#[test]
fn parse_elseif_branch_limit_rejected() {
    // MAX_ELSEIF_BRANCHES + 1 @elseif branches must be rejected with a
    // descriptive error that mentions the branch limit.
    let mut src = String::from("@if flag:\nfirst\n");
    for i in 0..=MAX_ELSEIF_BRANCHES {
        src.push_str(&format!("@elseif flag{i}:\nbranch{i}\n"));
    }
    src.push_str("@end\n");
    let tokens = tokenize(&src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_err(),
        "more than MAX_ELSEIF_BRANCHES @elseif branches must be rejected"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("branch") || err.contains(&MAX_ELSEIF_BRANCHES.to_string()),
        "error must mention branch limit, got: {err}"
    );
}
