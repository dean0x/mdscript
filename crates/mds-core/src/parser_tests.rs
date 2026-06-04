//! Parser unit and integration tests, extracted from parser.rs.

use super::helpers::*;
use super::*;
use crate::ast::{Arg, CondValue, ExportDirective, Expr, ImportDirective};
use crate::lexer::tokenize;
use crate::limits::MAX_DOT_SEGMENTS;

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
        assert!(
            matches!(&block.iterable, Expr::Var(v) if v == "obj"),
            "expected Expr::Var(\"obj\"), got {:?}",
            block.iterable
        );
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
        assert!(
            matches!(&block.iterable, Expr::MemberAccess { object, fields }
                if object == "data" && fields == &["list"]),
            "expected Expr::MemberAccess(data.list), got {:?}",
            block.iterable
        );
    } else {
        panic!("expected For node");
    }
}

#[test]
fn parse_if_dot_path_condition() {
    // @if config.debug: — condition is Condition::Truthy with MemberAccess expr
    let src = "@if config.debug:\nDebugging\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::If(block) = &module.body[0] {
        assert!(
            matches!(&block.condition, Condition::Truthy(Expr::MemberAccess { object, fields })
                if object == "config" && fields == &["debug"]),
            "expected Condition::Truthy(MemberAccess{{config.debug}}), got {:?}",
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

// ── Arg literal parsing ───────────────────────────────────────────────────

#[test]
fn parse_arg_boolean_true() {
    let arg = parse_single_arg("true").unwrap();
    assert!(matches!(arg, Arg::BooleanLiteral(true)));
}

#[test]
fn parse_arg_boolean_false() {
    let arg = parse_single_arg("false").unwrap();
    assert!(matches!(arg, Arg::BooleanLiteral(false)));
}

#[test]
fn parse_arg_null() {
    let arg = parse_single_arg("null").unwrap();
    assert!(matches!(arg, Arg::NullLiteral));
}

#[test]
fn parse_arg_integer() {
    let arg = parse_single_arg("42").unwrap();
    assert!(matches!(arg, Arg::NumberLiteral(n) if n == 42.0));
}

#[test]
fn parse_arg_float() {
    let arg = parse_single_arg("1.5").unwrap();
    match arg {
        Arg::NumberLiteral(n) => assert!((n - 1.5).abs() < 1e-9),
        other => panic!("expected NumberLiteral, got {other:?}"),
    }
}

#[test]
fn parse_arg_negative_integer() {
    let arg = parse_single_arg("-5").unwrap();
    assert!(matches!(arg, Arg::NumberLiteral(n) if n == -5.0));
}

#[test]
fn parse_arg_negative_float() {
    let arg = parse_single_arg("-1.5").unwrap();
    match arg {
        Arg::NumberLiteral(n) => assert!((n - (-1.5)).abs() < 1e-9),
        other => panic!("expected NumberLiteral, got {other:?}"),
    }
}

#[test]
fn parse_arg_identifier_not_confused_with_number() {
    let arg = parse_single_arg("myVar").unwrap();
    assert!(matches!(arg, Arg::Var(_)));
}

// ── Default parameter parsing ─────────────────────────────────────────────

#[test]
fn parse_define_required_params() {
    let src = "@define greet(name):\nHello {name}!\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::Define(def) = &module.body[0] {
        assert_eq!(def.params.len(), 1);
        assert_eq!(def.params[0].name, "name");
        assert!(def.params[0].default.is_none());
    } else {
        panic!("expected Define node");
    }
}

#[test]
fn parse_define_default_string() {
    let src = "@define greet(name = \"World\"):\nHello {name}!\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::Define(def) = &module.body[0] {
        assert_eq!(def.params.len(), 1);
        assert_eq!(def.params[0].name, "name");
        assert!(matches!(
            &def.params[0].default,
            Some(crate::ast::CondValue::String(s)) if s == "World"
        ));
    } else {
        panic!("expected Define node");
    }
}

#[test]
fn parse_define_default_number() {
    let src = "@define repeat(n = 3):\n{n}\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::Define(def) = &module.body[0] {
        assert_eq!(def.params.len(), 1);
        assert!(
            matches!(&def.params[0].default, Some(crate::ast::CondValue::Number(n)) if *n == 3.0)
        );
    } else {
        panic!("expected Define node");
    }
}

#[test]
fn parse_define_default_negative_number() {
    let src = "@define offset(n = -1):\n{n}\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::Define(def) = &module.body[0] {
        assert!(
            matches!(&def.params[0].default, Some(crate::ast::CondValue::Number(n)) if *n == -1.0)
        );
    } else {
        panic!("expected Define node");
    }
}

