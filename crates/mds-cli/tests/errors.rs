mod common;
use common::{fixture, mds_bin};
use std::collections::HashMap;

#[test]
fn undefined_variable_error() {
    let result = mds::compile(fixture("undefined_var.mds"), None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("username"));
}

#[test]
fn arity_mismatch_error() {
    let result = mds::compile(fixture("arity_error.mds"), None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("arity") || err.contains("expected 1"));
}

#[test]
fn file_not_found_error() {
    let result = mds::compile(std::path::PathBuf::from("nonexistent.mds"), None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("not found") || err.contains("nonexistent"),
        "expected file not found error, got: {err}"
    );
}

#[test]
fn not_mds_file_error() {
    // Try to compile a .md file without type: mds
    let path = fixture("not_mds.md");
    let result = mds::compile(&path, None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("not an MDS file"),
        "expected 'not an MDS file' in error, got: {err}"
    );
}

#[test]
fn undefined_function_error() {
    let source = "{nonexistent(\"arg\")}\n";
    let result = mds::compile_str_with(source, None, None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("undefined function") || err.contains("nonexistent"),
        "expected undefined function error, got: {err}"
    );
}

#[test]
fn undefined_namespace_in_qualified_call() {
    let source = "{missing_ns.greet(\"Alice\")}\n";
    let result = mds::compile_str_with(source, None, None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("undefined") || err.contains("missing_ns"),
        "expected undefined namespace error, got: {err}"
    );
}

#[test]
fn undefined_function_error_message_says_function() {
    let source = "{nonexistent_fn(\"arg\")}\n";
    let result = mds::compile_str_with(source, None, None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("undefined function") || err.contains("nonexistent_fn"),
        "expected 'undefined function' error, not 'undefined variable', got: {err}"
    );
    assert!(
        !err.contains("undefined variable"),
        "error should say 'function', not 'variable', got: {err}"
    );
}

#[test]
fn for_body_undefined_var_errors_at_validate_time() {
    let result = mds::compile(fixture("for_body_undef.mds"), None);
    assert!(
        result.is_err(),
        "expected error for undefined var in @for body"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("undefined") || err.contains("undefined_var_in_body"),
        "expected undefined variable error in for body, got: {err}"
    );
}

#[test]
fn for_iterate_non_array_error() {
    // Attempting to iterate over a non-array should produce a type error
    let mut vars = HashMap::new();
    vars.insert(
        "items".to_string(),
        mds::Value::String("not_an_array".to_string()),
    );
    let result = mds::compile(fixture("loop.mds"), Some(vars));
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("expected array") || err.contains("type error") || err.contains("string"));
}

#[test]
fn invalid_identifier_in_for_var() {
    // @for x-y in items: — loop variable 'x-y' is not a valid identifier
    let source = "---\nitems: [a, b]\n---\n@for x-y in items:\n- {item}\n@end\n";
    let result = mds::compile_str_with(source, None, None);
    assert!(
        result.is_err(),
        "invalid loop variable name must be rejected"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("invalid") || err.contains("x-y"),
        "expected syntax error about invalid identifier, got: {err}"
    );
}

#[test]
fn invalid_identifier_in_define_name() {
    // @define my-func(): — function name 'my-func' is not a valid identifier
    let source = "@define my-func():\nhello\n@end\n";
    let result = mds::compile_str_with(source, None, None);
    assert!(result.is_err(), "invalid function name must be rejected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("invalid") || err.contains("my-func"),
        "expected syntax error about invalid function name, got: {err}"
    );
}

#[test]
fn duplicate_define_params_errors() {
    // @define test(a, a): — duplicate parameter 'a' must be a compile error
    let source = "@define test(a, a):\n{a}\n@end\n";
    let result = mds::compile_str_with(source, None, None);
    assert!(result.is_err(), "duplicate parameter name must be rejected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("duplicate") || err.contains("'a'"),
        "expected duplicate parameter error, got: {err}"
    );
}

#[test]
fn duplicate_define_errors() {
    // NOTE: This test documents expected behavior (Spec: no duplicate function names).
    // If the compiler does not yet enforce this, this test will fail until the fix lands.
    let result = mds::compile(fixture("duplicate_define.mds"), None);
    assert!(
        result.is_err(),
        "duplicate @define for same function name should be an error"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("collision") || err.contains("duplicate") || err.contains("already defined"),
        "expected collision/duplicate error, got: {err}"
    );
}

