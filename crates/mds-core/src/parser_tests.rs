//! Parser unit and integration tests, extracted from parser.rs.

use super::helpers::*;
use super::*;
use crate::ast::{Arg, ExportDirective, Expr, ImportDirective};
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
    if let Ok(Expr::StringLiteral(s)) = result {
        assert_eq!(s, r#"say "hi""#, "escaped quote must be unescaped");
    } else {
        panic!("expected Expr::StringLiteral");
    }
}

#[test]
fn condition_value_unescaped_string_unchanged() {
    // Plain strings with no escapes must pass through unchanged
    let result = parse_cond_value(r#""hello world""#);
    assert!(result.is_ok());
    if let Ok(Expr::StringLiteral(s)) = result {
        assert_eq!(s, "hello world");
    } else {
        panic!("expected Expr::StringLiteral");
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
            Some(Expr::StringLiteral(s)) if s == "World"
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
        assert!(matches!(&def.params[0].default, Some(Expr::NumberLiteral(n)) if *n == 3.0));
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
        assert!(matches!(&def.params[0].default, Some(Expr::NumberLiteral(n)) if *n == -1.0));
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
            Some(Expr::BooleanLiteral(true))
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
        assert!(matches!(&def.params[0].default, Some(Expr::NullLiteral)));
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
            Some(Expr::StringLiteral(s)) if s == "a, b"
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

// ── #78 literal-only default guard ────────────────────────────────────────────
//
// `Param.default` is typed `Option<Expr>`, which *could* represent a function
// call, a variable reference, or a member access. The parser must keep rejecting
// every non-literal default exactly as it did before the type unification —
// only string/number/boolean/null literals are admissible. These tests pin that
// guard and the preserved rejection error so the type unification stays
// zero-behaviour-change (AC-2).

/// The exact rejection diagnostic emitted by `parse_define_params` for any
/// non-literal (or otherwise invalid) default value. Centralised so every guard
/// test asserts the identical preserved string.
const NON_LITERAL_DEFAULT_ERR: &str = "must be a string, number, boolean, or null";

#[test]
fn parse_define_default_function_call_rejected() {
    // `@define f(x = upper("a"))` — a function-call Expr in default position.
    // The type can now represent it, but the parser must still reject it.
    let src = "@define f(x = upper(\"a\")):\n{x}\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_err(),
        "a function-call default must be rejected, got: {result:?}"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains(NON_LITERAL_DEFAULT_ERR),
        "function-call default must produce the preserved literal-only error, got: {err}"
    );
}

#[test]
fn parse_define_default_variable_reference_rejected() {
    // `@define f(x = y)` — a bare identifier (variable reference) is not a literal.
    let src = "@define f(x = y):\n{x}\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_err(),
        "a variable-reference default must be rejected, got: {result:?}"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains(NON_LITERAL_DEFAULT_ERR),
        "variable-reference default must produce the preserved literal-only error, got: {err}"
    );
}

#[test]
fn parse_define_default_member_access_rejected() {
    // `@define f(x = config.key)` — a member-access Expr is not a literal.
    let src = "@define f(x = config.key):\n{x}\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_err(),
        "a member-access default must be rejected, got: {result:?}"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains(NON_LITERAL_DEFAULT_ERR),
        "member-access default must produce the preserved literal-only error, got: {err}"
    );
}

#[test]
fn parse_define_params_non_literal_default_rejected_directly() {
    // Same guard at the `parse_define_params` level (no full-pipeline wrapping):
    // every non-literal default form returns the preserved rejection error and
    // never yields a non-literal `Expr` in `Param.default`.
    for rhs in ["upper(\"a\")", "y", "config.key", "ns.f(1)"] {
        let token = format!("x = {rhs}");
        let result = parse_define_params(&token, "f");
        assert!(
            result.is_err(),
            "non-literal default `{rhs}` must be rejected, got: {result:?}"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains(NON_LITERAL_DEFAULT_ERR),
            "non-literal default `{rhs}` must produce the preserved error, got: {err}"
        );
    }
}

#[test]
fn parse_cond_value_only_admits_literals() {
    // Direct proof that the default-value parser never constructs a non-literal
    // Expr variant: literals succeed; calls/vars/member-access fall through to the
    // preserved trailing error.
    assert!(matches!(
        parse_cond_value("\"hi\""),
        Ok(Expr::StringLiteral(_))
    ));
    assert!(matches!(parse_cond_value("42"), Ok(Expr::NumberLiteral(_))));
    assert!(matches!(
        parse_cond_value("true"),
        Ok(Expr::BooleanLiteral(true))
    ));
    assert!(matches!(parse_cond_value("null"), Ok(Expr::NullLiteral)));
    for non_literal in ["upper(\"a\")", "y", "config.key"] {
        let result = parse_cond_value(non_literal);
        assert!(
            result.is_err(),
            "`{non_literal}` must not parse as a default literal, got: {result:?}"
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("comparison values must be string, number, boolean, or null"),
            "`{non_literal}` must hit the preserved trailing literal-only error"
        );
    }
}

// ── #78 literal-default → Value parity ─────────────────────────────────────────
//
// `literal_expr_to_value` (evaluator) replaced `condvalue_to_value`. These tests
// pin that each literal default `Expr` produces exactly the `Value` the old
// conversion produced, end-to-end through `compile_str`.

#[test]
fn default_string_value_parity() {
    // Expr::StringLiteral → Value::String, rendered verbatim.
    let out = crate::compile_str_md("@define f(x = \"hi\"):\n{x}\n@end\n{f()}\n").unwrap();
    assert_eq!(out, "hi\n");
}