#[test]
fn parse_define_default_bool() {
    let src = "@define toggle(flag = true):\n{flag}\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::Define(def) = &module.body[0] {
        assert!(matches!(
            &def.params[0].default,
            Some(crate::ast::CondValue::Boolean(true))
        ));
    } else {
        panic!("expected Define node");
    }
}

#[test]
fn parse_define_default_null() {
    let src = "@define maybe(x = null):\n{x}\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::Define(def) = &module.body[0] {
        assert!(matches!(
            &def.params[0].default,
            Some(crate::ast::CondValue::Null)
        ));
    } else {
        panic!("expected Define node");
    }
}

#[test]
fn parse_define_default_string_with_comma() {
    let src = "@define greet(sep = \"a, b\"):\n{sep}\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::Define(def) = &module.body[0] {
        assert!(matches!(
            &def.params[0].default,
            Some(crate::ast::CondValue::String(s)) if s == "a, b"
        ));
    } else {
        panic!("expected Define node");
    }
}

#[test]
fn parse_define_required_after_optional_rejected() {
    let src = "@define bad(a = \"x\", b):\n{a}\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_err(),
        "required param after optional must be rejected"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("required") || err.contains("optional") || err.contains("cannot follow"),
        "error should mention ordering constraint, got: {err}"
    );
}

#[test]
fn parse_define_duplicate_param_rejected() {
    let src = "@define bad(a, a):\n{a}\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(result.is_err(), "duplicate param name must be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("duplicate"),
        "error should mention duplicate, got: {err}"
    );
}

#[test]
fn parse_define_mixed_required_and_optional() {
    let src = "@define greet(name, greeting = \"Hello\"):\n{greeting} {name}!\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::Define(def) = &module.body[0] {
        assert_eq!(def.params.len(), 2);
        assert!(
            def.params[0].default.is_none(),
            "first param should be required"
        );
        assert!(
            def.params[1].default.is_some(),
            "second param should have default"
        );
    } else {
        panic!("expected Define node");
    }
}

// ── Logical operators ─────────────────────────────────────────────────────────

#[test]
fn parse_condition_and_two_vars() {
    let cond = parse_condition("a && b").unwrap();
    assert!(matches!(cond, crate::ast::Condition::And(_)));
    if let crate::ast::Condition::And(ops) = cond {
        assert_eq!(ops.len(), 2);
    }
}

#[test]
fn parse_condition_or_two_vars() {
    let cond = parse_condition("a || b").unwrap();
    assert!(matches!(cond, crate::ast::Condition::Or(_)));
    if let crate::ast::Condition::Or(ops) = cond {
        assert_eq!(ops.len(), 2);
    }
}

#[test]
fn parse_condition_and_with_equality() {
    let cond = parse_condition("role == \"admin\" && active").unwrap();
    assert!(matches!(cond, crate::ast::Condition::And(_)));
    if let crate::ast::Condition::And(ops) = cond {
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], crate::ast::Condition::Eq(..)));
        assert!(matches!(ops[1], crate::ast::Condition::Truthy(..)));
    }
}

#[test]
fn parse_condition_and_with_negation() {
    let cond = parse_condition("a && !b").unwrap();
    assert!(matches!(cond, crate::ast::Condition::And(_)));
    if let crate::ast::Condition::And(ops) = cond {
        assert!(matches!(ops[1], crate::ast::Condition::Not(..)));
    }
}

#[test]
fn parse_condition_and_has_higher_precedence_than_or() {
    // `a && b || c` → Or([And([a, b]), c])
    let cond = parse_condition("a && b || c").unwrap();
    assert!(matches!(cond, crate::ast::Condition::Or(_)));
    if let crate::ast::Condition::Or(ops) = cond {
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], crate::ast::Condition::And(_)));
        assert!(matches!(ops[1], crate::ast::Condition::Truthy(..)));
    }
}

#[test]
fn parse_condition_string_with_operator_inside_quotes() {
    // The `||` inside the string should NOT be treated as a logical operator
    let cond = parse_condition("msg == \"a || b\"").unwrap();
    assert!(
        matches!(cond, crate::ast::Condition::Eq(..)),
        "operator inside string should not split condition"
    );
}