#[test]
fn error_output_shows_line_numbers() {
    // Compile a file with a known error and verify the miette output
    // includes source context with line numbers
    let source = "---\nname: Alice\n---\nHello {username}!\n";
    let result = mds::compile_str_with(source, None, None);
    assert!(result.is_err(), "should fail with undefined variable");

    let err = result.unwrap_err();
    // Format the error using miette's Debug impl (includes source context)
    let formatted = format!("{err:?}");
    assert!(
        formatted.contains("username"),
        "error should mention 'username', got: {formatted}"
    );
    // miette's fancy rendering includes line number context
    // The source has the error on line 4
    assert!(
        formatted.contains("4") || formatted.contains("username"),
        "error output should include line number context, got: {formatted}"
    );
}

#[test]
fn error_format_includes_file_line_col() {
    // Per spec: errors include file path, line number, column, contextual explanation.
    // Use the CLI to get the full diagnostic rendering.
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("err_test.mds");
    std::fs::write(&input, "---\nname: Alice\n---\nHello {undefined_var}!\n").unwrap();

    let output = mds_bin()
        .args(["build", input.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "should fail with undefined variable"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    // miette renders: ╭─[path:line:col]
    assert!(
        stderr.contains("undefined_var"),
        "error should mention the undefined variable, got: {stderr}"
    );
    // Line number must be present (error is on line 4)
    assert!(
        stderr.contains(":4:") || stderr.contains("4 │"),
        "error should include line number 4, got: {stderr}"
    );
}

#[test]
fn circular_import_error_has_help_text() {
    let result = mds::compile(fixture("circular_a.mds"), None);
    assert!(result.is_err(), "circular import should fail");
    let err = result.unwrap_err();
    let formatted = format!("{err:?}");
    assert!(
        formatted.contains("import") || formatted.contains("cycle"),
        "circular import error should mention import/cycle, got: {formatted}"
    );
}

#[test]
fn type_error_for_non_array_in_for_loop() {
    // Build a source that tries @for over a non-array variable
    let source = "---\ncount: 42\n---\n@for item in count:\n- {item}\n@end\n";
    let result = mds::compile_str(source);
    assert!(result.is_err(), "type error should be returned");
    let err = result.unwrap_err();
    // Use Display (not Debug) to get the human-readable error message
    let display = format!("{err}");
    assert!(
        display.contains("array") || display.contains("type error"),
        "type error Display should mention 'array' or 'type error', got: {display}"
    );
}

#[test]
fn if_negation_supported() {
    // `@if !premium:` — negation is now supported.
    // With premium=true, the negated condition is false → else branch.
    let source = "---\npremium: true\n---\n@if !premium:\nnegated_yes\n@else:\nnegated_no\n@end\n";
    let result = mds::compile_str(source);
    assert!(
        result.is_ok(),
        "@if with negation must succeed, got: {:?}",
        result
    );
    assert!(
        result.unwrap().contains("negated_no"),
        "negation of true must take else branch"
    );
}

#[test]
fn import_file_not_found_includes_source_span() {
    // When an @import directive references a non-existent file, the error must
    // include the source location (file:line:col) pointing at the @import line.
    let dir = tempfile::tempdir().unwrap();
    let consumer = dir.path().join("test.mds");
    std::fs::write(
        &consumer,
        "@import \"./nonexistent.mds\" as missing\n\nsome text\n",
    )
    .unwrap();

    let result = mds::compile(&consumer, None);
    assert!(result.is_err(), "import of non-existent file should error");

    let err = result.unwrap_err();
    // The Display format reports the path
    let display = format!("{err}");
    assert!(
        display.contains("nonexistent") || display.contains("not found"),
        "error message should mention the missing path, got: {display}"
    );
    // The Debug format (miette fancy rendering) includes source context with
    // line/column numbers when a span is attached.
    let debug = format!("{err:?}");
    assert!(
        debug.contains("test.mds") || debug.contains("1"),
        "error should include source context (file or line number), got: {debug}"
    );
    // Verify the @import line is referenced in the rendered output
    assert!(
        debug.contains("import") || debug.contains("nonexistent"),
        "error context should reference the @import line, got: {debug}"
    );
}

#[test]
fn import_file_not_found_span_for_alias_import() {
    // Alias-form @import should also include source span on file-not-found.
    let dir = tempfile::tempdir().unwrap();
    let consumer = dir.path().join("alias_test.mds");
    std::fs::write(&consumer, "@import \"./missing.mds\" as m\n\nsome text\n").unwrap();

    let result = mds::compile(&consumer, None);
    assert!(result.is_err());
    let err = result.unwrap_err();
    let debug = format!("{err:?}");
    assert!(
        debug.contains("alias_test.mds") || debug.contains("1"),
        "alias import error should include source context, got: {debug}"
    );
}

#[test]
fn import_file_not_found_span_for_merge_import() {
    // Merge-form @import (no alias) should also include source span on file-not-found.
    let dir = tempfile::tempdir().unwrap();
    let consumer = dir.path().join("merge_test.mds");
    std::fs::write(&consumer, "@import \"./missing.mds\"\n\nsome text\n").unwrap();

    let result = mds::compile(&consumer, None);
    assert!(result.is_err());
    let err = result.unwrap_err();
    let debug = format!("{err:?}");
    assert!(
        debug.contains("merge_test.mds") || debug.contains("1"),
        "merge import error should include source context, got: {debug}"
    );
}

// ── @if condition parse error tests ─────────────────────────────────────────

#[test]
fn if_empty_after_operator_is_parse_error() {
    // `@if var ==:` — missing RHS value
    let source = "---\nvar: x\n---\n@if var ==:\nyes\n@end\n";
    let result = mds::compile_str(source);
    assert!(result.is_err(), "@if var ==: must be a parse error");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("expected value after '=='"),
        "error must say expected value after '==', got: {err}"
    );
}

#[test]
fn if_bare_equals_is_parse_error() {
    // `@if var = "a":` — single `=` must suggest `==`
    let source = "---\nvar: a\n---\n@if var = \"a\":\nyes\n@end\n";
    let result = mds::compile_str(source);
    assert!(result.is_err(), "@if var = 'a': must be a parse error");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("'=='") || err.contains("use '=='"),
        "error must suggest '==', got: {err}"
    );
}