#[test]
fn default_number_value_parity() {
    // Expr::NumberLiteral → Value::Number; integral floats render without a decimal.
    let out = crate::compile_str_md("@define f(x = 42):\n{x}\n@end\n{f()}\n").unwrap();
    assert_eq!(out, "42\n");
    let out_neg = crate::compile_str_md("@define f(x = -1):\n{x}\n@end\n{f()}\n").unwrap();
    assert_eq!(out_neg, "-1\n");
    let out_frac = crate::compile_str_md("@define f(x = 3.14):\n{x}\n@end\n{f()}\n").unwrap();
    assert_eq!(out_frac, "3.14\n");
}

#[test]
fn default_boolean_value_parity() {
    // Expr::BooleanLiteral → Value::Boolean, rendered as `true`/`false`.
    let out_true = crate::compile_str_md("@define f(x = true):\n{x}\n@end\n{f()}\n").unwrap();
    assert_eq!(out_true, "true\n");
    let out_false = crate::compile_str_md("@define f(x = false):\n{x}\n@end\n{f()}\n").unwrap();
    assert_eq!(out_false, "false\n");
}

#[test]
fn default_null_value_parity() {
    // Expr::NullLiteral → Value::Null, rendered as the empty string in interpolation.
    let out = crate::compile_str_md("@define f(x = null):\n[{x}]\n@end\n{f()}\n").unwrap();
    assert_eq!(out, "[]\n");
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
        crate::compile_str_md("---\na: true\nb: true\n---\n@if a && b:\nyes\n@end\n").unwrap();
    assert!(
        result.contains("yes"),
        "and with both true should render body, got: {result}"
    );
}

#[test]
fn evaluate_and_condition_one_false() {
    let result =
        crate::compile_str_md("---\na: true\nb: false\n---\n@if a && b:\nyes\n@else:\nno\n@end\n")
            .unwrap();
    assert!(
        result.contains("no"),
        "and with one false should take else, got: {result}"
    );
}

#[test]
fn evaluate_or_condition_one_true() {
    let result =
        crate::compile_str_md("---\na: false\nb: true\n---\n@if a || b:\nyes\n@else:\nno\n@end\n")
            .unwrap();
    assert!(
        result.contains("yes"),
        "or with one true should render body, got: {result}"
    );
}