#[test]
fn parse_condition_complex_three_or() {
    let cond = parse_condition("a || b || c").unwrap();
    assert!(matches!(cond, crate::ast::Condition::Or(_)));
    if let crate::ast::Condition::Or(ops) = cond {
        assert_eq!(ops.len(), 3);
    }
}

#[test]
fn parse_condition_empty_operand_rejected() {
    let result = parse_condition("a && ");
    assert!(result.is_err(), "empty operand after && should fail");
}

#[test]
fn parse_condition_empty_or_operand_rejected() {
    let result = parse_condition("|| b");
    assert!(result.is_err(), "empty operand before || should fail");
}

#[test]
fn parse_condition_max_operands_exceeded_rejected() {
    // MAX_LOGICAL_OPERANDS is 16; 17 operands in a || chain should be rejected.
    let parts: Vec<String> = (0..17).map(|i| format!("v{i}")).collect();
    let src_condition = parts.join(" || ");
    let result = parse_condition(&src_condition);
    assert!(
        result.is_err(),
        "more than MAX_LOGICAL_OPERANDS operands must be rejected"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("operand") || err.contains("maximum"),
        "error should mention operand limit, got: {err}"
    );
}

// ── Logical operator integration tests ───────────────────────────────────────

#[test]
fn evaluate_and_condition_both_true() {
    let result =
        crate::compile_str("---\na: true\nb: true\n---\n@if a && b:\nyes\n@end\n").unwrap();
    assert!(
        result.contains("yes"),
        "and with both true should render body, got: {result}"
    );
}

#[test]
fn evaluate_and_condition_one_false() {
    let result =
        crate::compile_str("---\na: true\nb: false\n---\n@if a && b:\nyes\n@else:\nno\n@end\n")
            .unwrap();
    assert!(
        result.contains("no"),
        "and with one false should take else, got: {result}"
    );
}

#[test]
fn evaluate_or_condition_one_true() {
    let result =
        crate::compile_str("---\na: false\nb: true\n---\n@if a || b:\nyes\n@else:\nno\n@end\n")
            .unwrap();
    assert!(
        result.contains("yes"),
        "or with one true should render body, got: {result}"
    );
}

#[test]
fn evaluate_or_condition_both_false() {
    let result =
        crate::compile_str("---\na: false\nb: false\n---\n@if a || b:\nyes\n@else:\nno\n@end\n")
            .unwrap();
    assert!(
        result.contains("no"),
        "or with both false should take else, got: {result}"
    );
}

#[test]
fn evaluate_elseif_with_logical_and_operator() {
    // parse_condition is shared between @if and @elseif; verify logical operators
    // work correctly in @elseif branches (b && c evaluates to BC when a=false, b=true, c=true).
    let src =
        "---\na: false\nb: true\nc: true\n---\n@if a:\nA\n@elseif b && c:\nBC\n@else:\nNO\n@end\n";
    let result = crate::compile_str(src).unwrap();
    assert!(
        result.contains("BC"),
        "@elseif with && should render branch when both operands are true, got: {result}"
    );
    assert!(
        !result.contains("A"),
        "@if branch should not render when a is false, got: {result}"
    );
    assert!(
        !result.contains("NO"),
        "@else branch should not render when @elseif matches, got: {result}"
    );
}

#[test]
fn evaluate_elseif_with_logical_or_operator() {
    // Verify @elseif with || takes the branch when at least one operand is true.
    let src =
        "---\na: false\nb: false\nc: true\n---\n@if a:\nA\n@elseif b || c:\nBC\n@else:\nNO\n@end\n";
    let result = crate::compile_str(src).unwrap();
    assert!(
        result.contains("BC"),
        "@elseif with || should render branch when one operand is true, got: {result}"
    );
    assert!(
        !result.contains("NO"),
        "@else branch should not render when @elseif matches, got: {result}"
    );
}

// ── Expression directives: parser tests (new feature) ───────────────────────

#[test]
fn parse_if_call_truthy() {
    // @if func(x): → Condition::Truthy(Expr::Call)
    let src = "@if contains(tags, \"rust\"):\nyes\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::If(block) = &module.body[0] {
        assert!(
            matches!(&block.condition, Condition::Truthy(Expr::Call { name, .. }) if name == "contains"),
            "expected Condition::Truthy(Call{{contains}}), got {:?}",
            block.condition
        );
    } else {
        panic!("expected If node");
    }
}

#[test]
fn parse_if_not_call() {
    // @if !func(x): → Condition::Not(Expr::Call)
    let src = "@if !starts_with(name, \"z\"):\nyes\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::If(block) = &module.body[0] {
        assert!(
            matches!(&block.condition, Condition::Not(Expr::Call { name, .. }) if name == "starts_with"),
            "expected Condition::Not(Call{{starts_with}}), got {:?}",
            block.condition
        );
    } else {
        panic!("expected If node");
    }
}