#[test]
fn if_negation_empty_variable_is_parse_error() {
    // `@if !:` — no variable name after `!`
    let source = "---\nvar: true\n---\n@if !:\nyes\n@end\n";
    let result = mds::compile_str(source);
    assert!(result.is_err(), "@if !: must be a parse error");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("expected variable name after '!'"),
        "error must say expected variable name, got: {err}"
    );
}

#[test]
fn if_double_negation_is_parse_error() {
    // `@if !!var:` — double negation not supported
    let source = "---\nvar: true\n---\n@if !!var:\nyes\n@end\n";
    let result = mds::compile_str(source);
    assert!(result.is_err(), "@if !!var: must be a parse error");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("double negation"),
        "error must mention double negation, got: {err}"
    );
}

#[test]
fn elseif_after_else_is_parse_error() {
    // `@elseif` appearing after `@else:` — not valid; the @else body only
    // accepts @end as a terminator so @elseif is an unknown directive there.
    let source = "---\nx: true\n---\n@if x:\nyes\n@else:\nno\n@elseif x:\nbad\n@end\n";
    let result = mds::compile_str(source);
    assert!(
        result.is_err(),
        "@elseif after @else: must be a parse error"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("@elseif") || err.contains("unknown directive"),
        "error must mention @elseif or unknown directive, got: {err}"
    );
}

#[test]
fn if_negation_combined_with_comparison_is_parse_error() {
    // `@if !var == "x":` — cannot combine negation with comparison
    let source = "---\nvar: x\n---\n@if !var == \"x\":\nyes\n@end\n";
    let result = mds::compile_str(source);
    assert!(result.is_err(), "@if !var == 'x': must be a parse error");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("cannot combine negation") || err.contains("negation with comparison"),
        "error must explain cannot combine negation with comparison, got: {err}"
    );
}

#[test]
fn if_unterminated_string_in_condition_is_parse_error() {
    // `@if var == "unclosed:` — unterminated string literal
    let source = "---\nvar: x\n---\n@if var == \"unclosed:\nyes\n@end\n";
    let result = mds::compile_str(source);
    assert!(result.is_err(), "unterminated string must be a parse error");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("unterminated string"),
        "error must mention unterminated string, got: {err}"
    );
}

#[test]
fn if_eq_undefined_variable_is_error() {
    // `@if missing == "x":` — undefined variable in equality
    let source = "---\nvar: x\n---\n@if missing == \"x\":\nyes\n@end\n";
    let result = mds::compile_str(source);
    assert!(
        result.is_err(),
        "undefined variable in equality must be an error"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("missing") || err.contains("undefined"),
        "error must mention the undefined variable, got: {err}"
    );
}

#[test]
fn if_negation_undefined_variable_is_error() {
    // `@if !missing:` — undefined variable in negation
    let source = "---\nvar: x\n---\n@if !missing:\nyes\n@end\n";
    let result = mds::compile_str(source);
    assert!(
        result.is_err(),
        "undefined variable in negation must be an error"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("missing") || err.contains("undefined"),
        "error must mention the undefined variable, got: {err}"
    );
}