#[test]
fn evaluate_or_condition_both_false() {
    let result =
        crate::compile_str_md("---\na: false\nb: false\n---\n@if a || b:\nyes\n@else:\nno\n@end\n")
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
    let result = crate::compile_str_md(src).unwrap();
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
    let result = crate::compile_str_md(src).unwrap();
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
    let result = crate::compile_str_md(
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
    let result = crate::compile_str_md(
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
    let result = crate::compile_str_md(
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
    let result = crate::compile_str_md(
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
    let result = crate::compile_str_md(
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
    let result = crate::compile_str_md(
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
    let result = crate::compile_str_md(
        "---\ncsv: \"a,b,c\"\n---\n@for x in split(csv, \",\"):\n- {x}\n@end\n",
    )
    .unwrap();
    assert!(
        result.contains("- a") && result.contains("- b") && result.contains("- c"),
        "@for split iterable should iterate over parts, got: {result}"
    );
}

#[test]
fn evaluate_for_sort_unique_iterable() {
    let result = crate::compile_str_md(
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
    let result =
        crate::compile_str_md("---\nname: Alice\n---\n@for x in upper(name):\n- {x}\n@end\n");
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
    let result = crate::compile_str_md("@if notabuiltin(x):\nyes\n@end\n");
    assert!(result.is_err(), "undefined function in @if should error");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("notabuiltin") || err.contains("undefined"),
        "error should mention the unknown function name, got: {err}"
    );
}

#[test]
fn evaluate_elseif_with_expression() {
    let result = crate::compile_str_md(
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
    let result = crate::compile_str_md(
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
    let result = crate::compile_str_md(
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
    let result = crate::compile_str_md(
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

// ── Phase 1: @extends / @block tests ──────────────────────────────────────────

#[test]
fn parse_block_standalone_basic() {
    // A standalone @block with a default body should parse to Node::Block.
    let src = "@block instructions:\nDo something useful.\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    assert_eq!(module.body.len(), 1);
    if let Node::Block(b) = &module.body[0] {
        assert_eq!(b.name, "instructions");
        assert!(!b.body.is_empty(), "block body should not be empty");
    } else {
        panic!("expected Block node, got {:?}", module.body[0]);
    }
}

#[test]
fn parse_block_empty_body() {
    // @block with no body (just @end) is valid — default is empty.
    let src = "@block tools:\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    assert_eq!(module.body.len(), 1);
    assert!(matches!(module.body[0], Node::Block(_)));
}

#[test]
fn parse_block_body_edge_newlines_stripped() {
    // Leading/trailing single newline stripped like @message/@define (decision #9).
    // Input: @block intro:\n<body-starts-here-after-newline>\nHello world.\n\n@end\n
    // strip_leading_newline removes the first \n; strip_trailing_newline removes the last \n.
    // Result: text = "\nHello world.\n" — inner blank line preserved, outermost \n stripped.
    let src = "@block intro:\nHello world.\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    if let Node::Block(b) = &module.body[0] {
        assert!(!b.body.is_empty(), "block body should not be empty");
        // strip_leading_newline strips the newline immediately after the colon.
        // strip_trailing_newline strips the newline before @end.
        // Resulting text should be exactly "Hello world." with no surrounding newlines.
        if let Node::Text(t) = &b.body[0] {
            assert_eq!(
                t.text, "Hello world.",
                "block body text should have leading/trailing newlines stripped"
            );
        } else {
            panic!("expected Text node in block body");
        }
    } else {
        panic!("expected Block node");
    }
}

#[test]
fn parse_block_missing_colon_rejected() {
    // @block without trailing colon → syntax error.
    let src = "@block instructions\nsome body\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(result.is_err(), "missing colon must be a syntax error");
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("@block"), "error should mention @block: {msg}");
}

#[test]
fn parse_block_invalid_identifier_rejected() {
    // @block with a non-identifier name → syntax error.
    let src = "@block 123bad:\nbody\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(result.is_err(), "invalid identifier must be a syntax error");
}

#[test]
fn parse_block_empty_name_rejected() {
    // @block with empty name (just whitespace before colon) → syntax error.
    let src = "@block :\nbody\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(result.is_err(), "empty block name must be a syntax error");
}

#[test]
fn parse_block_nested_inside_block_rejected() {
    // Nesting @block inside another @block → syntax error.
    let src = "@block outer:\n@block inner:\nbody\n@end\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(result.is_err(), "nested @block must be rejected");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("@block") || msg.contains("nested") || msg.contains("top-level"),
        "error should explain nesting restriction: {msg}"
    );
}

#[test]
fn parse_block_nested_inside_if_rejected() {
    // @block inside @if → syntax error (top-level only).
    let src = "@if flag:\n@block instructions:\nbody\n@end\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(result.is_err(), "@block inside @if must be rejected");
}

#[test]
fn parse_block_nested_inside_for_rejected() {
    // @block inside @for → syntax error (top-level only).
    let src = "@for x in items:\n@block b:\nbody\n@end\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(result.is_err(), "@block inside @for must be rejected");
}

#[test]
fn parse_block_nested_inside_message_rejected() {
    // @block inside @message → syntax error (top-level only).
    let src = "@message system:\n@block b:\nbody\n@end\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(result.is_err(), "@block inside @message must be rejected");
}

#[test]
fn parse_block_nested_inside_define_rejected() {
    // @block inside @define → syntax error (top-level only).
    let src = "@define foo():\n@block b:\nbody\n@end\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(result.is_err(), "@block inside @define must be rejected");
}

#[test]
fn parse_extends_basic() {
    // @extends "path" should parse into Module.extends with correct path and offset.
    let src = "@extends \"./base.mds\"\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    assert!(
        module.extends.is_some(),
        "module.extends should be Some after @extends directive"
    );
    let ext = module.extends.unwrap();
    assert_eq!(ext.path, "./base.mds");
    // The offset should be 0 — @extends is the first token.
    assert_eq!(ext.offset, 0, "offset of leading @extends should be 0");
}

#[test]
fn parse_extends_after_frontmatter() {
    // @extends after frontmatter is valid (it's the first directive after FM).
    let src = "---\nrole: assistant\n---\n@extends \"./base.mds\"\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    assert!(module.extends.is_some());
    assert_eq!(module.extends.unwrap().path, "./base.mds");
}

#[test]
fn parse_extends_after_blank_line_ok() {
    // A blank-line Text node before @extends should still count as "first directive after FM".
    let src = "\n@extends \"./base.mds\"\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    assert!(module.extends.is_some());
}

#[test]
fn parse_extends_sets_module_extends_none_for_standalone() {
    // A module with no @extends should have extends: None.
    let src = "Hello world!\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let module = parse_with_ctx(&tokens, "", "").unwrap();
    assert!(
        module.extends.is_none(),
        "standalone module must have extends = None"
    );
}

#[test]
fn parse_extends_stray_not_first_directive_rejected() {
    // E1: @extends appearing after a non-whitespace node → mds::extends (not mds::syntax).
    let src = "Some text.\n@extends \"./base.mds\"\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    let err = result.expect_err("stray @extends must be rejected");
    let serialized = err.serialize();
    assert_eq!(
        serialized.code, "mds::extends",
        "E1: stray @extends must map to mds::extends, got: {}",
        serialized.code
    );
    assert!(
        err.to_string().contains("first") || err.to_string().contains("@extends"),
        "error should explain placement rule: {}",
        err
    );
}

#[test]
fn parse_extends_duplicate_rejected() {
    // E2: Two @extends directives → mds::extends (not mds::syntax).
    let src = "@extends \"./a.mds\"\n@extends \"./b.mds\"\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    let err = result.expect_err("duplicate @extends must be rejected");
    let serialized = err.serialize();
    assert_eq!(
        serialized.code, "mds::extends",
        "E2: duplicate @extends must map to mds::extends, got: {}",
        serialized.code
    );
}

#[test]
fn parse_extends_missing_path_rejected() {
    // @extends without a quoted path → syntax error.
    let src = "@extends\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(result.is_err(), "@extends without path must be rejected");
}

#[test]
fn parse_extends_unquoted_path_rejected() {
    // @extends with an unquoted path → syntax error.
    let src = "@extends ./base.mds\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(result.is_err(), "unquoted path must be rejected");
}

// ── PR-E #72: Red-first edge-case tests for the 8 scanners ──────────────────
//
// These tests document the exact existing behaviour of each scanner across the
// edge-case matrix required by the PR-E Test Plan.  They are written BEFORE
// the byte-level scan primitive is extracted so that they serve as a red-first
// regression net: they must pass against the original code and must still pass
// after the refactor.
//
// Scanner families:
//   Byte-level (WITH paren tracking): has_bare_equals, strip_trailing_directive_colon,
//     find_unquoted_operator, split_on_unquoted_op
//   Byte-level (NO paren tracking):   has_unterminated_string, find_unquoted_equals
//   Char-level  (WITH paren tracking): parse_args_inner (via parse_args)
//   Char-level  (NO paren tracking):  split_on_unquoted_commas (via parse_define_params)

// ─── has_bare_equals ──────────────────────────────────────────────────────────

#[test]
fn has_bare_equals_mixed_single_double_quoting() {
    // `a == "it's"` — single quote inside double-quoted string: not a bare =
    let src = "@if a == \"it's\":\nok\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_ok(),
        "single-quote inside double-quoted string must not confuse scanner: {result:?}"
    );
}

#[test]
fn has_bare_equals_double_inside_single() {
    // `a == 'say "hi"'` — double quote inside single-quoted string: not a bare =
    let src = "@if a == 'say \"hi\"':\nok\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_ok(),
        "double-quote inside single-quoted string must not confuse scanner: {result:?}"
    );
}

#[test]
fn has_bare_equals_escaped_quote_in_string() {
    // `a == "say \"hi\""` — escaped double-quote inside string must not prematurely close string
    let src = "@if a == \"say \\\"hi\\\"\":\nok\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_ok(),
        "escaped quote inside string must not confuse scanner: {result:?}"
    );
}

#[test]
fn has_bare_equals_paren_depth_suppresses_detection() {
    // `func(a = 1)` — bare = inside parens must NOT trigger has_bare_equals
    // This appears in an @if truthy-check position: @if func(a=1):
    // has_bare_equals is only called when there is no operator (==, !=) found first.
    // func(a=1) has no == or !=, so parse_simple_condition falls through to has_bare_equals.
    // But the = is inside parens, so has_bare_equals must return false → it falls through
    // to parse_expr_inner which rejects it as invalid expression.
    let src = "@if func(a=1):\nok\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    // This is an error, but NOT because of bare_equals — the = is paren-protected.
    // The error is from parse_expr_inner or the function call parsing.
    // We just verify it's a syntax error (not a bare-equals hint).
    assert!(
        result.is_err(),
        "func(a=1) in @if is invalid syntax: {result:?}"
    );
    let msg = result.unwrap_err().to_string();
    // Must NOT say "use '==' for comparison, not '='" — that hint only fires for unprotected bare =
    assert!(
        !msg.contains("use '=='"),
        "paren-protected = must not trigger has_bare_equals hint, got: {msg}"
    );
}

#[test]
fn has_bare_equals_unbalanced_paren_outside_string() {
    // `a == b)` — unbalanced closing paren outside a string: should parse the == fine
    // (paren depth saturates to 0 on excess `)`)
    let result = find_unquoted_operator("a == b)");
    assert!(
        result.is_some(),
        "== must be found even with unbalanced closing paren: {result:?}"
    );
    let (_, op) = result.unwrap();
    assert_eq!(op, "==");
}

#[test]
fn has_bare_equals_operator_inside_string_not_found() {
    // `x == "a==b"` — == inside a string must not shadow the outer ==
    let result = find_unquoted_operator(r#"x == "a==b""#);
    assert!(
        result.is_some(),
        "outer == must be found when inner == is inside a string"
    );
    let (pos, op) = result.unwrap();
    assert_eq!(op, "==");
    assert_eq!(pos, 2, "outer == must be at byte 2");
}

#[test]
fn has_bare_equals_multibyte_utf8_adjacent_to_operator() {
    // `café == "ok"` — multibyte UTF-8 before the operator must not confuse byte scanning
    // `café` is 5 bytes in UTF-8 (c-a-f-é where é is 2 bytes), so == is at byte 6
    let result = find_unquoted_operator("café == \"ok\"");
    assert!(
        result.is_some(),
        "== must be found after multibyte UTF-8: {result:?}"
    );
    let (_, op) = result.unwrap();
    assert_eq!(op, "==");
}

#[test]
fn has_bare_equals_emoji_adjacent_to_operator() {
    // `🎉 == x` — emoji (4-byte UTF-8) before ==; byte scanning must not be confused
    let result = find_unquoted_operator("🎉 == x");
    assert!(
        result.is_some(),
        "== must be found after 4-byte emoji: {result:?}"
    );
    let (_, op) = result.unwrap();
    assert_eq!(op, "==");
}

// ─── strip_trailing_directive_colon ───────────────────────────────────────────

#[test]
fn strip_trailing_colon_basic() {
    assert_eq!(
        strip_trailing_directive_colon("cond:"),
        Some("cond"),
        "bare trailing colon must be stripped"
    );
}

#[test]
fn strip_trailing_colon_colon_inside_string_not_stripped() {
    // `"a:b":` — colon inside string literal must not be taken as the directive colon
    assert_eq!(
        strip_trailing_directive_colon(r#""a:b":"#),
        Some(r#""a:b""#),
        "colon inside string must not be stripped as directive colon"
    );
}

#[test]
fn strip_trailing_colon_colon_inside_parens_not_stripped() {
    // `func(a:b):` — colon inside parens (hypothetical) must not be taken as directive colon
    assert_eq!(
        strip_trailing_directive_colon("func(a:b):"),
        Some("func(a:b)"),
        "colon inside parens must not be stripped as directive colon"
    );
}

#[test]
fn strip_trailing_colon_single_quote_string() {
    // `'a:b':` — colon inside single-quoted string must be preserved
    assert_eq!(
        strip_trailing_directive_colon("'a:b':"),
        Some("'a:b'"),
        "colon inside single-quoted string must not be stripped"
    );
}

#[test]
fn strip_trailing_colon_escaped_quote_in_string() {
    // `"a\":b":` — escaped quote must not prematurely end the string
    assert_eq!(
        strip_trailing_directive_colon(r#""a\":b":"#),
        Some(r#""a\":b""#),
        "escaped quote must not end string prematurely"
    );
}

#[test]
fn strip_trailing_colon_multibyte_utf8_in_string() {
    // `"café: latte":` — multibyte UTF-8 inside string, colon inside string
    assert_eq!(
        strip_trailing_directive_colon("\"café: latte\":"),
        Some("\"café: latte\""),
        "multibyte UTF-8 inside string must not confuse scanner"
    );
}

#[test]
fn strip_trailing_colon_unbalanced_paren_returns_none() {
    // `func(a:` — unclosed paren → None (structurally malformed)
    assert_eq!(
        strip_trailing_directive_colon("func(a:"),
        None,
        "unclosed paren must return None"
    );
}

#[test]
fn strip_trailing_colon_no_colon_returns_none() {
    assert_eq!(
        strip_trailing_directive_colon("cond"),
        None,
        "missing trailing colon must return None"
    );
}

#[test]
fn strip_trailing_colon_nested_parens() {
    // `outer(inner()):` — nested parens, then directive colon
    assert_eq!(
        strip_trailing_directive_colon("outer(inner()):"),
        Some("outer(inner())"),
        "nested parens must be handled correctly"
    );
}

#[test]
fn strip_trailing_colon_paren_inside_string() {
    // `"a)b":` — closing paren inside string must not decrement paren depth
    assert_eq!(
        strip_trailing_directive_colon("\"a)b\":"),
        Some("\"a)b\""),
        "paren inside string must not affect paren depth"
    );
}

// ─── has_unterminated_string (NO paren tracking) ─────────────────────────────

#[test]
fn has_unterminated_string_mixed_quoting() {
    // `"it's` — unclosed double-quote with single quote inside: terminated=false
    assert!(
        has_unterminated_string("\"it's"),
        "unclosed double-quote with inner single quote must be unterminated"
    );
}

#[test]
fn has_unterminated_string_double_inside_single() {
    // `'say "hi"` — unclosed single-quote with double quotes inside: still unterminated
    assert!(
        has_unterminated_string("'say \"hi\""),
        "unclosed single-quote with inner double quotes must be unterminated"
    );
}

#[test]
fn has_unterminated_string_closed_double_quote() {
    assert!(
        !has_unterminated_string("\"hello\""),
        "closed double-quoted string must not be unterminated"
    );
}

#[test]
fn has_unterminated_string_escaped_close_quote() {
    // `"a\"` — the only closing candidate is escaped → still in-string
    assert!(
        has_unterminated_string(r#""a\""#),
        "escaped closing quote must still be unterminated"
    );
}

#[test]
fn has_unterminated_string_trailing_backslash() {
    // `"a\` — trailing backslash: the backslash consumes the next char (there is none),
    // so we consume backslash and skip EOF — the string remains open
    assert!(
        has_unterminated_string("\"a\\"),
        "string with trailing backslash must be unterminated"
    );
}

#[test]
fn has_unterminated_string_paren_inside_string_not_tracked() {
    // `"a)b"` — has_unterminated_string does NOT track parens; paren inside string is fine
    assert!(
        !has_unterminated_string("\"a)b\""),
        "paren inside closed string must not affect termination check"
    );
}

#[test]
fn has_unterminated_string_multibyte_utf8() {
    // `"café` — unclosed string with multibyte chars: still unterminated
    assert!(
        has_unterminated_string("\"café"),
        "unclosed string with multibyte UTF-8 must be unterminated"
    );
}

#[test]
fn has_unterminated_string_multibyte_before_closed_string() {
    // `"café"` — closed string with multibyte content: not unterminated
    assert!(
        !has_unterminated_string("\"café\""),
        "closed string with multibyte UTF-8 must not be unterminated"
    );
}

// ─── find_unquoted_operator (with paren tracking) ────────────────────────────

#[test]
fn find_unquoted_operator_ne_mixed_quoting() {
    // `a != "it's"` — single quote inside double-quoted RHS must not confuse scanner
    let result = find_unquoted_operator("a != \"it's\"");
    assert!(
        result.is_some(),
        "!= must be found with single quote inside double-quoted string"
    );
    let (_, op) = result.unwrap();
    assert_eq!(op, "!=");
}

#[test]
fn find_unquoted_operator_op_inside_string_not_found() {
    // `x == "a!=b"` — != inside string must not shadow outer ==
    let result = find_unquoted_operator(r#"x == "a!=b""#);
    assert!(
        result.is_some(),
        "outer == must be found when != is inside a string"
    );
    let (_, op) = result.unwrap();
    assert_eq!(op, "==");
}

#[test]
fn find_unquoted_operator_nested_parens() {
    // `contains(x, "==") != y` — == inside call args must be invisible; != outside must be found
    let result = find_unquoted_operator(r#"contains(x, "==") != y"#);
    assert!(result.is_some(), "!= outside parens must be found");
    let (_, op) = result.unwrap();
    assert_eq!(op, "!=");
}

#[test]
fn find_unquoted_operator_unbalanced_close_paren() {
    // `a == b)` — unbalanced ) saturates to 0; == is still found
    let result = find_unquoted_operator("a == b)");
    assert!(result.is_some());
    let (_, op) = result.unwrap();
    assert_eq!(op, "==");
}

#[test]
fn find_unquoted_operator_multibyte_in_string() {
    // `x == "日本語"` — multibyte UTF-8 inside string, == outside: must be found
    let result = find_unquoted_operator("x == \"日本語\"");
    assert!(
        result.is_some(),
        "== must be found with multibyte UTF-8 in string"
    );
    let (pos, op) = result.unwrap();
    assert_eq!(op, "==");
    assert_eq!(pos, 2, "== at byte 2");
}

#[test]
fn find_unquoted_operator_multibyte_before_operator() {
    // `日本語 == x` — multibyte UTF-8 before operator must not confuse byte scanner
    let result = find_unquoted_operator("日本語 == x");
    assert!(result.is_some(), "== must be found after multibyte UTF-8");
    let (_, op) = result.unwrap();
    assert_eq!(op, "==");
}

#[test]
fn find_unquoted_operator_emoji_before_operator() {
    // `🎉 != null` — 4-byte emoji before != must not confuse byte scanner
    let result = find_unquoted_operator("🎉 != null");
    assert!(result.is_some(), "!= must be found after 4-byte emoji");
    let (_, op) = result.unwrap();
    assert_eq!(op, "!=");
}

// ─── split_on_unquoted_op (with paren tracking) ──────────────────────────────

#[test]
fn split_on_unquoted_op_condition_with_op_inside_string() {
    // `a && "b&&c"` — && inside a double-quoted string must not split
    // We exercise this via parse_condition.
    let src = "@if a && b:\nok\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(result.is_ok(), "&& condition must parse: {result:?}");
}

#[test]
fn split_on_unquoted_op_op_inside_paren_not_split() {
    // `func(a && b)` — && inside parens must not split condition
    // parse_condition with single identifier (the outer truthy check): func(a&&b) is invalid
    // but the && inside parens is NOT a split point for split_on_unquoted_op.
    let src = "@if cond && other:\nok\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(result.is_ok(), "outer && in @if must parse: {result:?}");
}

#[test]
fn split_on_unquoted_op_multibyte_in_operand() {
    // `日本語 && ok` — multibyte UTF-8 in an operand must not confuse byte scanner
    // parse_condition rejects it (non-identifier), but the split must happen at the &&
    let src = "@if x && y:\nok\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_ok(),
        "multibyte operands exercise the split scanner: {result:?}"
    );
}

// ─── split_on_unquoted_op: adjacent / consecutive operators (regression #72) ──
//
// scan_bytes advances one byte at a time, so after a 2-byte operator match the
// closure must skip the operator's second byte. Three or more consecutive
// operator characters previously panicked with a reversed byte range. These
// tests pin the exact splits the pre-refactor hand-rolled loop produced.

#[test]
fn split_on_unquoted_op_simple_pair() {
    // Baseline: a single operator splits into two operands.
    assert_eq!(split_on_unquoted_op("a&&b", "&&"), vec!["a", "b"]);
    assert_eq!(split_on_unquoted_op("a||b", "||"), vec!["a", "b"]);
}

#[test]
fn split_on_unquoted_op_three_consecutive_amp() {
    // `a&&&b`: match `&&` at 1, resume at 3; the trailing `&` joins the next
    // segment → ["a", "&b"]. Must NOT panic.
    assert_eq!(split_on_unquoted_op("a&&&b", "&&"), vec!["a", "&b"]);
}

#[test]
fn split_on_unquoted_op_four_consecutive_amp() {
    // `a&&&&b`: two back-to-back operators → an empty middle operand.
    assert_eq!(split_on_unquoted_op("a&&&&b", "&&"), vec!["a", "", "b"]);
}

#[test]
fn split_on_unquoted_op_three_consecutive_pipe() {
    // `a|||b`: match `||` at 1, resume at 3; trailing `|` joins → ["a", "|b"].
    assert_eq!(split_on_unquoted_op("a|||b", "||"), vec!["a", "|b"]);
}

#[test]
fn split_on_unquoted_op_four_consecutive_pipe() {
    // `a||||b`: two back-to-back operators → empty middle operand.
    assert_eq!(split_on_unquoted_op("a||||b", "||"), vec!["a", "", "b"]);
}

#[test]
fn split_on_unquoted_op_all_operator_chars_amp() {
    // `&&&&` (4 chars): two operators at 0 and 2 → three empty segments.
    assert_eq!(split_on_unquoted_op("&&&&", "&&"), vec!["", "", ""]);
}

#[test]
fn split_on_unquoted_op_leading_operator() {
    // `&&a`: empty leading operand.
    assert_eq!(split_on_unquoted_op("&&a", "&&"), vec!["", "a"]);
}

#[test]
fn split_on_unquoted_op_trailing_operator() {
    // `a&&`: empty trailing operand.
    assert_eq!(split_on_unquoted_op("a&&", "&&"), vec!["a", ""]);
}

#[test]
fn split_on_unquoted_op_standalone_operator() {
    // `&&`: two empty operands.
    assert_eq!(split_on_unquoted_op("&&", "&&"), vec!["", ""]);
}

// ─── End-to-end: adjacent operators yield a graceful Err, never a panic ───────

#[test]
fn parse_condition_triple_amp_graceful_error_not_panic() {
    // `@if a &&& b:` reaches split_on_unquoted_op(s, "&&"). The old code panicked
    // on the reversed byte range; correct behaviour is a graceful empty-operand
    // syntax error (the `&b` operand alone is fine, but `a &&& b` parses as
    // ["a ", " & b"] → "& b" is not a valid simple condition). Either way: Err,
    // not panic.
    let result = parse_condition("a &&& b");
    assert!(
        result.is_err(),
        "adjacent && operators must return Err, not panic: {result:?}"
    );
}

#[test]
fn parse_condition_quad_pipe_graceful_error_not_panic() {
    // `@if x |||| y:` reaches split_on_unquoted_op(s, "||") and produces an empty
    // middle operand → graceful "empty operand in '||' expression" error.
    let result = parse_condition("x |||| y");
    assert!(
        result.is_err(),
        "adjacent || operators must return Err, not panic: {result:?}"
    );
}

#[test]
fn compile_template_triple_amp_if_graceful_error_not_panic() {
    // Full pipeline: a user template with `@if a &&& b:` must compile to a
    // graceful syntax error, NOT crash the compiler.
    let src = "@if a &&& b:\nok\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "test.mds", src);
    assert!(
        result.is_err(),
        "@if a &&& b: must be a graceful syntax error, not a panic: {result:?}"
    );
}

#[test]
fn compile_template_quad_pipe_if_graceful_error_not_panic() {
    // Full pipeline: a user template with `@if x |||| y:` must compile to a
    // graceful empty-operand syntax error, NOT crash the compiler.
    let src = "@if x |||| y:\nok\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "test.mds", src);
    assert!(
        result.is_err(),
        "@if x |||| y: must be a graceful syntax error, not a panic: {result:?}"
    );
}

// ─── parse_args_inner (char-level, WITH paren tracking) ──────────────────────

#[test]
fn parse_args_inner_mixed_single_double_quoting() {
    // func("it's") — single quote inside double-quoted arg must not split on comma
    let result = parse_args("\"it's\"");
    assert!(
        result.is_ok(),
        "single quote inside double-quoted arg must parse: {result:?}"
    );
    assert_eq!(result.unwrap().len(), 1);
}

#[test]
fn parse_args_inner_double_inside_single() {
    // func('say "hi"') — double quote inside single-quoted arg
    let result = parse_args("'say \"hi\"'");
    assert!(
        result.is_ok(),
        "double quote inside single-quoted arg must parse: {result:?}"
    );
    assert_eq!(result.unwrap().len(), 1);
}

#[test]
fn parse_args_inner_escaped_quote_in_string_arg() {
    // func("say \"hi\"") — escaped quote inside arg must not split
    let result = parse_args(r#""say \"hi\"""#);
    assert!(
        result.is_ok(),
        "escaped quote in arg must parse: {result:?}"
    );
    let args = result.unwrap();
    assert_eq!(args.len(), 1);
    if let Arg::StringLiteral(s) = &args[0] {
        assert_eq!(s, r#"say "hi""#);
    }
}

#[test]
fn parse_args_inner_nested_parens_not_split() {
    // func(inner(a, b), c) — comma inside nested call must not split outer args
    let result = parse_args("inner(a, b), c");
    assert!(
        result.is_ok(),
        "comma inside nested call must not be a separator: {result:?}"
    );
    assert_eq!(
        result.unwrap().len(),
        2,
        "must produce exactly 2 args: outer call + c"
    );
}

#[test]
fn parse_args_inner_empty_args() {
    // func() — empty args list
    let result = parse_args("");
    assert!(
        result.is_ok(),
        "empty args must parse to empty vec: {result:?}"
    );
    assert_eq!(result.unwrap().len(), 0);
}

#[test]
fn parse_args_inner_multibyte_utf8_string_arg() {
    // func("日本語") — multibyte UTF-8 in string arg must parse correctly
    let result = parse_args("\"日本語\"");
    assert!(
        result.is_ok(),
        "multibyte UTF-8 in string arg must parse: {result:?}"
    );
    let args = result.unwrap();
    assert_eq!(args.len(), 1);
    if let Arg::StringLiteral(s) = &args[0] {
        assert_eq!(s, "日本語");
    } else {
        panic!("expected StringLiteral, got {:?}", args[0]);
    }
}

#[test]
fn parse_args_inner_emoji_string_arg() {
    // func("🎉") — 4-byte emoji in string arg
    let result = parse_args("\"🎉\"");
    assert!(result.is_ok(), "emoji in string arg must parse: {result:?}");
    let args = result.unwrap();
    assert_eq!(args.len(), 1);
    if let Arg::StringLiteral(s) = &args[0] {
        assert_eq!(s, "🎉");
    } else {
        panic!("expected StringLiteral");
    }
}

// ─── split_on_unquoted_commas / find_unquoted_equals (NO paren tracking) ─────
//
// These scanners are exercised via parse_define_params which calls both.

#[test]
fn split_on_unquoted_commas_mixed_quoting() {
    // `param1, param2 = "it's"` — single quote inside double-quoted default must not split
    let result = parse_define_params("param1, param2 = \"it's\"", "f");
    assert!(
        result.is_ok(),
        "single quote inside double-quoted default: {result:?}"
    );
    let params = result.unwrap();
    assert_eq!(params.len(), 2);
    assert_eq!(params[0].name, "param1");
    assert_eq!(params[1].name, "param2");
    assert!(params[1].default.is_some());
}

#[test]
fn split_on_unquoted_commas_double_inside_single() {
    // `p = 'say "hi"'` — double quote inside single-quoted default must not confuse comma split
    let result = parse_define_params("p = 'say \"hi\"'", "f");
    assert!(
        result.is_ok(),
        "double quote inside single-quoted default: {result:?}"
    );
    let params = result.unwrap();
    assert_eq!(params.len(), 1);
    assert!(params[0].default.is_some());
}

#[test]
fn split_on_unquoted_commas_comma_inside_string_not_split() {
    // `p = "a,b"` — comma inside quoted default must not split the token
    let result = parse_define_params("p = \"a,b\"", "f");
    assert!(
        result.is_ok(),
        "comma inside quoted default must not split: {result:?}"
    );
    // The default value is a string "a,b", not split into two params
    let params = result.unwrap();
    assert_eq!(params.len(), 1, "must produce exactly 1 param, not 2");
}

#[test]
fn split_on_unquoted_commas_empty_consecutive() {
    // `a,,b` — consecutive commas → the empty token between them is skipped (empty=continue)
    let result = parse_define_params("a,,b", "f");
    // parse_define_params skips empty tokens, so `a,,b` → [a, b]
    assert!(
        result.is_ok(),
        "consecutive commas must be handled: {result:?}"
    );
    let params = result.unwrap();
    assert_eq!(params.len(), 2);
}

#[test]
fn find_unquoted_equals_no_paren_tracking() {
    // `p = "func(a=1)"` — = inside parens (inside string) must not be found;
    // the outer = (the default-value separator) must be the one found.
    // Note: find_unquoted_equals has NO paren tracking, but the = inside parens here
    // is also inside a string, so it's still protected by quote tracking.
    let result = parse_define_params("p = \"func(a=1)\"", "f");
    assert!(
        result.is_ok(),
        "= inside string inside parens must not be found early: {result:?}"
    );
    let params = result.unwrap();
    assert_eq!(params.len(), 1);
    assert!(params[0].default.is_some());
}

#[test]
fn find_unquoted_equals_multibyte_utf8_before_equals() {
    // `日 = "ok"` — multibyte UTF-8 before = ; but identifiers are ASCII-only so this errors
    // on the identifier check, not on the equals scan.
    let result = parse_define_params("日 = \"ok\"", "f");
    // The = is found correctly (byte scanner), but identifier validation rejects "日"
    assert!(result.is_err(), "non-ASCII param name must be rejected");
}

#[test]
fn find_unquoted_equals_multibyte_in_default_value() {
    // `p = "日本語"` — multibyte UTF-8 in default value string
    let result = parse_define_params("p = \"日本語\"", "f");
    assert!(
        result.is_ok(),
        "multibyte UTF-8 in default value must parse: {result:?}"
    );
    let params = result.unwrap();
    assert_eq!(params.len(), 1);
    if let Some(Expr::StringLiteral(s)) = &params[0].default {
        assert_eq!(s, "日本語");
    } else {
        panic!("expected Expr::StringLiteral with multibyte content");
    }
}

// ─── PR-E #79: parse_interpolation_expr vs parse_expr_inner dispatch ──────────
//
// These tests document the no-literals difference between interpolation parsing
// and directive expression parsing.  Both share the same call/dot/var dispatch
// but only parse_expr_inner accepts literal values.

#[test]
fn interpolation_expr_var_parsed_correctly() {
    // `{name}` — simple variable reference works in interpolation
    let src = "{name}";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_ok(),
        "simple var interpolation must parse: {result:?}"
    );
}

#[test]
fn interpolation_expr_member_access() {
    // `{user.name}` — member access works in interpolation
    let src = "{user.name}";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_ok(),
        "member access interpolation must parse: {result:?}"
    );
}

#[test]
fn interpolation_expr_call() {
    // `{upper(x)}` — function call works in interpolation
    let src = "{upper(x)}";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(result.is_ok(), "call interpolation must parse: {result:?}");
}

#[test]
fn interpolation_expr_qualified_call() {
    // `{str.upper(x)}` — qualified call works in interpolation
    let src = "{str.upper(x)}";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_ok(),
        "qualified call interpolation must parse: {result:?}"
    );
}

#[test]
fn interpolation_expr_literal_rejected() {
    // `{"hello"}` — string literal is NOT valid in interpolation (no-literals rule)
    let src = "{\"hello\"}";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_err(),
        "string literal must NOT be valid in interpolation: {result:?}"
    );
}

#[test]
fn directive_expr_literal_accepted() {
    // `@if "hello" == x:` — string literal IS valid in directive expression (parse_expr_inner)
    let src = "@if \"hello\" == x:\nok\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_ok(),
        "string literal must be valid in directive expression: {result:?}"
    );
}

#[test]
fn directive_expr_var_parsed_correctly() {
    // `@if user.admin:` — member access in directive condition (parse_expr_inner)
    let src = "@if user.admin:\nok\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_ok(),
        "member access in @if condition must parse: {result:?}"
    );
}

#[test]
fn directive_expr_qualified_call() {
    // `@if str.upper(x) == "Y":` — qualified call in directive expression
    let src = "@if str.upper(x) == \"Y\":\nok\n@end\n";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_ok(),
        "qualified call in @if condition must parse: {result:?}"
    );
}

#[test]
fn interpolation_multibyte_utf8_adjacent_to_brace() {
    // `{café}` — multibyte UTF-8 in interpolation identifier must be rejected
    // (identifiers are ASCII-only)
    let src = "{café}";
    let tokens = tokenize(src, "test.mds").unwrap();
    let result = parse_with_ctx(&tokens, "", "");
    assert!(
        result.is_err(),
        "multibyte identifier in interpolation must be rejected"
    );
}

// ── A3: Error-code mapping consolidation ─────────────────────────────────────

/// A3 — parser-layer error codes (E1 / E2 / E9):
///
/// | Error | Trigger | Expected code |
/// |-------|---------|---------------|
/// | E1 | @extends not first directive | mds::extends |
/// | E2 | two @extends declarations | mds::extends |
/// | E9 | @block nested inside @block | mds::syntax |
#[test]
fn a3_parser_error_code_table() {
    // E1: stray @extends → mds::extends
    {
        let src = "Some text.\n@extends \"./base.mds\"\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let err = parse_with_ctx(&tokens, "", "").unwrap_err();
        let s = err.serialize();
        assert_eq!(
            s.code, "mds::extends",
            "A3 E1: stray @extends must be mds::extends"
        );
        assert!(
            s.span.is_some(),
            "A3 E1: stray @extends error must carry a source span"
        );
    }

    // E2: double @extends → mds::extends (second one is stray)
    {
        let src = "@extends \"./a.mds\"\n@extends \"./b.mds\"\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let err = parse_with_ctx(&tokens, "", "").unwrap_err();
        let s = err.serialize();
        assert_eq!(
            s.code, "mds::extends",
            "A3 E2: double @extends must be mds::extends"
        );
        assert!(
            s.span.is_some(),
            "A3 E2: double @extends error must carry a source span"
        );
    }

    // E9: @block nested inside @block → mds::syntax (correct per spec; NOT mds::extends)
    {
        let src = "@block outer:\n@block inner:\nbody\n@end\n@end\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let err = parse_with_ctx(&tokens, "", "").unwrap_err();
        assert_eq!(
            err.serialize().code,
            "mds::syntax",
            "A3 E9: @block nesting must be mds::syntax"
        );
    }
}