#[test]
fn parse_if_call_eq_literal() {
    // @if func(x) == "val": → Eq(Expr::Call, Expr::StringLiteral)
    let src = "@if lower(name) == \"alice\":\nyes\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::If(block) = &module.body[0] {
        assert!(
            matches!(&block.condition, Condition::Eq(Expr::Call { name, .. }, Expr::StringLiteral(s))
                if name == "lower" && s == "alice"),
            "expected Condition::Eq(Call, StringLiteral), got {:?}",
            block.condition
        );
    } else {
        panic!("expected If node");
    }
}

#[test]
fn parse_if_call_eq_call() {
    // @if func(a) == func(b): → Eq(Expr::Call, Expr::Call)
    let src = "@if lower(a) == lower(b):\nyes\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::If(block) = &module.body[0] {
        assert!(
            matches!(&block.condition, Condition::Eq(Expr::Call { name: lhs, .. }, Expr::Call { name: rhs, .. })
                if lhs == "lower" && rhs == "lower"),
            "expected Condition::Eq(Call, Call), got {:?}",
            block.condition
        );
    } else {
        panic!("expected If node");
    }
}

#[test]
fn parse_if_and_with_calls() {
    // @if func(a) && func(b): → And with Truthy(Call) operands
    let src = "@if contains(t, \"r\") && contains(t, \"g\"):\nyes\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::If(block) = &module.body[0] {
        assert!(
            matches!(&block.condition, Condition::And(_)),
            "expected Condition::And, got {:?}",
            block.condition
        );
        if let Condition::And(ops) = &block.condition {
            assert_eq!(ops.len(), 2);
            assert!(matches!(&ops[0], Condition::Truthy(Expr::Call { .. })));
            assert!(matches!(&ops[1], Condition::Truthy(Expr::Call { .. })));
        }
    } else {
        panic!("expected If node");
    }
}

#[test]
fn parse_if_qualified_call_truthy() {
    // @if ns.func(x): → Truthy(QualifiedCall)
    let src = "@if utils.check(val):\nyes\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::If(block) = &module.body[0] {
        assert!(
            matches!(&block.condition, Condition::Truthy(Expr::QualifiedCall { namespace, name, .. })
                if namespace == "utils" && name == "check"),
            "expected Condition::Truthy(QualifiedCall), got {:?}",
            block.condition
        );
    } else {
        panic!("expected If node");
    }
}

#[test]
fn parse_for_call_iterable() {
    // @for x in func(args): → ForBlock with Expr::Call
    let src = "@for x in split(csv, \",\"):\n- {x}\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::For(block) = &module.body[0] {
        assert!(
            matches!(&block.iterable, Expr::Call { name, .. } if name == "split"),
            "expected Expr::Call{{split}}, got {:?}",
            block.iterable
        );
    } else {
        panic!("expected For node");
    }
}

#[test]
fn parse_for_nested_call_iterable() {
    // @for x in sort(unique(tags)): → nested calls
    let src = "@for x in sort(unique(tags)):\n- {x}\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::For(block) = &module.body[0] {
        if let Expr::Call { name, args } = &block.iterable {
            assert_eq!(name, "sort", "outer call should be 'sort'");
            assert_eq!(args.len(), 1, "sort() should have exactly one argument");
            if let Arg::Call {
                name: inner_name,
                args: inner_args,
            } = &args[0]
            {
                assert_eq!(inner_name, "unique", "inner call should be 'unique'");
                assert_eq!(
                    inner_args.len(),
                    1,
                    "unique() should have exactly one argument"
                );
                assert!(
                    matches!(&inner_args[0], Arg::Var(v) if v == "tags"),
                    "unique() argument should be Arg::Var(\"tags\"), got {:?}",
                    inner_args[0]
                );
            } else {
                panic!("expected inner Arg::Call{{unique}}, got {:?}", args[0]);
            }
        } else {
            panic!("expected Expr::Call{{sort}}, got {:?}", block.iterable);
        }
    } else {
        panic!("expected For node");
    }
}

#[test]
fn parse_if_colon_in_string_arg() {
    // @if contains(s, "a:b"): — colon inside string arg must not corrupt directive parsing
    let src = "@if contains(s, \"a:b\"):\nyes\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_ok(),
        "colon inside string arg should not corrupt directive parsing: {result:?}"
    );
    if let Ok(module) = result {
        if let Node::If(block) = &module.body[0] {
            assert!(
                matches!(&block.condition, Condition::Truthy(Expr::Call { name, .. }) if name == "contains"),
                "expected Truthy(Call{{contains}}), got {:?}",
                block.condition
            );
        } else {
            panic!("expected If node");
        }
    }
}

#[test]
fn parse_for_colon_as_separator() {
    // @for x in split(s, ":"): — colon as argument must not corrupt directive parsing
    let src = "@for x in split(s, \":\"):\n- {x}\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_ok(),
        "colon as separator arg should parse correctly: {result:?}"
    );
    if let Ok(module) = result {
        if let Node::For(block) = &module.body[0] {
            assert!(
                matches!(&block.iterable, Expr::Call { name, .. } if name == "split"),
                "expected Expr::Call{{split}}, got {:?}",
                block.iterable
            );
        } else {
            panic!("expected For node");
        }
    }
}

#[test]
fn parse_if_bare_literal_rejected() {
    // @if true: → parse error "use a variable or function call"
    let src = "@if true:\nyes\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(result.is_err(), "@if true: should be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("literal"),
        "error should mention literal, got: {err}"
    );
}

#[test]
fn parse_if_string_literal_truthy_rejected() {
    // @if "literal": → parse error
    let src = "@if \"hello\":\nyes\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(result.is_err(), "@if \"literal\": should be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("literal"),
        "error should mention literal, got: {err}"
    );
}

#[test]
fn parse_for_literal_iterable_rejected() {
    // @for x in "literal": → parse error
    let src = "@for x in \"items\":\n- {x}\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(result.is_err(), "@for x in \"literal\": should be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("literal"),
        "error should mention literal, got: {err}"
    );
}

#[test]
fn parse_if_negation_combined_with_comparison_rejected() {
    // @if !func(x) == "v": → parse error "cannot combine negation"
    let src = "@if !lower(name) == \"alice\":\nyes\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_err(),
        "@if !func == val: should be rejected (negation + comparison)"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("negation") || err.contains("comparison"),
        "error should mention negation/comparison, got: {err}"
    );
}

#[test]
fn parse_elseif_call_condition() {
    // @elseif func(x) == "v": → Condition with Eq
    let src = "@if a:\nA\n@elseif lower(b) == \"val\":\nB\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::If(block) = &module.body[0] {
        assert_eq!(block.elseif_branches.len(), 1);
        let (cond, _) = &block.elseif_branches[0];
        assert!(
            matches!(cond, Condition::Eq(Expr::Call { name, .. }, Expr::StringLiteral(s))
                if name == "lower" && s == "val"),
            "expected Eq(Call, StringLiteral), got {:?}",
            cond
        );
    } else {
        panic!("expected If node");
    }
}

// ── Backward compatibility parser tests ──────────────────────────────────────

#[test]
fn parse_backward_compat_if_var_truthy() {
    // @if active: → Truthy(Expr::Var("active"))
    let src = "@if active:\nyes\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::If(block) = &module.body[0] {
        assert!(
            matches!(&block.condition, Condition::Truthy(Expr::Var(v)) if v == "active"),
            "expected Truthy(Var(active)), got {:?}",
            block.condition
        );
    } else {
        panic!("expected If node");
    }
}

#[test]
fn parse_backward_compat_if_var_eq_string() {
    // @if role == "admin": → Eq(Expr::Var, Expr::StringLiteral)
    let src = "@if role == \"admin\":\nyes\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::If(block) = &module.body[0] {
        assert!(
            matches!(&block.condition, Condition::Eq(Expr::Var(v), Expr::StringLiteral(s))
                if v == "role" && s == "admin"),
            "expected Eq(Var(role), StringLiteral(admin)), got {:?}",
            block.condition
        );
    } else {
        panic!("expected If node");
    }
}

#[test]
fn parse_backward_compat_for_var_iterable() {
    // @for x in items: → ForBlock with Expr::Var
    let src = "@for x in items:\n- {x}\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::For(block) = &module.body[0] {
        assert!(
            matches!(&block.iterable, Expr::Var(v) if v == "items"),
            "expected Expr::Var(items), got {:?}",
            block.iterable
        );
    } else {
        panic!("expected For node");
    }
}

// ── Evaluator integration tests for expression directives ────────────────────

#[test]
fn evaluate_if_call_truthy_contains() {
    let result = crate::compile_str(
        "---\ntags:\n  - rust\n  - go\n---\n@if contains(tags, \"rust\"):\nyes\n@else:\nno\n@end\n",
    )
    .unwrap();
    assert!(
        result.contains("yes"),
        "@if contains(tags, \"rust\") with rust in tags should be truthy, got: {result}"
    );
}

#[test]
fn evaluate_if_not_call() {
    let result = crate::compile_str(
        "---\nname: abc\n---\n@if !starts_with(name, \"z\"):\nyes\n@else:\nno\n@end\n",
    )
    .unwrap();
    assert!(
        result.contains("yes"),
        "@if !starts_with should be truthy when name doesn't start with z, got: {result}"
    );
}

#[test]
fn evaluate_if_lower_eq_literal() {
    let result = crate::compile_str(
        "---\nname: Alice\n---\n@if lower(name) == \"alice\":\nyes\n@else:\nno\n@end\n",
    )
    .unwrap();
    assert!(
        result.contains("yes"),
        "@if lower(name) == \"alice\" should match, got: {result}"
    );
}

#[test]
fn evaluate_if_call_eq_call_match() {
    let result = crate::compile_str(
        "---\na: Alice\nb: ALICE\n---\n@if lower(a) == lower(b):\nyes\n@else:\nno\n@end\n",
    )
    .unwrap();
    assert!(
        result.contains("yes"),
        "@if lower(a) == lower(b) should match when both lowercase to same, got: {result}"
    );
}

#[test]
fn evaluate_if_call_eq_call_no_match() {
    let result = crate::compile_str(
        "---\na: Alice\nb: Bob\n---\n@if lower(a) == lower(b):\nyes\n@else:\nno\n@end\n",
    )
    .unwrap();
    assert!(
        result.contains("no"),
        "@if lower(a) == lower(b) should not match when different, got: {result}"
    );
}

#[test]
fn evaluate_if_and_with_calls() {
    let result = crate::compile_str(
        "---\nt: grunge\n---\n@if contains(t, \"r\") && contains(t, \"g\"):\nyes\n@else:\nno\n@end\n",
    )
    .unwrap();
    assert!(
        result.contains("yes"),
        "@if contains && contains should be truthy when both true, got: {result}"
    );
}

#[test]
fn evaluate_for_split_iterable() {
    let result =
        crate::compile_str("---\ncsv: \"a,b,c\"\n---\n@for x in split(csv, \",\"):\n- {x}\n@end\n")
            .unwrap();
    assert!(
        result.contains("- a") && result.contains("- b") && result.contains("- c"),
        "@for split iterable should iterate over parts, got: {result}"
    );
}

#[test]
fn evaluate_for_sort_unique_iterable() {
    let result = crate::compile_str(
        "---\ntags:\n  - b\n  - a\n  - b\n---\n@for t in sort(unique(tags)):\n- {t}\n@end\n",
    )
    .unwrap();
    // Ensure deduplication — only 2 items, not 3
    let dashes: Vec<_> = result.lines().filter(|l| l.starts_with("- ")).collect();
    assert_eq!(
        dashes.len(),
        2,
        "should have exactly 2 unique items, got: {result}"
    );
    // Ensure sort order — ascending lexicographic: a before b
    assert_eq!(
        dashes[0], "- a",
        "first item should be '- a' (sorted), got: {result}"
    );
    assert_eq!(
        dashes[1], "- b",
        "second item should be '- b' (sorted), got: {result}"
    );
}

#[test]
fn evaluate_for_non_array_result_is_error() {
    let result = crate::compile_str("---\nname: Alice\n---\n@for x in upper(name):\n- {x}\n@end\n");
    assert!(
        result.is_err(),
        "non-array result from @for expression should error"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("array") || err.contains("iterate"),
        "error should mention array type mismatch, got: {err}"
    );
}

#[test]
fn evaluate_if_undefined_function_is_error() {
    let result = crate::compile_str("@if notabuiltin(x):\nyes\n@end\n");
    assert!(result.is_err(), "undefined function in @if should error");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("notabuiltin") || err.contains("undefined"),
        "error should mention the unknown function name, got: {err}"
    );
}

#[test]
fn evaluate_elseif_with_expression() {
    let result = crate::compile_str(
        "---\nname: Alice\n---\n@if lower(name) == \"bob\":\nBob\n@elseif lower(name) == \"alice\":\nAlice\n@else:\nOther\n@end\n",
    )
    .unwrap();
    assert!(
        result.contains("Alice"),
        "@elseif with expression should work, got: {result}"
    );
}

// ── NotEq operator tests ──────────────────────────────────────────────────────

#[test]
fn parse_if_call_not_eq_literal() {
    // @if lower(name) != "alice": → NotEq(Expr::Call, Expr::StringLiteral)
    let src = "@if lower(name) != \"alice\":\nyes\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::If(block) = &module.body[0] {
        assert!(
            matches!(&block.condition, Condition::NotEq(Expr::Call { name, .. }, Expr::StringLiteral(s))
                if name == "lower" && s == "alice"),
            "expected Condition::NotEq(Call{{lower}}, StringLiteral(alice)), got {:?}",
            block.condition
        );
    } else {
        panic!("expected If node");
    }
}

#[test]
fn evaluate_if_call_not_eq_truthy() {
    // @if lower(name) != "bob": with name:Alice → truthy branch taken
    let result = crate::compile_str(
        "---\nname: Alice\n---\n@if lower(name) != \"bob\":\nyes\n@else:\nno\n@end\n",
    )
    .unwrap();
    assert!(
        result.contains("yes"),
        "@if lower(name) != \"bob\" should be truthy when name is Alice, got: {result}"
    );
}

#[test]
fn evaluate_if_call_not_eq_falsy() {
    // @if lower(name) != "alice": with name:Alice → false branch taken
    let result = crate::compile_str(
        "---\nname: Alice\n---\n@if lower(name) != \"alice\":\nyes\n@else:\nno\n@end\n",
    )
    .unwrap();
    assert!(
        result.contains("no"),
        "@if lower(name) != \"alice\" should be falsy when name is Alice, got: {result}"
    );
}

// ── OR operator with expression operands ──────────────────────────────────────

#[test]
fn parse_if_or_with_calls() {
    // @if contains(t, "r") || contains(t, "z"): → Or with Truthy(Call) operands
    let src = "@if contains(t, \"r\") || contains(t, \"z\"):\nyes\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::If(block) = &module.body[0] {
        assert!(
            matches!(&block.condition, Condition::Or(_)),
            "expected Condition::Or, got {:?}",
            block.condition
        );
        if let Condition::Or(ops) = &block.condition {
            assert_eq!(ops.len(), 2);
            assert!(matches!(&ops[0], Condition::Truthy(Expr::Call { .. })));
            assert!(matches!(&ops[1], Condition::Truthy(Expr::Call { .. })));
        }
    } else {
        panic!("expected If node");
    }
}

#[test]
fn evaluate_if_or_with_calls() {
    // first operand is false, second is true → truthy
    let result = crate::compile_str(
        "---\nt: grunge\n---\n@if contains(t, \"z\") || contains(t, \"g\"):\nyes\n@else:\nno\n@end\n",
    )
    .unwrap();
    assert!(
        result.contains("yes"),
        "@if contains(t,z) || contains(t,g) should be truthy when second matches, got: {result}"
    );
}

// ── @for with QualifiedCall iterable ─────────────────────────────────────────

#[test]
fn parse_for_qualified_call_iterable() {
    // @for x in ns.func(args): → ForBlock with Expr::QualifiedCall
    let src = "@for x in utils.items(config):\n- {x}\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::For(block) = &module.body[0] {
        assert!(
            matches!(&block.iterable, Expr::QualifiedCall { namespace, name, .. }
                if namespace == "utils" && name == "items"),
            "expected Expr::QualifiedCall{{utils.items}}, got {:?}",
            block.iterable
        );
    } else {
        panic!("expected For node");
    }
}

// ── @elseif unterminated string error ────────────────────────────────────────

#[test]
fn parse_elseif_unterminated_string_error() {
    // @elseif with unterminated string should give targeted error, not generic "must end with ':'"
    let src = "@if x:\nA\n@elseif lower(name) == \"alice:\nB\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_err(),
        "unterminated string in @elseif should error"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("unterminated string"),
        "error should mention unterminated string, got: {err}"
    );
}

// ── @for unterminated string error ───────────────────────────────────────────

#[test]
fn parse_for_unterminated_string_error() {
    // @for with unterminated string arg should give targeted error, not generic "must end with ':'"
    let src = "@for x in split(s, \"alice:\n- {x}\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(result.is_err(), "unterminated string in @for should error");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("unterminated string"),
        "error should mention unterminated string, got: {err}"
    );
}

// ── Security tests ────────────────────────────────────────────────────────────

#[test]
fn split_resource_limit_too_many_elements() {
    // split() producing more than MAX_ARRAY_ELEMENTS should be rejected.
    use crate::limits::MAX_ARRAY_ELEMENTS;
    // Create a string with MAX_ARRAY_ELEMENTS+1 commas → MAX_ARRAY_ELEMENTS+2 parts
    let big_input: String = std::iter::repeat_n("x,", MAX_ARRAY_ELEMENTS + 1).collect();
    let result = crate::builtins::call_builtin(
        "split",
        &[
            crate::value::Value::String(big_input),
            crate::value::Value::String(",".to_string()),
        ],
    );
    assert!(
        result.is_err(),
        "split() producing > MAX_ARRAY_ELEMENTS elements must be rejected"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("element") || err.contains("maximum") || err.contains("limit"),
        "error should mention element limit, got: {err}"
    );
}

#[test]
fn join_resource_limit_output_too_large() {
    // join() producing output > MAX_OUTPUT_SIZE (50 MB) should be rejected.
    use crate::limits::MAX_OUTPUT_SIZE;
    let big_element = "a".repeat(1024); // 1 KB per element
    let item_count = (MAX_OUTPUT_SIZE / 1024) + 100; // just over 50K elements
    let items: Vec<crate::value::Value> = (0..item_count)
        .map(|_| crate::value::Value::String(big_element.clone()))
        .collect();
    let arr = crate::value::Value::Array(items);
    let sep = crate::value::Value::String(",".to_string());
    let result = crate::builtins::call_builtin("join", &[arr, sep]);
    assert!(
        result.is_err(),
        "join() producing > MAX_OUTPUT_SIZE must be rejected"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("maximum size") || err.contains("output"),
        "error should mention output size limit, got: {err}"
    );
}

// ── Escape-aware string literal boundary detection ────────────────────────
//
// Fix: rust-HIGH-parser_helpers:146 — parse_expr_inner, parse_cond_value, and
// parse_single_arg_inner previously accepted `"\"` as a complete string literal
// with value `\`. The closing quote in `"\"` is preceded by an odd number of
// backslashes, making it an escaped quote — the string is unterminated.

#[test]
fn parse_expr_inner_escaped_closing_quote_is_unterminated() {
    // `"\"`  — quote, backslash, quote: closing quote is escaped → unterminated
    let result = parse_expr_inner(r#""\""#);
    assert!(
        result.is_err(),
        "escaped closing quote must not be accepted as a complete string literal, got {result:?}"
    );
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("unterminated"),
        "error must mention unterminated string, got: {msg}"
    );
}

#[test]
fn parse_expr_inner_double_backslash_then_quote_is_complete() {
    // `"\\"` — 4 bytes: open-quote, backslash, backslash, close-quote.
    // The two backslashes form a `\\` escape, so the closing quote is unescaped → complete.
    // Inner content is `\\`, unescape → `\`.
    // Note: in a Rust string literal, `"\"\\\\\""` produces the 4 bytes `"\\"`
    let s = "\"\\\\\"";
    let result = parse_expr_inner(s);
    assert!(
        result.is_ok(),
        "double-backslash-terminated string must parse as complete: {result:?}"
    );
    if let Ok(Expr::StringLiteral(v)) = result {
        assert_eq!(v, "\\", "inner `\\\\` must unescape to a single backslash");
    } else {
        panic!("expected StringLiteral");
    }
}

#[test]
fn parse_expr_inner_escaped_quote_in_middle_followed_by_close() {
    // `"say \"hi\""` — the inner `\"` is an escaped quote, the final `"` is the real close
    let result = parse_expr_inner(r#""say \"hi\"""#);
    assert!(
        result.is_ok(),
        "string with escaped inner quote must parse: {result:?}"
    );
    if let Ok(Expr::StringLiteral(s)) = result {
        assert_eq!(s, r#"say "hi""#);
    } else {
        panic!("expected StringLiteral");
    }
}

#[test]
fn parse_cond_value_escaped_closing_quote_is_unterminated() {
    // `"\"` in a condition value context — must be rejected as unterminated
    let result = parse_cond_value(r#""\""#);
    assert!(
        result.is_err(),
        "escaped closing quote must not be accepted as a complete condition string: {result:?}"
    );
}

#[test]
fn parse_single_arg_escaped_closing_quote_is_unterminated() {
    // `"\"` as a function argument — must not silently parse as string literal `\`
    let result = parse_single_arg(r#""\""#);
    assert!(
        result.is_err(),
        "escaped closing quote must not be accepted as a complete arg string: {result:?}"
    );
}
